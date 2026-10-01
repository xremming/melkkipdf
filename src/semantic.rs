//! Search by meaning: a vector for every page, made by a static embedding
//! model, and a search that ranks the pages by how alike the query is to
//! them.
//!
//! Where text search folds the page and the query to match them letter for
//! letter, this hands both to the model as they are: the model has its own
//! idea of what a word is and was trained on text as it is written. The
//! model is large, so it is read from disk only once a reader searches by
//! meaning, on the thread that vectorizes the document, and readers who never
//! do pay nothing for it.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, RwLock};
use std::thread;

use model2vec_rs::model::StaticModel;
use slint::Weak;

use crate::MainWindow;

/// The variable naming the directory the model is in, for a build that
/// should look somewhere other than [`MODEL_DIR`].
const MODEL_DIR_VARIABLE: &str = "MELKKIPDF_MODEL_DIR";
/// Where the model is looked for otherwise, under the platform's data
/// directory.
const MODEL_DIR: &str = "melkkipdf/model";

/// How much of the page before and of the page after goes into a page's
/// chunk, so what is said across a page break is found on either page.
const NEIGHBOUR_SHARE: f32 = 0.25;
/// Pages scoring under this are not alike enough to list. Scores are cosines
/// of unit vectors: a page about the query scores 0.3 to 0.4, most pages of
/// a document 0.1 to 0.2, and no page comes near this for a query about
/// nothing in it.
pub const MIN_SCORE: f32 = 0.2;
/// Pages scoring under this share of the best page's score are not listed
/// either. A query the document is all about has every page scoring close
/// to the floor, and the list is for the pages that stand out.
const RELATED_SHARE: f32 = 0.6;
/// How many words each window of a page is that the passage it matched by
/// is looked for in, and how many words apart the windows start. Windows of
/// one length score alike, where a short one would score high or low by
/// chance, and overlapping ones catch a passage however it sits.
const WINDOW_WORDS: usize = 40;
const WINDOW_STRIDE: usize = 20;
/// How far above the page's middling window the best one has to score to be
/// the passage the page matched by. A page about the query throughout has no
/// such passage, and marking one would be a guess.
const PASSAGE_MARGIN: f32 = 0.1;

/// The embedding model, loaded the first time it is needed.
pub struct EmbeddingModel {
    source: Source,
    loaded: RwLock<Option<Loaded>>,
}

enum Source {
    /// A model2vec model's directory, with its `model.safetensors`,
    /// `tokenizer.json` and `config.json`.
    Directory(PathBuf),
    /// A stand-in that counts the words texts share, for tests.
    #[cfg(feature = "testing")]
    Words,
}

/// A loaded model. Cheap to clone, as the clones share the weights.
#[derive(Clone)]
pub enum Loaded {
    Static(StaticModel),
    #[cfg(feature = "testing")]
    Words,
}

impl EmbeddingModel {
    /// The model in the directory [`MODEL_DIR_VARIABLE`] names, or in
    /// [`MODEL_DIR`] under the platform's data directory. Nothing is read
    /// until [`Self::load`].
    pub fn locate() -> Self {
        let directory =
            std::env::var_os(MODEL_DIR_VARIABLE).map(PathBuf::from).unwrap_or_else(|| {
                dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(MODEL_DIR)
            });
        Self::in_directory(directory)
    }

    /// The model in `directory`, read once [`Self::load`] is called.
    pub fn in_directory(directory: PathBuf) -> Self {
        Self { source: Source::Directory(directory), loaded: RwLock::new(None) }
    }

    /// A model that scores texts by the words they share, so tests can search
    /// by meaning without a model on disk.
    #[cfg(feature = "testing")]
    pub fn words() -> Self {
        Self { source: Source::Words, loaded: RwLock::new(None) }
    }

    /// The model, read from disk first if it has not been, which takes a
    /// while and a good deal of memory.
    pub fn load(&self) -> Result<Loaded, String> {
        if let Some(loaded) = self.loaded() {
            return Ok(loaded);
        }
        let mut slot = self.loaded.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(loaded) = slot.as_ref() {
            return Ok(loaded.clone());
        }
        let loaded = match &self.source {
            Source::Directory(directory) => StaticModel::from_pretrained(
                directory, None, None, None,
            )
            .map(Loaded::Static)
            .map_err(|err| {
                format!(
                    "could not load the model from {}: {err:#} (set {MODEL_DIR_VARIABLE} to \
                         the directory of a model2vec model)",
                    directory.display()
                )
            })?,
            #[cfg(feature = "testing")]
            Source::Words => Loaded::Words,
        };
        *slot = Some(loaded.clone());
        Ok(loaded)
    }

    /// The model if it has been loaded, without waiting for anything.
    pub fn loaded(&self) -> Option<Loaded> {
        self.loaded.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

impl Loaded {
    /// A unit vector for each text, as it is, with nothing folded or
    /// trimmed. The texts have to be unit vectors for their dot product to be
    /// the cosine [`search`] scores by.
    pub fn embed(&self, texts: &[String]) -> Vectors {
        let rows = match self {
            // No length limit: a page runs to a few thousand characters, and
            // a chunk takes in parts of two more, well past the default.
            Self::Static(model) => model.encode_with_args(texts, None, 64),
            #[cfg(feature = "testing")]
            Self::Words => texts.iter().map(|text| words_vector(text)).collect(),
        };
        let mut vectors = Vectors::default();
        for row in rows {
            vectors.push(&row);
        }
        vectors
    }
}

/// How many dimensions the test model's vectors have.
#[cfg(feature = "testing")]
const WORDS_DIMS: usize = 64;

/// A unit vector of which words occur, each word on the dimension its hash
/// picks, so texts sharing words have alike vectors. Whether a word occurs
/// rather than how often, so a test page can repeat a filler line without
/// drowning the words that matter.
#[cfg(feature = "testing")]
fn words_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; WORDS_DIMS];
    for word in text.split_whitespace() {
        // FNV-1a, since the standard hasher is not promised to stay the same
        // between Rust versions, and a test should fail for one reason only.
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in word.to_lowercase().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        vector[(hash % WORDS_DIMS as u64) as usize] = 1.0;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

/// Unit vectors of the same length, one per page, in one run of floats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Vectors {
    /// How many floats each vector is. Zero until the first is pushed.
    dims: usize,
    data: Vec<f32>,
}

impl Vectors {
    /// How many vectors there are.
    pub fn len(&self) -> usize {
        self.data.len().checked_div(self.dims).unwrap_or(0)
    }

    /// The vector at `index`.
    pub fn get(&self, index: usize) -> &[f32] {
        &self.data[index * self.dims..(index + 1) * self.dims]
    }

    /// Adds `vector` after the others. Every vector has to be as long as the
    /// first, which a model promises.
    pub fn push(&mut self, vector: &[f32]) {
        if self.dims == 0 {
            self.dims = vector.len();
        }
        assert_eq!(vector.len(), self.dims, "a vector of another length");
        self.data.extend_from_slice(vector);
    }

    /// Adds every vector of `other` after the others.
    pub fn append(&mut self, other: &Vectors) {
        for index in 0..other.len() {
            self.push(other.get(index));
        }
    }
}

/// The text the model sees for a page: the last [`NEIGHBOUR_SHARE`] of the
/// page before it, its own text, and the first share of the page after it.
/// The parts are cut between words, and a page's text goes in as it is.
pub fn chunk(previous: Option<&str>, page: &str, next: Option<&str>) -> String {
    let mut chunk = String::new();
    for part in [previous.map(tail), Some(page), next.map(head)].into_iter().flatten() {
        if !chunk.is_empty() && !part.is_empty() {
            chunk.push(' ');
        }
        chunk.push_str(part);
    }
    chunk
}

/// The last [`NEIGHBOUR_SHARE`] of `text`, from the start of a word, or all
/// of it when it is short.
fn tail(text: &str) -> &str {
    let count = text.chars().count();
    let keep = (count as f32 * NEIGHBOUR_SHARE).ceil() as usize;
    if keep >= count {
        return text;
    }
    let from = text.char_indices().nth(count - keep).map_or(text.len(), |(index, _)| index);
    match text[from..].find(' ') {
        Some(space) => &text[from + space + 1..],
        // No word ends after the cut, so the cut is as good a place as any,
        // which for a script written without spaces it is.
        None => &text[from..],
    }
}

/// The first [`NEIGHBOUR_SHARE`] of `text`, to the end of a word, or all of
/// it when it is short.
fn head(text: &str) -> &str {
    let count = text.chars().count();
    let keep = (count as f32 * NEIGHBOUR_SHARE).ceil() as usize;
    if keep >= count {
        return text;
    }
    let to = text.char_indices().nth(keep).map_or(text.len(), |(index, _)| index);
    match text[..to].rfind(' ') {
        Some(space) => &text[..space],
        None => &text[..to],
    }
}

/// The pages most like `query`, best first, as `(page, score)`: at most
/// `limit` of them, none scoring under [`MIN_SCORE`], and none under
/// [`RELATED_SHARE`] of the best score.
pub fn search(vectors: &Vectors, query: &[f32], limit: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> =
        (0..vectors.len()).map(|page| (page, dot(vectors.get(page), query))).collect();
    // Best first, and the earlier page first of two scoring the same.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let floor = scored.first().map_or(MIN_SCORE, |&(_, best)| MIN_SCORE.max(best * RELATED_SHARE));
    scored.retain(|&(_, score)| score >= floor);
    scored.truncate(limit);
    scored
}

/// The cosine of two unit vectors.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

/// The byte ranges of `text`'s windows: [`WINDOW_WORDS`] words each,
/// starting [`WINDOW_STRIDE`] words apart, with the last one ending at the
/// last word. Text of fewer words than a window is one window.
pub fn windows(text: &str) -> Vec<(usize, usize)> {
    let words: Vec<(usize, usize)> = text
        .split_whitespace()
        .map(|word| {
            let start = word.as_ptr() as usize - text.as_ptr() as usize;
            (start, start + word.len())
        })
        .collect();
    let Some(&(_, last)) = words.last() else {
        return Vec::new();
    };
    if words.len() <= WINDOW_WORDS {
        return vec![(words[0].0, last)];
    }
    let mut windows: Vec<(usize, usize)> = (0..=words.len() - WINDOW_WORDS)
        .step_by(WINDOW_STRIDE)
        .map(|first| (words[first].0, words[first + WINDOW_WORDS - 1].1))
        .collect();
    if windows.last().is_some_and(|&(_, end)| end < last) {
        windows.push((words[words.len() - WINDOW_WORDS].0, last));
    }
    windows
}

/// The passage of `text` most like `query`, as a byte range, when one stands
/// out from the rest of the page (see [`PASSAGE_MARGIN`]). A text of one
/// window has nothing to stand out from.
pub fn best_passage(model: &Loaded, text: &str, query: &[f32]) -> Option<(usize, usize)> {
    let windows = windows(text);
    if windows.len() < 2 {
        return None;
    }
    let texts: Vec<String> =
        windows.iter().map(|&(start, end)| text[start..end].to_string()).collect();
    let vectors = model.embed(&texts);
    let scores: Vec<f32> = (0..windows.len()).map(|index| dot(vectors.get(index), query)).collect();
    let best =
        (0..scores.len()).max_by(|&a, &b| scores[a].total_cmp(&scores[b]).then(b.cmp(&a)))?;
    let mut sorted = scores.clone();
    sorted.sort_by(f32::total_cmp);
    let middling = sorted[sorted.len() / 2];
    (scores[best] - middling >= PASSAGE_MARGIN).then_some(windows[best])
}

/// What a vectorizer made: the vectors of the pages from `first_page` on, or
/// why it could not make any.
pub struct Vectorized {
    pub doc: i32,
    pub first_page: usize,
    pub result: Result<Vectors, String>,
}

/// Makes vectors for one document's pages, on a thread per batch, and sends
/// them to the app tagged with the document's id.
pub struct Vectorizer {
    doc: i32,
    model: Arc<EmbeddingModel>,
    sender: Sender<Vectorized>,
    window: Weak<MainWindow>,
}

impl Vectorizer {
    pub fn new(
        doc: i32,
        model: Arc<EmbeddingModel>,
        sender: Sender<Vectorized>,
        window: Weak<MainWindow>,
    ) -> Self {
        Self { doc, model, sender, window }
    }

    /// The model the vectors come from.
    pub fn model(&self) -> &EmbeddingModel {
        &self.model
    }

    /// Makes a vector for each of `chunks`, the chunks of the pages from
    /// `first_page` on, and calls the window's `pages-vectorized` callback
    /// once they are sent. Loads the model first if nothing has.
    pub fn spawn(&self, first_page: usize, chunks: Vec<String>) {
        let doc = self.doc;
        let model = self.model.clone();
        let sender = self.sender.clone();
        let window = self.window.clone();
        thread::spawn(move || {
            let result = model.load().map(|loaded| loaded.embed(&chunks));
            if sender.send(Vectorized { doc, first_page, result }).is_ok() {
                let _ = window.upgrade_in_event_loop(|window| window.invoke_pages_vectorized());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Vectors, chunk, head, search, tail, windows};

    #[test]
    fn a_chunk_takes_a_quarter_of_each_neighbour() {
        let previous = "one two three four five six seven eight";
        let next = "alpha beta gamma delta epsilon zeta eta theta";
        assert_eq!(chunk(Some(previous), "the page", Some(next)), "eight the page alpha beta");
        assert_eq!(chunk(None, "the page", None), "the page");
        assert_eq!(chunk(Some(""), "the page", Some("")), "the page");
        assert_eq!(chunk(Some(previous), "", Some(next)), "eight alpha beta");
    }

    #[test]
    fn neighbour_parts_are_cut_between_words() {
        assert_eq!(tail("aaaa bbbb cccc dddd"), "dddd");
        assert_eq!(head("aaaa bbbb cccc dddd"), "aaaa");
        // The cut lands inside "bbbbbb", which goes with the part it starts in.
        assert_eq!(tail("aaaaaaaaaaaa bbbbbb cc"), "cc");
        assert_eq!(head("aa bbbbbb cccccccccccc"), "aa");
    }

    #[test]
    fn a_neighbour_too_short_to_cut_goes_in_whole() {
        assert_eq!(tail("a"), "a");
        assert_eq!(head("a"), "a");
        assert_eq!(tail(""), "");
        assert_eq!(head(""), "");
    }

    #[test]
    fn text_without_spaces_is_cut_by_length() {
        assert_eq!(tail("abcdefgh"), "gh");
        assert_eq!(head("abcdefgh"), "ab");
        assert_eq!(head("日本語の文章です"), "日本");
    }

    #[test]
    fn windows_overlap_and_reach_the_end() {
        let words: Vec<String> = (0..70).map(|index| format!("w{index}")).collect();
        let text = words.join(" ");
        let found = windows(&text);
        let as_words: Vec<(usize, usize)> = found
            .iter()
            .map(|&(start, end)| {
                let before = text[..start].split_whitespace().count();
                (before, before + text[start..end].split_whitespace().count())
            })
            .collect();
        assert_eq!(as_words, [(0, 40), (20, 60), (30, 70)]);
        assert!(
            found.iter().all(|&(start, end)| !text[start..end].starts_with(' ')
                && !text[start..end].ends_with(' '))
        );
    }

    #[test]
    fn short_text_is_one_window_and_none_is_none() {
        assert_eq!(windows("  a few words  "), [(2, 13)]);
        assert_eq!(windows("   "), []);
        assert_eq!(windows(""), []);
    }

    #[cfg(feature = "testing")]
    #[test]
    fn the_passage_is_the_window_that_stands_out() {
        use super::best_passage;
        let model = super::EmbeddingModel::words().load().unwrap();
        let mut words: Vec<&str> = vec!["filler"; 125];
        words[50] = "orient";
        words[51] = "express";
        let text = words.join(" ");
        let query = model.embed(&["orient express".to_string()]);
        let (start, end) = best_passage(&model, &text, query.get(0)).expect("no passage");
        assert!(text[start..end].contains("orient express"));
        // The best window is the first of those holding the words.
        assert_eq!(text[..start].split_whitespace().count(), 20);

        // Every window alike, no passage; too short for two windows, none.
        let query = model.embed(&["filler".to_string()]);
        assert_eq!(best_passage(&model, &text, query.get(0)), None);
        assert_eq!(best_passage(&model, "orient express", query.get(0)), None);
    }

    fn vectors(rows: &[&[f32]]) -> Vectors {
        let mut vectors = Vectors::default();
        for row in rows {
            vectors.push(row);
        }
        vectors
    }

    #[test]
    fn pages_come_best_first_and_unalike_ones_not_at_all() {
        let pages = vectors(&[&[1.0, 0.0], &[0.0, 1.0], &[0.6, 0.8], &[0.8, 0.6]]);
        let found = search(&pages, &[1.0, 0.0], 10);
        assert_eq!(found, [(0, 1.0), (3, 0.8), (2, 0.6)]);
        assert_eq!(search(&pages, &[1.0, 0.0], 2), [(0, 1.0), (3, 0.8)]);
    }

    #[test]
    fn pages_far_below_the_best_are_not_listed() {
        let pages = vectors(&[&[0.8, 0.6], &[0.4, 0.917], &[0.5, 0.866]]);
        // The first scores 0.8, so a page has to score 0.48 to be listed
        // beside it, which the last just does and the middle one does not.
        let found = search(&pages, &[1.0, 0.0], 10);
        assert_eq!(found.iter().map(|&(page, _)| page).collect::<Vec<_>>(), [0, 2]);
    }

    #[test]
    fn pages_scoring_the_same_come_in_order() {
        let pages = vectors(&[&[1.0, 0.0], &[1.0, 0.0]]);
        assert_eq!(search(&pages, &[1.0, 0.0], 10), [(0, 1.0), (1, 1.0)]);
    }

    #[test]
    fn vectors_append_and_count() {
        let mut all = vectors(&[&[1.0, 0.0]]);
        all.append(&vectors(&[&[0.0, 1.0], &[1.0, 1.0]]));
        assert_eq!(all.len(), 3);
        assert_eq!(all.get(2), [1.0, 1.0]);
        assert_eq!(Vectors::default().len(), 0);
    }

    #[cfg(feature = "testing")]
    #[test]
    fn the_test_model_scores_shared_words() {
        let model = super::EmbeddingModel::words();
        let loaded = model.load().unwrap();
        let texts = ["the orient express".to_string(), "nothing to see here".to_string()];
        let pages = loaded.embed(&texts);
        let query = loaded.embed(&["Orient Express".to_string()]);
        let found = search(&pages, query.get(0), 10);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 0);
    }
}
