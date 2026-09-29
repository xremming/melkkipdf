//! Keeps to one running viewer, so documents opened while it runs become tabs
//! in its window rather than windows of their own.
//!
//! The first instance takes a lock and listens on a Unix socket beside it. A
//! later one finds the lock taken, sends its documents over the socket and
//! exits. The lock, rather than the socket, decides which instance is first,
//! because a socket file outlives an instance that crashed but its lock does
//! not.

use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

const LOCK_NAME: &str = "instance.lock";
const SOCKET_NAME: &str = "instance.sock";
/// How long a later instance keeps trying to reach the first before opening a
/// window of its own. The first binds its socket right after taking the lock,
/// so this only runs out when that instance is stuck.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_RETRY: Duration = Duration::from_millis(50);
/// How long the first instance waits for a later one to finish sending, so one
/// that stalls cannot hold up the ones after it.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// What this instance should do, now that it knows whether another runs.
pub enum Claim {
    /// No other instance runs, so this one opens the window and takes the
    /// documents later instances are asked to open.
    First(Listener),
    /// The documents went to the instance already running, which leaves
    /// nothing for this one to do.
    Forwarded,
    /// Whether another instance runs could not be told, or it could not be
    /// reached, so this one opens a window of its own as it did before.
    Alone,
}

/// The first instance's socket, and the lock that keeps it the first.
pub struct Listener {
    lock: File,
    socket: UnixListener,
}

impl Listener {
    /// Sends each later instance's documents to `requests` from a thread of
    /// its own, calling `wake` after each so the event loop takes them in.
    /// The lock goes with the thread, which lives as long as the app does.
    pub fn serve(self, requests: Sender<Vec<String>>, wake: impl Fn() + Send + 'static) {
        let Self { lock, socket } = self;
        thread::spawn(move || {
            let _lock = lock;
            for stream in socket.incoming() {
                let paths = match stream.and_then(read_request) {
                    Ok(paths) => paths,
                    Err(err) => {
                        eprintln!("Failed to take the documents another instance sent: {err}.");
                        continue;
                    }
                };
                if requests.send(paths).is_err() {
                    return;
                }
                wake();
            }
        });
    }
}

/// Becomes the first instance, or hands `paths` to the one already running.
pub fn claim(paths: &[String]) -> Claim {
    let Some(directory) = directory() else {
        return Claim::Alone;
    };
    // LaunchServices never starts a second copy of a running bundle, and it
    // hands a bundle its documents by Apple Event once running rather than on
    // the command line. A bundle that finds the lock taken was started for
    // documents it has yet to receive, by a copy run outside the bundle, and
    // forwarding now would lose them.
    let may_forward = !cfg!(target_os = "macos") || !launched_from_bundle();
    claim_in(&directory, paths, may_forward)
}

fn claim_in(directory: &Path, paths: &[String], may_forward: bool) -> Claim {
    match try_claim(directory, paths, may_forward) {
        Ok(claim) => claim,
        Err(err) => {
            eprintln!("Failed to reach another instance, so opening a window of its own: {err}.");
            Claim::Alone
        }
    }
}

fn try_claim(directory: &Path, paths: &[String], may_forward: bool) -> io::Result<Claim> {
    fs::create_dir_all(directory)?;
    // The lock file is never removed: an instance could otherwise lock a file
    // that has just been unlinked while another creates a fresh one.
    let lock =
        File::options().write(true).create(true).truncate(false).open(directory.join(LOCK_NAME))?;
    let socket = directory.join(SOCKET_NAME);
    match lock.try_lock() {
        Ok(()) => {
            // A socket left behind by an instance that crashed would stop the
            // bind, and holding the lock means no live instance is using it.
            match fs::remove_file(&socket) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
            let socket = UnixListener::bind(&socket)?;
            Ok(Claim::First(Listener { lock, socket }))
        }
        Err(TryLockError::WouldBlock) if may_forward => {
            forward(&socket, paths)?;
            Ok(Claim::Forwarded)
        }
        Err(TryLockError::WouldBlock) => Ok(Claim::Alone),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// Where the lock and socket live: the user's runtime directory on Linux, or
/// the part of it a flatpak shares between its instances when sandboxed, and
/// the per-user temporary directory on macOS, which has no runtime directory.
fn directory() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(std::env::temp_dir().join("melkkipdf"));
    }
    let runtime = dirs::runtime_dir()?;
    Some(match std::env::var_os("FLATPAK_ID") {
        Some(id) => runtime.join("app").join(id),
        None => runtime.join("melkkipdf"),
    })
}

fn launched_from_bundle() -> bool {
    std::env::current_exe()
        .is_ok_and(|exe| exe.parent().is_some_and(|dir| dir.ends_with("Contents/MacOS")))
}

/// Sends `paths` to the first instance. They are made absolute first, because
/// that instance resolves relative paths against its own working directory.
fn forward(socket: &Path, paths: &[String]) -> io::Result<()> {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(_) if started.elapsed() < CONNECT_TIMEOUT => thread::sleep(CONNECT_RETRY),
            Err(err) => return Err(err),
        }
    };
    let mut request = Vec::new();
    for path in paths {
        let path = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
        request.extend_from_slice(path.to_string_lossy().as_bytes());
        request.push(0);
    }
    stream.write_all(&request)
}

/// The paths a later instance sent, each ended by a NUL, which no path can
/// contain.
fn read_request(mut stream: UnixStream) -> io::Result<Vec<String>> {
    // macOS refuses the timeout on a socket whose peer has already closed,
    // as one that sends nothing does right away, but then reading cannot
    // block either.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut request = Vec::new();
    stream.read_to_end(&mut request)?;
    Ok(request
        .split(|&byte| byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Claim, SOCKET_NAME, claim_in};

    /// A fresh, empty directory for one test's lock and socket, removed again
    /// when the test is done with it.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let directory =
            std::env::temp_dir().join(format!("melkkipdf-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        Scratch(directory)
    }

    fn first(claim: Claim) -> super::Listener {
        match claim {
            Claim::First(listener) => listener,
            Claim::Forwarded => panic!("expected the first instance, but it forwarded"),
            Claim::Alone => panic!("expected the first instance, but it ran alone"),
        }
    }

    #[test]
    fn a_later_instance_hands_its_documents_to_the_first() {
        let directory = scratch("forward");
        let listener = first(claim_in(&directory.0, &[], true));
        let (requests, received) = mpsc::channel();
        listener.serve(requests, || {});

        let paths = ["/docs/a.pdf".to_owned(), "b.pdf".to_owned()];
        assert!(matches!(claim_in(&directory.0, &paths, true), Claim::Forwarded));

        let relative = std::env::current_dir().unwrap().join("b.pdf");
        let expected = vec!["/docs/a.pdf".to_owned(), relative.to_string_lossy().into_owned()];
        assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), expected);
    }

    #[test]
    fn a_later_instance_without_documents_still_reaches_the_first() {
        let directory = scratch("empty");
        let listener = first(claim_in(&directory.0, &[], true));
        let (requests, received) = mpsc::channel();
        listener.serve(requests, || {});

        assert!(matches!(claim_in(&directory.0, &[], true), Claim::Forwarded));
        assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_later_instance_that_may_not_forward_runs_alone() {
        let directory = scratch("alone");
        let _listener = first(claim_in(&directory.0, &[], true));
        assert!(matches!(claim_in(&directory.0, &[], false), Claim::Alone));
    }

    #[test]
    fn a_socket_left_by_a_crashed_instance_is_replaced() {
        let directory = scratch("stale");
        std::fs::write(directory.0.join(SOCKET_NAME), "").unwrap();
        first(claim_in(&directory.0, &[], true));
    }

    #[test]
    fn the_next_instance_is_first_once_the_first_has_gone() {
        let directory = scratch("gone");
        drop(first(claim_in(&directory.0, &[], true)));
        first(claim_in(&directory.0, &[], true));
    }
}
