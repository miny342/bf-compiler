//! Interpreter for the Brainfuck dialect specified in the workspace's
//! `SPEC.md`.

use std::error::Error as StdError;
use std::fmt;

/// Number of cells in the standard tape.
pub const TAPE_LEN: usize = 30_000;

/// Execution measurements useful for comparing generated Brainfuck programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunStats {
    /// Number of parsed Brainfuck instructions executed, including jumps.
    pub executed_instructions: u64,
    /// Largest tape index reached by the data pointer.
    pub max_pointer: usize,
}

/// Binary output and measurements from one interpreter run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub output: Vec<u8>,
    pub stats: RunStats,
}

/// An error encountered while parsing or running a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnmatchedOpeningBracket { offset: usize },
    UnmatchedClosingBracket { offset: usize },
    TapeUnderflow { instruction_offset: usize },
    TapeOverflow { instruction_offset: usize },
    InvalidUtf8Output,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmatchedOpeningBracket { offset } => {
                write!(f, "unmatched '[' at byte offset {offset}")
            }
            Self::UnmatchedClosingBracket { offset } => {
                write!(f, "unmatched ']' at byte offset {offset}")
            }
            Self::TapeUnderflow { instruction_offset } => write!(
                f,
                "data pointer moved left of the tape at byte offset {instruction_offset}"
            ),
            Self::TapeOverflow { instruction_offset } => write!(
                f,
                "data pointer moved right of the tape at byte offset {instruction_offset}"
            ),
            Self::InvalidUtf8Output => write!(f, "program output is not valid UTF-8"),
        }
    }
}

impl StdError for Error {}

#[derive(Debug, Clone, Copy)]
enum Op {
    Right,
    Left,
    Increment,
    Decrement,
    Output,
    Input,
    JumpIfZero(usize),
    JumpIfNonZero(usize),
}

#[derive(Debug, Clone, Copy)]
struct Instruction {
    op: Op,
    source_offset: usize,
}

/// Runs a Brainfuck program with byte-oriented input and output.
///
/// Non-instruction bytes in `source` are ignored. See the workspace's
/// `SPEC.md` for the exact dialect.
pub fn run(source: &[u8], input: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(run_with_stats(source, input)?.output)
}

/// Runs a Brainfuck program and returns its output and execution measurements.
pub fn run_with_stats(source: &[u8], input: &[u8]) -> Result<RunResult, Error> {
    let instructions = parse(source)?;
    execute(&instructions, input)
}

/// String convenience wrapper around [`run`].
///
/// This returns [`Error::InvalidUtf8Output`] if the program emits bytes that
/// are not valid UTF-8. Use [`run`] when arbitrary binary output is expected.
pub fn run_str(source: &str, input: &str) -> Result<String, Error> {
    let output = run(source.as_bytes(), input.as_bytes())?;
    String::from_utf8(output).map_err(|_| Error::InvalidUtf8Output)
}

fn parse(source: &[u8]) -> Result<Vec<Instruction>, Error> {
    let mut instructions: Vec<Instruction> = Vec::new();
    let mut openings = Vec::new();

    for (source_offset, byte) in source.iter().copied().enumerate() {
        let op = match byte {
            b'>' => Op::Right,
            b'<' => Op::Left,
            b'+' => Op::Increment,
            b'-' => Op::Decrement,
            b'.' => Op::Output,
            b',' => Op::Input,
            b'[' => {
                openings.push(instructions.len());
                Op::JumpIfZero(usize::MAX)
            }
            b']' => {
                let Some(opening_index) = openings.pop() else {
                    return Err(Error::UnmatchedClosingBracket {
                        offset: source_offset,
                    });
                };
                let closing_index = instructions.len();
                instructions[opening_index].op = Op::JumpIfZero(closing_index + 1);
                Op::JumpIfNonZero(opening_index + 1)
            }
            _ => continue,
        };
        instructions.push(Instruction { op, source_offset });
    }

    if let Some(opening_index) = openings.first().copied() {
        return Err(Error::UnmatchedOpeningBracket {
            offset: instructions[opening_index].source_offset,
        });
    }

    Ok(instructions)
}

fn execute(instructions: &[Instruction], input: &[u8]) -> Result<RunResult, Error> {
    let mut tape = vec![0_u8; TAPE_LEN];
    let mut pointer = 0_usize;
    let mut max_pointer = 0_usize;
    let mut program_counter = 0_usize;
    let mut input_position = 0_usize;
    let mut output = Vec::new();
    let mut executed_instructions = 0_u64;

    while let Some(instruction) = instructions.get(program_counter) {
        executed_instructions += 1;
        match instruction.op {
            Op::Right => {
                if pointer + 1 == TAPE_LEN {
                    return Err(Error::TapeOverflow {
                        instruction_offset: instruction.source_offset,
                    });
                }
                pointer += 1;
                max_pointer = max_pointer.max(pointer);
                program_counter += 1;
            }
            Op::Left => {
                if pointer == 0 {
                    return Err(Error::TapeUnderflow {
                        instruction_offset: instruction.source_offset,
                    });
                }
                pointer -= 1;
                program_counter += 1;
            }
            Op::Increment => {
                tape[pointer] = tape[pointer].wrapping_add(1);
                program_counter += 1;
            }
            Op::Decrement => {
                tape[pointer] = tape[pointer].wrapping_sub(1);
                program_counter += 1;
            }
            Op::Output => {
                output.push(tape[pointer]);
                program_counter += 1;
            }
            Op::Input => {
                tape[pointer] = input.get(input_position).copied().unwrap_or(0);
                input_position = input_position.saturating_add(1);
                program_counter += 1;
            }
            Op::JumpIfZero(target) => {
                program_counter = if tape[pointer] == 0 {
                    target
                } else {
                    program_counter + 1
                };
            }
            Op::JumpIfNonZero(target) => {
                program_counter = if tape[pointer] != 0 {
                    target
                } else {
                    program_counter + 1
                };
            }
        }
    }

    Ok(RunResult {
        output,
        stats: RunStats {
            executed_instructions,
            max_pointer,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comments_and_runs_a_loop() {
        assert_eq!(run_str("+++ add three [>++<-] >.", ""), Ok("\u{6}".into()));
    }

    #[test]
    fn increment_and_decrement_wrap() {
        assert_eq!(run(b"-.", b""), Ok(vec![255]));
        assert_eq!(run(b"-.+.", b""), Ok(vec![255, 0]));
    }

    #[test]
    fn input_and_output_are_binary() {
        assert_eq!(run(b",.,.,.", &[0, 128, 255]), Ok(vec![0, 128, 255]));
    }

    #[test]
    fn reports_execution_measurements() {
        let result = run_with_stats(b"++[>++<-]>.", b"").unwrap();
        assert_eq!(result.output, vec![4]);
        assert_eq!(result.stats.executed_instructions, 17);
        assert_eq!(result.stats.max_pointer, 1);
    }

    #[test]
    fn eof_sets_the_current_cell_to_zero() {
        assert_eq!(run(b"+,.", b""), Ok(vec![0]));
    }

    #[test]
    fn zero_skips_nested_loop() {
        assert_eq!(run(b"[+[.-]].", b""), Ok(vec![0]));
    }

    #[test]
    fn reports_unmatched_brackets_using_source_offsets() {
        assert_eq!(
            run(b"comment [ +", b""),
            Err(Error::UnmatchedOpeningBracket { offset: 8 })
        );
        assert_eq!(
            run(b"abc]", b""),
            Err(Error::UnmatchedClosingBracket { offset: 3 })
        );
    }

    #[test]
    fn checks_both_tape_boundaries() {
        assert_eq!(
            run(b"comment <", b""),
            Err(Error::TapeUnderflow {
                instruction_offset: 8
            })
        );

        let source = vec![b'>'; TAPE_LEN];
        assert_eq!(
            run(&source, b""),
            Err(Error::TapeOverflow {
                instruction_offset: TAPE_LEN - 1
            })
        );
    }

    #[test]
    fn string_wrapper_rejects_non_utf8_output() {
        assert_eq!(run_str("-.", ""), Err(Error::InvalidUtf8Output));
    }
}
