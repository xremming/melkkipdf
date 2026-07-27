use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    melkkipdf::run(std::env::args().nth(1))
}
