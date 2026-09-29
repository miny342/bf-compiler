use std::collections::HashSet;
use std::error::Error;
use std::fmt;

/// A logical cell allocated by a [`Program`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId(usize);

impl CellId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

/// One destination of a destructive [`Instruction::Transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferTarget {
    pub dst: CellId,
    /// The source value is multiplied by this value modulo 256 before being
    /// added to `dst`.
    pub factor: u8,
}

/// A structured, cell-oriented operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    /// Overwrite `dst` with `value`.
    Set { dst: CellId, value: u8 },
    /// Add `value` to `dst` modulo 256.
    AddConst { dst: CellId, value: u8 },
    /// Add `src * factor` to every target, then set `src` to zero.
    ///
    /// An empty target list is a clear operation. The source must not also be
    /// a target, target cells must be unique, and factors must be nonzero.
    Transfer {
        src: CellId,
        targets: Vec<TransferTarget>,
    },
    /// Overwrite `dst` with one byte of input, or zero at EOF.
    Input { dst: CellId },
    /// Output `src` without changing it.
    Output { src: CellId },
    /// Execute `body` while `condition` is nonzero.
    ///
    /// The condition is not changed implicitly.
    Loop {
        condition: CellId,
        body: Vec<Instruction>,
    },
    /// Execute exactly one branch and consume `condition`.
    ///
    /// A nonzero condition selects `then_body`; zero selects `else_body`.
    /// The condition is zero before the selected body starts and is forced
    /// back to zero after it finishes.
    Branch {
        condition: CellId,
        then_body: Vec<Instruction>,
        else_body: Vec<Instruction>,
    },
}

/// A validated IR program and its statically allocated cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    cell_count: usize,
    instructions: Vec<Instruction>,
}

impl Program {
    pub fn new(cell_count: usize, instructions: Vec<Instruction>) -> Result<Self, IrError> {
        validate_instructions(&instructions, cell_count)?;
        Ok(Self {
            cell_count,
            instructions,
        })
    }

    pub const fn cell_count(&self) -> usize {
        self.cell_count
    }

    pub fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrError {
    CellOutOfBounds { cell: CellId, cell_count: usize },
    TransferSourceIsTarget { cell: CellId },
    DuplicateTransferTarget { cell: CellId },
    ZeroTransferFactor { cell: CellId },
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CellOutOfBounds { cell, cell_count } => write!(
                f,
                "cell {} is outside a program with {cell_count} cells",
                cell.index()
            ),
            Self::TransferSourceIsTarget { cell } => {
                write!(f, "transfer source cell {} is also a target", cell.index())
            }
            Self::DuplicateTransferTarget { cell } => {
                write!(
                    f,
                    "transfer target cell {} occurs more than once",
                    cell.index()
                )
            }
            Self::ZeroTransferFactor { cell } => {
                write!(f, "transfer target cell {} has a zero factor", cell.index())
            }
        }
    }
}

impl Error for IrError {}

fn validate_instructions(instructions: &[Instruction], cell_count: usize) -> Result<(), IrError> {
    for instruction in instructions {
        match instruction {
            Instruction::Set { dst, .. }
            | Instruction::AddConst { dst, .. }
            | Instruction::Input { dst } => validate_cell(*dst, cell_count)?,
            Instruction::Output { src } => validate_cell(*src, cell_count)?,
            Instruction::Transfer { src, targets } => {
                validate_cell(*src, cell_count)?;
                let mut seen = HashSet::with_capacity(targets.len());
                for target in targets {
                    validate_cell(target.dst, cell_count)?;
                    if target.dst == *src {
                        return Err(IrError::TransferSourceIsTarget { cell: *src });
                    }
                    if !seen.insert(target.dst) {
                        return Err(IrError::DuplicateTransferTarget { cell: target.dst });
                    }
                    if target.factor == 0 {
                        return Err(IrError::ZeroTransferFactor { cell: target.dst });
                    }
                }
            }
            Instruction::Loop { condition, body } => {
                validate_cell(*condition, cell_count)?;
                validate_instructions(body, cell_count)?;
            }
            Instruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                validate_cell(*condition, cell_count)?;
                validate_instructions(then_body, cell_count)?;
                validate_instructions(else_body, cell_count)?;
            }
        }
    }
    Ok(())
}

fn validate_cell(cell: CellId, cell_count: usize) -> Result<(), IrError> {
    if cell.index() < cell_count {
        Ok(())
    } else {
        Err(IrError::CellOutOfBounds { cell, cell_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_transfer_aliases() {
        let cell = CellId::new(0);
        let result = Program::new(
            1,
            vec![Instruction::Transfer {
                src: cell,
                targets: vec![TransferTarget {
                    dst: cell,
                    factor: 1,
                }],
            }],
        );
        assert_eq!(result, Err(IrError::TransferSourceIsTarget { cell }));
    }

    #[test]
    fn validates_nested_instructions() {
        let result = Program::new(
            1,
            vec![Instruction::Loop {
                condition: CellId::new(0),
                body: vec![Instruction::Output {
                    src: CellId::new(1),
                }],
            }],
        );
        assert_eq!(
            result,
            Err(IrError::CellOutOfBounds {
                cell: CellId::new(1),
                cell_count: 1,
            })
        );
    }
}
