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
    /// Estimated instructions executed by a target that groups adjacent,
    /// identical `+`, `-`, `<`, and `>` instructions into one operation.
    pub executed_rle_instructions: u64,
    /// Largest tape index reached by the data pointer.
    pub max_pointer: usize,
    /// Work handled by the optimized interpreter rather than the raw BF VM.
    pub optimization: OptimizationRunStats,
}

/// Dynamic counters for the native operations selected by the interpreter.
///
/// These counters are diagnostic only. `RunStats::executed_instructions` and
/// `RunStats::executed_rle_instructions` retain the exact counts of the
/// unoptimized bytecode execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OptimizationRunStats {
    /// Fast-IR instructions dispatched by Rust.
    pub executed_native_operations: u64,
    /// Coalesced `+`, `-`, `<`, or `>` runs executed.
    pub rle_operations: u64,
    /// Native clear loops executed, including loops skipped for an initial 0.
    pub clear_loops: u64,
    /// Native scanning loops executed.
    pub scan_loops: u64,
    /// Pointer advances performed by native scanning loops.
    pub scan_steps: u64,
    /// Native linear-transfer loops executed.
    pub transfer_loops: u64,
    /// Source-cell units transferred by native linear-transfer loops.
    pub transfer_iterations: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RleOp {
    Right,
    Left,
    Increment,
    Decrement,
}

impl Op {
    const fn rle_op(self) -> Option<RleOp> {
        match self {
            Self::Right => Some(RleOp::Right),
            Self::Left => Some(RleOp::Left),
            Self::Increment => Some(RleOp::Increment),
            Self::Decrement => Some(RleOp::Decrement),
            Self::Output | Self::Input | Self::JumpIfZero(_) | Self::JumpIfNonZero(_) => None,
        }
    }
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
    let optimized = optimize(&instructions);
    execute(&optimized, input)
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

#[derive(Debug, Clone)]
enum FastInstruction {
    Move {
        amount: isize,
        source_offsets: Box<[usize]>,
    },
    Add {
        amount: u8,
        raw_count: u64,
    },
    Input,
    Output,
    Loop {
        body: Vec<FastInstruction>,
        optimization: Option<LoopOptimization>,
    },
}

#[derive(Debug, Clone)]
enum LoopOptimization {
    Clear {
        delta: u8,
        body_raw_count: u64,
        body_rle_count: u64,
    },
    Scan {
        amount: isize,
        source_offsets: Box<[usize]>,
    },
    Transfer {
        source_delta: u8,
        updates: Box<[(isize, u8)]>,
        min_offset: isize,
        max_offset: isize,
        body_raw_count: u64,
        body_rle_count: u64,
    },
}

fn optimize(instructions: &[Instruction]) -> Vec<FastInstruction> {
    optimize_range(instructions, 0, instructions.len())
}

fn optimize_range(instructions: &[Instruction], start: usize, end: usize) -> Vec<FastInstruction> {
    let mut result = Vec::new();
    let mut position = start;
    while position < end {
        match instructions[position].op {
            Op::Right | Op::Left => {
                let op = instructions[position].op;
                let run_start = position;
                while position < end && instructions[position].op == op {
                    position += 1;
                }
                let count = position - run_start;
                let magnitude = isize::try_from(count).expect("BF source fits in isize");
                let amount = if op == Op::Right {
                    magnitude
                } else {
                    -magnitude
                };
                let source_offsets = instructions[run_start..position]
                    .iter()
                    .map(|instruction| instruction.source_offset)
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                result.push(FastInstruction::Move {
                    amount,
                    source_offsets,
                });
            }
            Op::Increment | Op::Decrement => {
                let op = instructions[position].op;
                let run_start = position;
                while position < end && instructions[position].op == op {
                    position += 1;
                }
                let count = position - run_start;
                let magnitude = (count & 0xff) as u8;
                let amount = if op == Op::Increment {
                    magnitude
                } else {
                    0_u8.wrapping_sub(magnitude)
                };
                result.push(FastInstruction::Add {
                    amount,
                    raw_count: count as u64,
                });
            }
            Op::Input => {
                result.push(FastInstruction::Input);
                position += 1;
            }
            Op::Output => {
                result.push(FastInstruction::Output);
                position += 1;
            }
            Op::JumpIfZero(target) => {
                let closing = target - 1;
                let body = optimize_range(instructions, position + 1, closing);
                let optimization = recognize_loop(instructions, position + 1, closing, &body);
                result.push(FastInstruction::Loop { body, optimization });
                position = target;
            }
            Op::JumpIfNonZero(_) => unreachable!("loop closing is consumed with its opening"),
        }
    }
    result
}

fn recognize_loop(
    instructions: &[Instruction],
    start: usize,
    end: usize,
    body: &[FastInstruction],
) -> Option<LoopOptimization> {
    if start == end {
        return None;
    }

    if let [FastInstruction::Add { amount, raw_count }] = body
        && amount % 2 == 1
    {
        return Some(LoopOptimization::Clear {
            delta: *amount,
            body_raw_count: *raw_count,
            body_rle_count: 1,
        });
    }

    if let [
        FastInstruction::Move {
            amount,
            source_offsets,
        },
    ] = body
    {
        return Some(LoopOptimization::Scan {
            amount: *amount,
            source_offsets: source_offsets.clone(),
        });
    }

    let mut pointer = 0_isize;
    let mut min_offset = 0_isize;
    let mut max_offset = 0_isize;
    let mut updates = std::collections::BTreeMap::<isize, u8>::new();
    let mut previous = None;
    let mut rle_count = 0_u64;

    for instruction in &instructions[start..end] {
        let rle_op = instruction.op.rle_op()?;
        if previous != Some(rle_op) {
            rle_count += 1;
            previous = Some(rle_op);
        }
        match instruction.op {
            Op::Right => {
                pointer += 1;
                max_offset = max_offset.max(pointer);
            }
            Op::Left => {
                pointer -= 1;
                min_offset = min_offset.min(pointer);
            }
            Op::Increment => {
                let value = updates.entry(pointer).or_default();
                *value = value.wrapping_add(1);
            }
            Op::Decrement => {
                let value = updates.entry(pointer).or_default();
                *value = value.wrapping_sub(1);
            }
            Op::Output | Op::Input | Op::JumpIfZero(_) | Op::JumpIfNonZero(_) => return None,
        }
    }

    let source_delta = updates.remove(&0).unwrap_or(0);
    if pointer != 0 || !matches!(source_delta, 1 | 255) {
        return None;
    }
    updates.retain(|_, factor| *factor != 0);
    Some(LoopOptimization::Transfer {
        source_delta,
        updates: updates.into_iter().collect::<Vec<_>>().into_boxed_slice(),
        min_offset,
        max_offset,
        body_raw_count: (end - start) as u64,
        body_rle_count: rle_count,
    })
}

struct Machine<'a> {
    tape: Vec<u8>,
    pointer: usize,
    max_pointer: usize,
    input: &'a [u8],
    input_position: usize,
    output: Vec<u8>,
    executed_instructions: u64,
    executed_rle_instructions: u64,
    optimization: OptimizationRunStats,
}

impl<'a> Machine<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            tape: vec![0; TAPE_LEN],
            pointer: 0,
            max_pointer: 0,
            input,
            input_position: 0,
            output: Vec::new(),
            executed_instructions: 0,
            executed_rle_instructions: 0,
            optimization: OptimizationRunStats::default(),
        }
    }

    fn add_counts(&mut self, raw: u64, rle: u64) {
        self.executed_instructions += raw;
        self.executed_rle_instructions += rle;
    }

    fn execute_block(&mut self, instructions: &[FastInstruction]) -> Result<(), Error> {
        for instruction in instructions {
            self.optimization.executed_native_operations += 1;
            match instruction {
                FastInstruction::Move {
                    amount,
                    source_offsets,
                } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts(source_offsets.len() as u64, 1);
                    self.move_pointer(*amount, source_offsets)?;
                }
                FastInstruction::Add { amount, raw_count } => {
                    self.optimization.rle_operations += 1;
                    self.add_counts(*raw_count, 1);
                    self.tape[self.pointer] = self.tape[self.pointer].wrapping_add(*amount);
                }
                FastInstruction::Input => {
                    self.add_counts(1, 1);
                    self.tape[self.pointer] =
                        self.input.get(self.input_position).copied().unwrap_or(0);
                    self.input_position = self.input_position.saturating_add(1);
                }
                FastInstruction::Output => {
                    self.add_counts(1, 1);
                    self.output.push(self.tape[self.pointer]);
                }
                FastInstruction::Loop { body, optimization } => {
                    self.execute_loop(body, optimization.as_ref())?;
                }
            }
        }
        Ok(())
    }

    fn execute_loop(
        &mut self,
        body: &[FastInstruction],
        optimization: Option<&LoopOptimization>,
    ) -> Result<(), Error> {
        match optimization {
            Some(LoopOptimization::Clear {
                delta,
                body_raw_count,
                body_rle_count,
            }) => {
                self.optimization.clear_loops += 1;
                let iterations = iterations_to_zero(self.tape[self.pointer], *delta);
                self.add_counts(
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                self.tape[self.pointer] = 0;
                Ok(())
            }
            Some(LoopOptimization::Scan {
                amount,
                source_offsets,
            }) => {
                self.optimization.scan_loops += 1;
                self.add_counts(1, 1);
                while self.tape[self.pointer] != 0 {
                    self.optimization.scan_steps += 1;
                    self.optimization.rle_operations += 1;
                    self.add_counts(source_offsets.len() as u64, 1);
                    self.move_pointer(*amount, source_offsets)?;
                    self.add_counts(1, 1);
                }
                Ok(())
            }
            Some(LoopOptimization::Transfer {
                source_delta,
                updates,
                min_offset,
                max_offset,
                body_raw_count,
                body_rle_count,
            }) => {
                let initial = self.tape[self.pointer];
                let iterations = if initial == 0 {
                    0
                } else if *source_delta == 255 {
                    u64::from(initial)
                } else {
                    u64::from(0_u8.wrapping_sub(initial))
                };
                if iterations != 0
                    && (!self.offset_is_valid(*min_offset) || !self.offset_is_valid(*max_offset))
                {
                    return self.execute_generic_loop(body);
                }

                self.optimization.transfer_loops += 1;
                self.optimization.transfer_iterations += iterations;
                self.add_counts(
                    1 + iterations * (body_raw_count + 1),
                    1 + iterations * (body_rle_count + 1),
                );
                if iterations != 0 {
                    let factor = iterations as u8;
                    for &(offset, update) in updates.iter() {
                        let target = self.pointer.checked_add_signed(offset).unwrap();
                        self.tape[target] =
                            self.tape[target].wrapping_add(update.wrapping_mul(factor));
                    }
                    if *max_offset > 0 {
                        let reached = self.pointer.checked_add_signed(*max_offset).unwrap();
                        self.max_pointer = self.max_pointer.max(reached);
                    }
                    self.tape[self.pointer] = 0;
                }
                Ok(())
            }
            None => self.execute_generic_loop(body),
        }
    }

    fn execute_generic_loop(&mut self, body: &[FastInstruction]) -> Result<(), Error> {
        self.add_counts(1, 1);
        while self.tape[self.pointer] != 0 {
            self.execute_block(body)?;
            self.add_counts(1, 1);
        }
        Ok(())
    }

    fn offset_is_valid(&self, offset: isize) -> bool {
        self.pointer
            .checked_add_signed(offset)
            .is_some_and(|position| position < self.tape.len())
    }

    fn move_pointer(&mut self, amount: isize, source_offsets: &[usize]) -> Result<(), Error> {
        if amount > 0 {
            let count = amount as usize;
            if self
                .pointer
                .checked_add(count)
                .is_none_or(|p| p >= self.tape.len())
            {
                let valid_steps = self.tape.len() - 1 - self.pointer;
                return Err(Error::TapeOverflow {
                    instruction_offset: source_offsets[valid_steps],
                });
            }
            self.pointer += count;
            self.max_pointer = self.max_pointer.max(self.pointer);
        } else {
            let count = amount.unsigned_abs();
            if count > self.pointer {
                return Err(Error::TapeUnderflow {
                    instruction_offset: source_offsets[self.pointer],
                });
            }
            self.pointer -= count;
        }
        Ok(())
    }
}

fn iterations_to_zero(initial: u8, delta: u8) -> u64 {
    let mut value = initial;
    let mut iterations = 0;
    while value != 0 {
        value = value.wrapping_add(delta);
        iterations += 1;
    }
    iterations
}

fn execute(instructions: &[FastInstruction], input: &[u8]) -> Result<RunResult, Error> {
    let mut machine = Machine::new(input);
    machine.execute_block(instructions)?;
    Ok(RunResult {
        output: machine.output,
        stats: RunStats {
            executed_instructions: machine.executed_instructions,
            executed_rle_instructions: machine.executed_rle_instructions,
            max_pointer: machine.max_pointer,
            optimization: machine.optimization,
        },
    })
}

#[cfg(test)]
fn execute_reference(instructions: &[Instruction], input: &[u8]) -> Result<RunResult, Error> {
    let mut tape = vec![0_u8; TAPE_LEN];
    let mut pointer = 0_usize;
    let mut max_pointer = 0_usize;
    let mut program_counter = 0_usize;
    let mut input_position = 0_usize;
    let mut output = Vec::new();
    let mut executed_instructions = 0_u64;
    let mut executed_rle_instructions = 0_u64;
    let mut previous_rle_op: Option<(usize, RleOp)> = None;

    while let Some(instruction) = instructions.get(program_counter) {
        executed_instructions += 1;
        let rle_op = instruction.op.rle_op();
        let continues_previous_run = matches!(
            (previous_rle_op, rle_op),
            (Some((previous_counter, previous_op)), Some(current_op))
                if previous_counter.checked_add(1) == Some(program_counter)
                    && previous_op == current_op
        );
        if !continues_previous_run {
            executed_rle_instructions += 1;
        }
        previous_rle_op = rle_op.map(|op| (program_counter, op));

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
            executed_rle_instructions,
            max_pointer,
            optimization: OptimizationRunStats::default(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_reference(source: &[u8], input: &[u8]) -> Result<RunResult, Error> {
        let instructions = parse(source)?;
        execute_reference(&instructions, input)
    }

    fn assert_matches_reference(source: &[u8], input: &[u8]) -> RunResult {
        let optimized = run_with_stats(source, input).unwrap();
        let reference = run_reference(source, input).unwrap();
        assert_eq!(optimized.output, reference.output);
        assert_eq!(
            optimized.stats.executed_instructions,
            reference.stats.executed_instructions,
        );
        assert_eq!(
            optimized.stats.executed_rle_instructions,
            reference.stats.executed_rle_instructions,
        );
        assert_eq!(optimized.stats.max_pointer, reference.stats.max_pointer);
        optimized
    }

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
        assert_eq!(result.stats.executed_rle_instructions, 14);
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

    #[test]
    fn optimized_rle_clear_scan_and_transfer_match_the_raw_vm_stats() {
        let clear = assert_matches_reference(b"+++++[-].", b"");
        assert_eq!(clear.stats.optimization.clear_loops, 1);

        // Scan over three nonzero cells and stop at the first zero cell.
        let scan = assert_matches_reference(b"+>+>+<<[>].", b"");
        assert_eq!(scan.stats.optimization.scan_loops, 1);
        assert_eq!(scan.stats.optimization.scan_steps, 3);

        let transfer = assert_matches_reference(b"++++[->>>++>+<<<<]>>>.>.", b"");
        assert_eq!(transfer.output, vec![8, 4]);
        assert_eq!(transfer.stats.optimization.transfer_loops, 1);
        assert_eq!(transfer.stats.optimization.transfer_iterations, 4);

        let wrapping_rle = assert_matches_reference(
            b"++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++\
              ---------------------------------------------------------------->><<.",
            b"",
        );
        assert!(wrapping_rle.stats.optimization.rle_operations >= 4);
    }

    #[test]
    fn generic_nested_loops_and_io_match_the_raw_vm_stats() {
        assert_matches_reference(b",[>+.<-]", &[3]);
        assert_matches_reference(b"++[>++[>+<-]<-]>>.", b"");
        assert_matches_reference(b"[+[.-]].", b"");
    }

    #[test]
    fn optimized_boundary_errors_keep_raw_source_offsets() {
        let underflow = b"comments <  <<";
        assert_eq!(
            run_with_stats(underflow, b""),
            run_reference(underflow, b"")
        );

        let mut overflow = vec![b'>'; TAPE_LEN + 8];
        overflow.splice(10..10, b" ignored ".iter().copied());
        assert_eq!(
            run_with_stats(&overflow, b""),
            run_reference(&overflow, b""),
        );
    }

    #[test]
    fn optimized_loop_counts_match_the_raw_vm_for_every_cell_value() {
        for initial in 0_u16..=255 {
            let mut clear = "+".repeat(usize::from(initial));
            clear.push_str("[-].");
            assert_matches_reference(clear.as_bytes(), b"");

            let mut incrementing_clear = "+".repeat(usize::from(initial));
            incrementing_clear.push_str("[+].");
            assert_matches_reference(incrementing_clear.as_bytes(), b"");

            let mut transfer = "+".repeat(usize::from(initial));
            transfer.push_str("[->>++>+<<<]>>.>.");
            assert_matches_reference(transfer.as_bytes(), b"");

            let mut incrementing_transfer = "+".repeat(usize::from(initial));
            incrementing_transfer.push_str("[+>+<]>.");
            assert_matches_reference(incrementing_transfer.as_bytes(), b"");
        }

        for distance in 1..=32 {
            let mut scan = String::new();
            for _ in 0..distance {
                scan.push_str("+>");
            }
            scan.push_str(&"<".repeat(distance));
            scan.push_str("[>].");
            assert_matches_reference(scan.as_bytes(), b"");
        }
    }
}
