use std::error::Error;
use std::fmt;

use crate::{BfInstruction, BfProgram, CellId, Instruction, Program, TransferTarget};

/// Tape size defined by the project's Brainfuck execution specification.
pub const TAPE_LEN: usize = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenError {
    TapeCapacityExceeded { required: usize, available: usize },
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TapeCapacityExceeded {
                required,
                available,
            } => write!(
                f,
                "program requires {required} tape cells, but only {available} are available"
            ),
        }
    }
}

impl Error for CodegenError {}

/// Compile a validated IR program to Brainfuck source.
pub fn compile(program: &Program) -> Result<String, CodegenError> {
    Ok(lower(program)?.to_source())
}

/// Lower a validated cell IR program to unoptimized BF IR.
pub fn lower(program: &Program) -> Result<BfProgram, CodegenError> {
    let temporary_count = maximum_branch_depth(program.instructions());
    let required = program.cell_count().saturating_add(temporary_count);
    if required > TAPE_LEN {
        return Err(CodegenError::TapeCapacityExceeded {
            required,
            available: TAPE_LEN,
        });
    }

    let mut emitter = Emitter::new(program.cell_count());
    emitter.emit_all(program.instructions());
    Ok(BfProgram::new(emitter.output))
}

fn maximum_branch_depth(instructions: &[Instruction]) -> usize {
    instructions
        .iter()
        .map(|instruction| match instruction {
            Instruction::Loop { body, .. } => maximum_branch_depth(body),
            Instruction::Branch {
                then_body,
                else_body,
                ..
            } => 1 + maximum_branch_depth(then_body).max(maximum_branch_depth(else_body)),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

struct Emitter {
    output: Vec<BfInstruction>,
    pointer: usize,
    first_temporary: usize,
    temporary_depth: usize,
}

impl Emitter {
    fn new(first_temporary: usize) -> Self {
        Self {
            output: Vec::new(),
            pointer: 0,
            first_temporary,
            temporary_depth: 0,
        }
    }

    fn emit_all(&mut self, instructions: &[Instruction]) {
        for instruction in instructions {
            self.emit(instruction);
        }
    }

    fn emit(&mut self, instruction: &Instruction) {
        match instruction {
            Instruction::Set { dst, value } => self.set(*dst, *value),
            Instruction::AddConst { dst, value } => {
                self.move_to(*dst);
                self.adjust(*value);
            }
            Instruction::Transfer { src, targets } => self.transfer(*src, targets),
            Instruction::Input { dst } => {
                self.move_to(*dst);
                self.output.push(BfInstruction::Input);
            }
            Instruction::Output { src } => {
                self.move_to(*src);
                self.output.push(BfInstruction::Output);
            }
            Instruction::Loop { condition, body } => {
                self.move_to(*condition);
                let body = self.capture(|emitter| {
                    emitter.emit_all(body);
                    emitter.move_to(*condition);
                });
                self.output.push(BfInstruction::Loop(body));
            }
            Instruction::Branch {
                condition,
                then_body,
                else_body,
            } => self.branch(*condition, then_body, else_body),
        }
    }

    fn set(&mut self, dst: CellId, value: u8) {
        self.clear(dst);
        self.adjust(value);
    }

    fn clear(&mut self, cell: CellId) {
        self.move_to(cell);
        self.output
            .push(BfInstruction::Loop(vec![BfInstruction::Add(255)]));
    }

    fn adjust(&mut self, value: u8) {
        self.output.push(BfInstruction::Add(value));
    }

    fn transfer(&mut self, src: CellId, targets: &[TransferTarget]) {
        if targets.is_empty() {
            self.clear(src);
            return;
        }

        self.move_to(src);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            for target in targets {
                emitter.move_to(target.dst);
                emitter.adjust(target.factor);
            }
            emitter.move_to(src);
        });
        self.output.push(BfInstruction::Loop(body));
    }

    fn branch(&mut self, condition: CellId, then_body: &[Instruction], else_body: &[Instruction]) {
        let flag = self.acquire_temporary();
        self.set(flag, 1);

        self.move_to(condition);
        let then_loop = self.capture(|emitter| {
            emitter.clear(condition);
            emitter.emit_all(then_body);
            emitter.clear(condition);
            emitter.clear(flag);
            emitter.move_to(condition);
        });
        self.output.push(BfInstruction::Loop(then_loop));

        self.move_to(flag);
        let else_loop = self.capture(|emitter| {
            emitter.clear(flag);
            emitter.emit_all(else_body);
            emitter.clear(condition);
            emitter.clear(flag);
        });
        self.output.push(BfInstruction::Loop(else_loop));

        self.release_temporary();
    }

    fn acquire_temporary(&mut self) -> CellId {
        let cell = CellId::new(self.first_temporary + self.temporary_depth);
        self.temporary_depth += 1;
        cell
    }

    fn release_temporary(&mut self) {
        self.temporary_depth -= 1;
    }

    fn move_to(&mut self, cell: CellId) {
        let destination = cell.index();
        let amount = destination as isize - self.pointer as isize;
        self.output.push(BfInstruction::Move(amount));
        self.pointer = destination;
    }

    fn capture(&mut self, emit: impl FnOnce(&mut Self)) -> Vec<BfInstruction> {
        let outer = std::mem::take(&mut self.output);
        emit(self);
        std::mem::replace(&mut self.output, outer)
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;

    fn execute(cell_count: usize, instructions: Vec<Instruction>, input: &[u8]) -> Vec<u8> {
        let program = Program::new(cell_count, instructions).unwrap();
        let source = compile(&program).unwrap();
        run(source.as_bytes(), input).unwrap()
    }

    #[test]
    fn lowering_does_not_optimize_bf_ir() {
        let cell = CellId::new(0);
        let program = Program::new(
            1,
            vec![
                Instruction::AddConst {
                    dst: cell,
                    value: 1,
                },
                Instruction::AddConst {
                    dst: cell,
                    value: 2,
                },
            ],
        )
        .unwrap();

        assert_eq!(
            lower(&program).unwrap().into_instructions(),
            vec![
                BfInstruction::Move(0),
                BfInstruction::Add(1),
                BfInstruction::Move(0),
                BfInstruction::Add(2),
            ]
        );
    }

    #[test]
    fn set_add_input_and_output_work() {
        let cell = CellId::new(0);
        let output = execute(
            1,
            vec![
                Instruction::Set {
                    dst: cell,
                    value: 250,
                },
                Instruction::AddConst {
                    dst: cell,
                    value: 10,
                },
                Instruction::Output { src: cell },
                Instruction::Input { dst: cell },
                Instruction::Output { src: cell },
            ],
            b"A",
        );
        assert_eq!(output, vec![4, b'A']);
    }

    #[test]
    fn transfer_supports_clear_add_sub_and_clone() {
        let source = CellId::new(0);
        let add = CellId::new(1);
        let subtract = CellId::new(2);
        let clone = CellId::new(3);
        let output = execute(
            4,
            vec![
                Instruction::Set {
                    dst: source,
                    value: 5,
                },
                Instruction::Set {
                    dst: add,
                    value: 10,
                },
                Instruction::Set {
                    dst: subtract,
                    value: 10,
                },
                Instruction::Transfer {
                    src: source,
                    targets: vec![
                        TransferTarget {
                            dst: add,
                            factor: 1,
                        },
                        TransferTarget {
                            dst: subtract,
                            factor: 255,
                        },
                        TransferTarget {
                            dst: clone,
                            factor: 1,
                        },
                    ],
                },
                Instruction::Output { src: source },
                Instruction::Output { src: add },
                Instruction::Output { src: subtract },
                Instruction::Output { src: clone },
            ],
            b"",
        );
        assert_eq!(output, vec![0, 15, 5, 5]);
    }

    #[test]
    fn loop_has_native_brainfuck_semantics() {
        let counter = CellId::new(0);
        let value = CellId::new(1);
        let output = execute(
            2,
            vec![
                Instruction::Set {
                    dst: counter,
                    value: 3,
                },
                Instruction::Loop {
                    condition: counter,
                    body: vec![
                        Instruction::AddConst {
                            dst: value,
                            value: 2,
                        },
                        Instruction::AddConst {
                            dst: counter,
                            value: 255,
                        },
                    ],
                },
                Instruction::Output { src: value },
            ],
            b"",
        );
        assert_eq!(output, vec![6]);
    }

    #[test]
    fn branch_runs_once_and_consumes_its_condition() {
        let condition = CellId::new(0);
        let value = CellId::new(1);
        for (condition_value, expected) in [(0, 20), (1, 10), (7, 10)] {
            let output = execute(
                2,
                vec![
                    Instruction::Set {
                        dst: condition,
                        value: condition_value,
                    },
                    Instruction::Branch {
                        condition,
                        then_body: vec![
                            Instruction::Set {
                                dst: value,
                                value: 10,
                            },
                            Instruction::Set {
                                dst: condition,
                                value: 99,
                            },
                        ],
                        else_body: vec![
                            Instruction::Set {
                                dst: value,
                                value: 20,
                            },
                            Instruction::Set {
                                dst: condition,
                                value: 99,
                            },
                        ],
                    },
                    Instruction::Output { src: condition },
                    Instruction::Output { src: value },
                ],
                b"",
            );
            assert_eq!(output, vec![0, expected]);
        }
    }

    #[test]
    fn nested_branches_use_distinct_temporary_cells() {
        let outer = CellId::new(0);
        let inner = CellId::new(1);
        let value = CellId::new(2);
        let output = execute(
            3,
            vec![
                Instruction::Set {
                    dst: outer,
                    value: 1,
                },
                Instruction::Set {
                    dst: inner,
                    value: 1,
                },
                Instruction::Branch {
                    condition: outer,
                    then_body: vec![Instruction::Branch {
                        condition: inner,
                        then_body: vec![Instruction::Set {
                            dst: value,
                            value: 42,
                        }],
                        else_body: vec![],
                    }],
                    else_body: vec![],
                },
                Instruction::Output { src: value },
            ],
            b"",
        );
        assert_eq!(output, vec![42]);
    }
}
