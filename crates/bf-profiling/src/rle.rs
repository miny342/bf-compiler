//! Versioned, textual BF run-length encoding. Counts are total repetitions.
use std::{
    fmt,
    io::{self, Write},
};

pub const HEADER: &str = "@BFCRLE1;";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RleError(pub usize);
impl fmt::Display for RleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid BF RLE count/header at byte offset {}", self.0)
    }
}
impl std::error::Error for RleError {}

/// A run carries physical source positions, not expanded positions.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub byte: u8,
    pub count: usize,
    pub offset: usize,
    pub end: usize,
}

pub fn compressed(source: &[u8]) -> bool {
    source.starts_with(HEADER.as_bytes())
}

/// Decode one command at `offset`. Non-command bytes are left to the caller.
pub fn run_at(source: &[u8], offset: usize, rle: bool) -> Result<Run, RleError> {
    let byte = source[offset];
    let mut end = offset + 1;
    let mut count = 1;
    if rle
        && matches!(byte, b'+' | b'-' | b'<' | b'>')
        && source.get(end).is_some_and(u8::is_ascii_digit)
    {
        count = 0usize;
        while let Some(digit) = source.get(end).filter(|b| b.is_ascii_digit()) {
            count = count
                .checked_mul(10)
                .and_then(|n| n.checked_add((digit - b'0') as usize))
                .ok_or(RleError(offset))?;
            end += 1;
        }
        if count == 0 || count > isize::MAX as usize {
            return Err(RleError(offset));
        }
    }
    Ok(Run {
        byte,
        count,
        offset,
        end,
    })
}

pub struct Runs<'a> {
    source: &'a [u8],
    position: usize,
    rle: bool,
    total: usize,
}
impl<'a> Runs<'a> {
    pub fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            position: 0,
            rle: compressed(source),
            total: 0,
        }
    }
}
impl Iterator for Runs<'_> {
    type Item = Result<Run, RleError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.position == 0 && self.source.starts_with(b"@BFCRLE") && !self.rle {
            self.position = self.source.len();
            return Some(Err(RleError(0)));
        }
        while self.position < self.source.len() {
            let offset = self.position;
            if !super::is_bf_instruction(self.source[offset]) {
                self.position += 1;
                continue;
            }
            if let Ok(run) = run_at(self.source, offset, self.rle) {
                self.position = run.end;
                if let Some(total) = self
                    .total
                    .checked_add(run.count)
                    .filter(|n| *n <= isize::MAX as usize)
                {
                    self.total = total;
                    return Some(Ok(run));
                }
            }
            self.position = self.source.len();
            return Some(Err(RleError(offset)));
        }
        None
    }
}

pub fn write_run(output: &mut impl Write, byte: u8, count: usize) -> io::Result<()> {
    if count == 0 {
        return Ok(());
    }
    output.write_all(&[byte])?;
    if count > 1 {
        write!(output, "{count}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn count_syntax_and_limits() {
        for (source, expected) in [
            ("+163", vec![1]),
            ("@BFCRLE1;+163>23-<16", vec![163, 23, 1, 16]),
            ("@BFCRLE1;+ 163", vec![1]),
            ("@BFCRLE1;+001", vec![1]),
        ] {
            assert_eq!(
                Runs::new(source.as_bytes())
                    .map(|r| r.unwrap().count)
                    .collect::<Vec<_>>(),
                expected
            );
        }
        for source in [
            "@BFCRLE1;+0".into(),
            format!("@BFCRLE1;>{}+", isize::MAX),
            format!("@BFCRLE1;>{}", isize::MAX as u128 + 1),
        ] {
            assert!(
                Runs::new(source.as_bytes())
                    .collect::<Result<Vec<_>, _>>()
                    .is_err()
            );
        }
    }
}
