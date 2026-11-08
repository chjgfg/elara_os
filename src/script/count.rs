//! Shared incremental wc semantics for both buffered modules and streaming stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Bytes,
    Lines,
    Words,
}
pub(crate) fn options(args: &[String]) -> (Mode, &[String]) {
    match args.first().map(String::as_str) {
        Some("-c") => (Mode::Bytes, &args[1..]),
        Some("-l") => (Mode::Lines, &args[1..]),
        Some("-w") => (Mode::Words, &args[1..]),
        _ => (Mode::Bytes, args),
    }
}
pub(crate) struct Counter {
    mode: Mode,
    count: u64,
    inside: bool,
}
impl Counter {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            count: 0,
            inside: false,
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        let mut increment = 0u64;
        let mut inside = self.inside;
        match self.mode {
            Mode::Bytes => {
                increment = u64::try_from(bytes.len()).map_err(|_| "wc count overflow")?
            }
            Mode::Lines => increment = bytes.iter().filter(|b| **b == b'\n').count() as u64,
            Mode::Words => {
                for byte in bytes {
                    let word = !byte.is_ascii_whitespace();
                    increment += u64::from(word && !inside);
                    inside = word;
                }
            }
        }
        self.count = self
            .count
            .checked_add(increment)
            .ok_or("wc count overflow")?;
        self.inside = inside;
        Ok(())
    }
    pub fn value(&self) -> u64 {
        self.count
    }
}
