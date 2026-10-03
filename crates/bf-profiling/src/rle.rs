//! Versioned, textual BF run-length encoding. Counts are total repetitions.
use std::{
    fmt,
    io::{self, Write},
};

pub const HEADER: &str = "@BFCRLE1;";
/// BFCRLE v2 uses hexadecimal counts after `+`, `-`, `<`, and `>`.
pub const HEX_HEADER: &str = "@BFCRLE2;";

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
    source.starts_with(HEADER.as_bytes()) || source.starts_with(HEX_HEADER.as_bytes())
}

pub fn hex_compressed(source: &[u8]) -> bool {
    source.starts_with(HEX_HEADER.as_bytes())
}

fn count_digit(byte: u8, hexadecimal: bool) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' if hexadecimal => Some(byte - b'a' + 10),
        b'A'..=b'F' if hexadecimal => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode one command at `offset`. Non-command bytes are left to the caller.
#[inline]
pub fn run_at(source: &[u8], offset: usize, rle: bool) -> Result<Run, RleError> {
    let byte = source[offset];
    let mut end = offset + 1;
    let mut count = 1;
    let hexadecimal = rle && hex_compressed(source);
    if rle
        && matches!(byte, b'+' | b'-' | b'<' | b'>')
        && source
            .get(end)
            .and_then(|byte| count_digit(*byte, hexadecimal))
            .is_some()
    {
        count = 0usize;
        while let Some(digit) = source
            .get(end)
            .and_then(|byte| count_digit(*byte, hexadecimal))
        {
            count = count
                .checked_mul(if hexadecimal { 16 } else { 10 })
                .and_then(|n| n.checked_add(usize::from(digit)))
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
    coalesce_plain: bool,
}
impl<'a> Runs<'a> {
    /// Coalesce physically adjacent plain `+`, `-`, `<`, and `>` commands.
    /// Encoded runs and other commands retain their usual source positions.
    pub fn new_coalesced(source: &'a [u8]) -> Self {
        Self {
            coalesce_plain: true,
            ..Self::new(source)
        }
    }

    pub fn new(source: &'a [u8]) -> Self {
        Self {
            source,
            position: 0,
            rle: compressed(source),
            total: 0,
            coalesce_plain: false,
        }
    }
}
impl Iterator for Runs<'_> {
    type Item = Result<Run, RleError>;
    #[inline]
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
            if let Ok(mut run) = run_at(self.source, offset, self.rle) {
                if self.coalesce_plain && !self.rle && matches!(run.byte, b'+' | b'-' | b'<' | b'>')
                {
                    let mut end = run.end;
                    while self.source.get(end) == Some(&run.byte) {
                        end += 1;
                    }
                    run.count = end - run.offset;
                    run.end = end;
                }
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
        for (source, expected) in [
            ("@BFCRLE2;+a>10-<f", vec![10, 16, 1, 15]),
            ("@BFCRLE2;+A>10-<F", vec![10, 16, 1, 15]),
            ("@BFCRLE2;+000f", vec![15]),
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
            "@BFCRLE2;+0".into(),
            "@BFCRLE2;+000".into(),
        ] {
            assert!(
                Runs::new(source.as_bytes())
                    .collect::<Result<Vec<_>, _>>()
                    .is_err()
            );
        }
    }
    #[test]
    fn coalesced_plain_runs_preserve_every_physical_command_offset() {
        for byte in 0_u8..=255 {
            for count in [0, 1, 2, 15, 16, 17, 127, 1024] {
                let mut source = vec![b' '];
                source.extend(std::iter::repeat_n(byte, count));
                source.push(b' ');
                let expected = Runs::new(&source)
                    .map(|r| {
                        let r = r.unwrap();
                        (r.byte, r.offset, r.end)
                    })
                    .collect::<Vec<_>>();
                let mut actual = Vec::new();
                for run in Runs::new_coalesced(&source) {
                    let run = run.unwrap();
                    assert_eq!(run.end - run.offset, run.count);
                    for offset in run.offset..run.end {
                        actual.push((run.byte, offset, offset + 1));
                    }
                    if !matches!(byte, b'+' | b'-' | b'<' | b'>') {
                        assert_eq!(run.count, 1);
                    }
                }
                assert_eq!(actual, expected, "byte={byte} count={count}");
            }
        }
        for source in [b"++++xyz+++>9>>>--[[]].,,".as_slice(), b"< <xyz<<<<"] {
            let runs = Runs::new_coalesced(source)
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            for run in runs {
                assert!(
                    source[run.offset..run.end]
                        .iter()
                        .all(|byte| *byte == run.byte)
                );
            }
        }
    }

    #[test]
    fn coalescing_preserves_encoded_runs_and_invalid_header_offsets() {
        for source in [
            b"@BFCRLE1;+++>123xyz>7-255".as_slice(),
            b"@BFCRLE2;+++>aF-ff<3",
            b"@BFCRLE1;+0",
            b"@BFCRLE2;>ffffffffffffffffffffffff",
            b"@BFCRLE3;++",
        ] {
            let collect = |runs: Runs<'_>| {
                runs.map(|r| r.map(|r| (r.byte, r.count, r.offset, r.end)))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                collect(Runs::new_coalesced(source)),
                collect(Runs::new(source))
            );
        }
    }
}
