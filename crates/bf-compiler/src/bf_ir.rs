//! Brainfuck-shaped intermediate representation.

/// A Brainfuck operation before serialization to source text.
///
/// Movement and addition store their complete amount in one node. No
/// normalization or merging of adjacent nodes is performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BfInstruction {
    /// Move right when positive and left when negative.
    Move(isize),
    /// Add to the current cell modulo 256.
    Add(u8),
    Input,
    Output,
    Loop(Vec<BfInstruction>),
}

/// A complete BF IR program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BfProgram {
    instructions: Vec<BfInstruction>,
}

impl BfProgram {
    pub fn new(instructions: Vec<BfInstruction>) -> Self {
        Self { instructions }
    }

    pub fn instructions(&self) -> &[BfInstruction] {
        &self.instructions
    }

    pub fn into_instructions(self) -> Vec<BfInstruction> {
        self.instructions
    }

    /// Serialize the BF IR to Brainfuck source.
    ///
    /// An `Add` node uses the shorter equivalent run of `+` or `-`. This is
    /// the encoding of that node, not an optimization pass: adjacent nodes are
    /// not inspected or merged.
    pub fn to_source(&self) -> String {
        let mut source = String::new();
        write_instructions(&self.instructions, &mut source);
        source
    }
}

fn write_instructions(instructions: &[BfInstruction], output: &mut String) {
    for instruction in instructions {
        match instruction {
            BfInstruction::Move(amount) if *amount >= 0 => {
                output.extend(std::iter::repeat_n('>', amount.unsigned_abs()));
            }
            BfInstruction::Move(amount) => {
                output.extend(std::iter::repeat_n('<', amount.unsigned_abs()));
            }
            BfInstruction::Add(value) if *value <= 128 => {
                output.extend(std::iter::repeat_n('+', usize::from(*value)));
            }
            BfInstruction::Add(value) => {
                let count = usize::from(256_u16 - u16::from(*value));
                output.extend(std::iter::repeat_n('-', count));
            }
            BfInstruction::Input => output.push(','),
            BfInstruction::Output => output.push('.'),
            BfInstruction::Loop(body) => {
                output.push('[');
                write_instructions(body, output);
                output.push(']');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_nested_ir_without_merging_nodes() {
        let program = BfProgram::new(vec![
            BfInstruction::Move(2),
            BfInstruction::Move(-1),
            BfInstruction::Add(2),
            BfInstruction::Add(255),
            BfInstruction::Input,
            BfInstruction::Output,
            BfInstruction::Loop(vec![BfInstruction::Add(255)]),
        ]);

        assert_eq!(program.to_source(), ">><++-,.[-]");
    }
}
