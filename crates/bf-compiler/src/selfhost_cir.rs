//! Compact, arena-independent interchange format for the stage-2 selfhost CIR.
//!
//! The selfhost compiler lowers into a deliberately flat ABI: frame values are
//! byte slots, static objects are addressed by a 24-bit logical cell index and
//! dynamic aggregate accesses carry their base and length explicitly.  This
//! module preserves that representation instead of rebuilding the typed AST or
//! serializing arena records verbatim.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

const MAGIC: &[u8; 8] = b"BFCIR\0\x01\n";
const RECORD_FUNCTION: u8 = 1;
const RECORD_CONTINUATION: u8 = 2;
const RECORD_INSTRUCTION: u8 = 3;
const RECORD_TERMINATOR: u8 = 4;
const RECORD_END: u8 = 0xff;
const MAX_U24: u32 = 0x00ff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirStorage {
    Frame,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirReturnType {
    Void,
    Cell,
    Aggregate(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelfhostCirParameter {
    pub destination: u8,
    pub cells: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfhostCirFunction {
    pub id: u16,
    pub entry: u16,
    pub frame_cells: u8,
    pub return_type: SelfhostCirReturnType,
    pub parameters: Vec<SelfhostCirParameter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirUnaryOp {
    Negate,
    Not,
    Booleanize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirBinaryOp {
    Add,
    Subtract,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirGlobalOp {
    Set,
    Copy,
    Store,
    Add,
    Subtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelfhostCirArrayOp {
    Load,
    Store,
    Add,
    Subtract,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfhostCirInstruction {
    Set {
        destination: u8,
        value: u8,
    },
    Copy {
        destination: u8,
        source: u8,
    },
    CopyAbiValue {
        destination: u8,
    },
    Input {
        destination: u8,
    },
    Output {
        source: u8,
    },
    Unary {
        op: SelfhostCirUnaryOp,
        destination: u8,
    },
    Binary {
        op: SelfhostCirBinaryOp,
        destination: u8,
        source: u8,
    },
    Global {
        op: SelfhostCirGlobalOp,
        data: u8,
        address: u32,
    },
    Array {
        op: SelfhostCirArrayOp,
        data: u8,
        offset_low: u8,
        offset_high: u8,
        base: u32,
        cells: u32,
        storage: SelfhostCirStorage,
    },
    CopyOutbox {
        destination: u8,
        index: u8,
    },
    OffsetAddScaled {
        low: u8,
        high: u8,
        source: u8,
        amount: u16,
    },
    OffsetAddConstant {
        low: u8,
        high: u8,
        amount: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelfhostCirCallArgument {
    pub source: u8,
    pub destination: u8,
    pub cells: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfhostCirTerminator {
    Goto {
        target: u16,
    },
    Branch {
        condition: u8,
        then_target: u16,
        else_target: u16,
    },
    Call {
        callee: u16,
        arguments: Vec<SelfhostCirCallArgument>,
        return_to: u16,
    },
    ReturnVoid,
    ReturnCell {
        source: u8,
    },
    ReturnAggregate {
        source: u8,
        cells: u8,
    },
    Halt,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfhostCirContinuation {
    pub id: u16,
    pub function: u16,
    pub instructions: Vec<SelfhostCirInstruction>,
    pub terminator: SelfhostCirTerminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfhostCirProgram {
    pub static_cells: u32,
    pub main_function: u16,
    pub functions: Vec<SelfhostCirFunction>,
    pub continuations: Vec<SelfhostCirContinuation>,
}

impl SelfhostCirProgram {
    pub fn new(
        static_cells: u32,
        main_function: u16,
        functions: Vec<SelfhostCirFunction>,
        continuations: Vec<SelfhostCirContinuation>,
    ) -> Result<Self, SelfhostCirError> {
        let program = Self {
            static_cells,
            main_function,
            functions,
            continuations,
        };
        program.validate()?;
        Ok(program)
    }

    pub fn encode(&self) -> Result<Vec<u8>, SelfhostCirError> {
        self.validate()?;
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        push_u24(&mut output, self.static_cells)?;
        push_u16(&mut output, self.main_function);
        for function in &self.functions {
            output.push(RECORD_FUNCTION);
            push_u16(&mut output, function.id);
            push_u16(&mut output, function.entry);
            output.push(function.frame_cells);
            match function.return_type {
                SelfhostCirReturnType::Void => output.extend_from_slice(&[0, 0]),
                SelfhostCirReturnType::Cell => output.extend_from_slice(&[1, 0]),
                SelfhostCirReturnType::Aggregate(cells) => {
                    output.extend_from_slice(&[2, cells]);
                }
            }
            output.push(narrow_count(
                "function parameters",
                function.parameters.len(),
            )?);
            for parameter in &function.parameters {
                output.extend_from_slice(&[parameter.destination, parameter.cells]);
            }
        }
        for continuation in &self.continuations {
            output.push(RECORD_CONTINUATION);
            push_u16(&mut output, continuation.id);
            push_u16(&mut output, continuation.function);
            for instruction in &continuation.instructions {
                output.push(RECORD_INSTRUCTION);
                encode_instruction(&mut output, instruction)?;
            }
            output.push(RECORD_TERMINATOR);
            encode_terminator(&mut output, &continuation.terminator)?;
        }
        output.push(RECORD_END);
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SelfhostCirError> {
        let mut input = Decoder::new(bytes);
        if input.take(MAGIC.len())? != MAGIC {
            return Err(input.error("invalid CIR magic or version"));
        }
        let static_cells = input.u24()?;
        let main_function = input.u16()?;
        let mut functions = Vec::new();
        let mut continuations: Vec<SelfhostCirContinuation> = Vec::new();
        let mut current: Option<(u16, u16, Vec<SelfhostCirInstruction>)> = None;
        loop {
            let tag = input.u8()?;
            match tag {
                RECORD_FUNCTION => {
                    if current.is_some() {
                        return Err(input.error("function record inside a continuation"));
                    }
                    let id = input.u16()?;
                    let entry = input.u16()?;
                    let frame_cells = input.u8()?;
                    let return_tag = input.u8()?;
                    let return_cells = input.u8()?;
                    let return_type = match (return_tag, return_cells) {
                        (0, 0) => SelfhostCirReturnType::Void,
                        (1, 0) => SelfhostCirReturnType::Cell,
                        (2, cells) if cells != 0 => SelfhostCirReturnType::Aggregate(cells),
                        _ => return Err(input.error("invalid function return type")),
                    };
                    let parameter_count = usize::from(input.u8()?);
                    let mut parameters = Vec::with_capacity(parameter_count);
                    for _ in 0..parameter_count {
                        parameters.push(SelfhostCirParameter {
                            destination: input.u8()?,
                            cells: input.u8()?,
                        });
                    }
                    functions.push(SelfhostCirFunction {
                        id,
                        entry,
                        frame_cells,
                        return_type,
                        parameters,
                    });
                }
                RECORD_CONTINUATION => {
                    if current.is_some() {
                        return Err(input.error("continuation has no terminator"));
                    }
                    current = Some((input.u16()?, input.u16()?, Vec::new()));
                }
                RECORD_INSTRUCTION => {
                    let Some((_, _, instructions)) = current.as_mut() else {
                        return Err(input.error("instruction outside a continuation"));
                    };
                    instructions.push(decode_instruction(&mut input)?);
                }
                RECORD_TERMINATOR => {
                    let Some((id, function, instructions)) = current.take() else {
                        return Err(input.error("terminator outside a continuation"));
                    };
                    continuations.push(SelfhostCirContinuation {
                        id,
                        function,
                        instructions,
                        terminator: decode_terminator(&mut input)?,
                    });
                }
                RECORD_END => {
                    if current.is_some() {
                        return Err(input.error("continuation has no terminator"));
                    }
                    if !input.is_empty() {
                        return Err(input.error("trailing bytes after CIR end record"));
                    }
                    break;
                }
                _ => return Err(input.error(format!("unknown CIR record tag {tag}"))),
            }
        }
        Self::new(static_cells, main_function, functions, continuations)
    }

    pub fn validate(&self) -> Result<(), SelfhostCirError> {
        if self.static_cells > MAX_U24 {
            return Err(SelfhostCirError::Invalid(
                "static cell count exceeds 24 bits".into(),
            ));
        }
        let functions = self
            .functions
            .iter()
            .map(|function| (function.id, function))
            .collect::<HashMap<_, _>>();
        if functions.len() != self.functions.len() {
            return Err(SelfhostCirError::Invalid("duplicate function ID".into()));
        }
        if !functions.contains_key(&self.main_function) {
            return Err(SelfhostCirError::Invalid("unknown main function".into()));
        }
        let continuations = self
            .continuations
            .iter()
            .map(|continuation| (continuation.id, continuation))
            .collect::<HashMap<_, _>>();
        if continuations.len() != self.continuations.len() {
            return Err(SelfhostCirError::Invalid(
                "duplicate continuation ID".into(),
            ));
        }
        if continuations.contains_key(&0) {
            return Err(SelfhostCirError::Invalid(
                "continuation ID zero is reserved".into(),
            ));
        }
        for function in &self.functions {
            if function.frame_cells == 0 {
                return Err(SelfhostCirError::Invalid(format!(
                    "function {} has an empty frame",
                    function.id
                )));
            }
            let Some(entry) = continuations.get(&function.entry) else {
                return Err(SelfhostCirError::Invalid(format!(
                    "function {} has unknown entry {}",
                    function.id, function.entry
                )));
            };
            if entry.function != function.id {
                return Err(SelfhostCirError::Invalid(format!(
                    "function {} entry {} belongs to function {}",
                    function.id, function.entry, entry.function
                )));
            }
            let mut destinations = HashSet::new();
            for parameter in &function.parameters {
                if parameter.cells == 0
                    || usize::from(parameter.destination) + usize::from(parameter.cells)
                        > usize::from(function.frame_cells)
                {
                    return Err(SelfhostCirError::Invalid(format!(
                        "function {} has an out-of-bounds parameter",
                        function.id
                    )));
                }
                for slot in parameter.destination..parameter.destination + parameter.cells {
                    if !destinations.insert(slot) {
                        return Err(SelfhostCirError::Invalid(format!(
                            "function {} has overlapping parameters",
                            function.id
                        )));
                    }
                }
            }
        }
        for continuation in &self.continuations {
            let Some(function) = functions.get(&continuation.function) else {
                return Err(SelfhostCirError::Invalid(format!(
                    "continuation {} has unknown owner {}",
                    continuation.id, continuation.function
                )));
            };
            for instruction in &continuation.instructions {
                validate_instruction(instruction, function.frame_cells, self.static_cells)?;
            }
            validate_terminator(
                &continuation.terminator,
                function,
                &functions,
                &continuations,
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfhostCirError {
    Decode { offset: usize, message: String },
    Invalid(String),
}

impl fmt::Display for SelfhostCirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode { offset, message } => {
                write!(f, "invalid selfhost CIR at byte {offset}: {message}")
            }
            Self::Invalid(message) => write!(f, "invalid selfhost CIR: {message}"),
        }
    }
}

impl Error for SelfhostCirError {}

fn validate_instruction(
    instruction: &SelfhostCirInstruction,
    frame_cells: u8,
    static_cells: u32,
) -> Result<(), SelfhostCirError> {
    let slot = |slot: u8| {
        if slot < frame_cells {
            Ok(())
        } else {
            Err(SelfhostCirError::Invalid(format!(
                "frame slot {slot} exceeds frame size {frame_cells}"
            )))
        }
    };
    let global = |address: u32| {
        if address < static_cells {
            Ok(())
        } else {
            Err(SelfhostCirError::Invalid(format!(
                "global address {address} exceeds static size {static_cells}"
            )))
        }
    };
    match *instruction {
        SelfhostCirInstruction::Set { destination, .. }
        | SelfhostCirInstruction::CopyAbiValue { destination }
        | SelfhostCirInstruction::Input { destination }
        | SelfhostCirInstruction::Unary { destination, .. } => slot(destination),
        SelfhostCirInstruction::Copy {
            destination,
            source,
        }
        | SelfhostCirInstruction::Binary {
            destination,
            source,
            ..
        } => {
            slot(destination)?;
            slot(source)
        }
        SelfhostCirInstruction::Output { source } => slot(source),
        SelfhostCirInstruction::Global {
            op: SelfhostCirGlobalOp::Set,
            address,
            ..
        } => global(address),
        SelfhostCirInstruction::Global { data, address, .. } => {
            slot(data)?;
            global(address)
        }
        SelfhostCirInstruction::Array {
            data,
            offset_low,
            offset_high,
            base,
            cells,
            storage,
            ..
        } => {
            slot(data)?;
            slot(offset_low)?;
            slot(offset_high)?;
            if cells == 0 || cells > u16::MAX.into() {
                return Err(SelfhostCirError::Invalid(
                    "dynamic region must contain 1..=65535 cells".into(),
                ));
            }
            match storage {
                SelfhostCirStorage::Frame => {
                    let end = base
                        .checked_add(cells)
                        .ok_or_else(|| SelfhostCirError::Invalid("frame region overflow".into()))?;
                    if end > u32::from(frame_cells) {
                        return Err(SelfhostCirError::Invalid(
                            "dynamic frame region exceeds its frame".into(),
                        ));
                    }
                }
                SelfhostCirStorage::Global => {
                    global(base)?;
                    let last = base.checked_add(cells - 1).ok_or_else(|| {
                        SelfhostCirError::Invalid("global region overflow".into())
                    })?;
                    global(last)?;
                }
            }
            Ok(())
        }
        SelfhostCirInstruction::CopyOutbox { destination, .. } => slot(destination),
        SelfhostCirInstruction::OffsetAddScaled {
            low, high, source, ..
        } => {
            slot(low)?;
            slot(high)?;
            slot(source)
        }
        SelfhostCirInstruction::OffsetAddConstant { low, high, .. } => {
            slot(low)?;
            slot(high)
        }
    }
}

fn validate_terminator(
    terminator: &SelfhostCirTerminator,
    function: &SelfhostCirFunction,
    functions: &HashMap<u16, &SelfhostCirFunction>,
    continuations: &HashMap<u16, &SelfhostCirContinuation>,
) -> Result<(), SelfhostCirError> {
    let successor = |target: u16, expected_owner: u16| {
        let target = continuations.get(&target).ok_or_else(|| {
            SelfhostCirError::Invalid(format!("unknown continuation target {target}"))
        })?;
        if target.function != expected_owner {
            return Err(SelfhostCirError::Invalid(format!(
                "continuation target {} crosses a function boundary",
                target.id
            )));
        }
        Ok(())
    };
    let slot = |source: u8| {
        if source < function.frame_cells {
            Ok(())
        } else {
            Err(SelfhostCirError::Invalid(format!(
                "return/branch slot {source} exceeds function {} frame",
                function.id
            )))
        }
    };
    match terminator {
        SelfhostCirTerminator::Goto { target } => successor(*target, function.id),
        SelfhostCirTerminator::Branch {
            condition,
            then_target,
            else_target,
        } => {
            slot(*condition)?;
            successor(*then_target, function.id)?;
            successor(*else_target, function.id)
        }
        SelfhostCirTerminator::Call {
            callee,
            arguments,
            return_to,
        } => {
            let callee = functions.get(callee).ok_or_else(|| {
                SelfhostCirError::Invalid(format!("call targets unknown function {callee}"))
            })?;
            successor(*return_to, function.id)?;
            if arguments.len() != callee.parameters.len() {
                return Err(SelfhostCirError::Invalid(format!(
                    "call to function {} has {} arguments, expected {}",
                    callee.id,
                    arguments.len(),
                    callee.parameters.len()
                )));
            }
            for (argument, parameter) in arguments.iter().zip(&callee.parameters) {
                if argument.cells == 0
                    || usize::from(argument.source) + usize::from(argument.cells)
                        > usize::from(function.frame_cells)
                    || argument.destination != parameter.destination
                    || argument.cells != parameter.cells
                {
                    return Err(SelfhostCirError::Invalid(format!(
                        "call to function {} has an invalid argument layout",
                        callee.id
                    )));
                }
            }
            Ok(())
        }
        SelfhostCirTerminator::ReturnVoid => {
            if function.return_type == SelfhostCirReturnType::Void {
                Ok(())
            } else {
                Err(SelfhostCirError::Invalid(format!(
                    "function {} returns void with a non-void descriptor",
                    function.id
                )))
            }
        }
        SelfhostCirTerminator::ReturnCell { source } => {
            slot(*source)?;
            if function.return_type == SelfhostCirReturnType::Cell {
                Ok(())
            } else {
                Err(SelfhostCirError::Invalid(format!(
                    "function {} returns a cell with a different descriptor",
                    function.id
                )))
            }
        }
        SelfhostCirTerminator::ReturnAggregate { source, cells } => {
            if *cells == 0
                || usize::from(*source) + usize::from(*cells) > usize::from(function.frame_cells)
                || function.return_type != SelfhostCirReturnType::Aggregate(*cells)
            {
                Err(SelfhostCirError::Invalid(format!(
                    "function {} has an invalid aggregate return",
                    function.id
                )))
            } else {
                Ok(())
            }
        }
        SelfhostCirTerminator::Halt => {
            if function.id == 0 || function.return_type == SelfhostCirReturnType::Void {
                Ok(())
            } else {
                Err(SelfhostCirError::Invalid("non-void function halts".into()))
            }
        }
        SelfhostCirTerminator::Abort => Ok(()),
    }
}

fn encode_instruction(
    output: &mut Vec<u8>,
    instruction: &SelfhostCirInstruction,
) -> Result<(), SelfhostCirError> {
    match instruction {
        SelfhostCirInstruction::Set { destination, value } => {
            output.extend_from_slice(&[1, *destination, *value]);
        }
        SelfhostCirInstruction::Copy {
            destination,
            source,
        } => output.extend_from_slice(&[2, *destination, *source]),
        SelfhostCirInstruction::CopyAbiValue { destination } => {
            output.extend_from_slice(&[3, *destination]);
        }
        SelfhostCirInstruction::Input { destination } => {
            output.extend_from_slice(&[4, *destination]);
        }
        SelfhostCirInstruction::Output { source } => output.extend_from_slice(&[5, *source]),
        SelfhostCirInstruction::Binary {
            op,
            destination,
            source,
        } => output.extend_from_slice(&[binary_tag(*op), *destination, *source]),
        SelfhostCirInstruction::Unary { op, destination } => {
            output.extend_from_slice(&[unary_tag(*op), *destination]);
        }
        SelfhostCirInstruction::Global { op, data, address } => {
            output.extend_from_slice(&[global_tag(*op), *data]);
            push_u24(output, *address)?;
        }
        SelfhostCirInstruction::Array {
            op,
            data,
            offset_low,
            offset_high,
            base,
            cells,
            storage,
        } => {
            output.extend_from_slice(&[array_tag(*op), *data, *offset_low, *offset_high]);
            push_u24(output, *base)?;
            push_u24(output, *cells)?;
            output.push(match storage {
                SelfhostCirStorage::Frame => 0,
                SelfhostCirStorage::Global => 1,
            });
        }
        SelfhostCirInstruction::CopyOutbox { destination, index } => {
            output.extend_from_slice(&[26, *destination, *index]);
        }
        SelfhostCirInstruction::OffsetAddScaled {
            low,
            high,
            source,
            amount,
        } => {
            output.extend_from_slice(&[27, *low, *high, *source]);
            push_u16(output, *amount);
        }
        SelfhostCirInstruction::OffsetAddConstant { low, high, amount } => {
            output.extend_from_slice(&[28, *low, *high]);
            push_u16(output, *amount);
        }
    }
    Ok(())
}

fn decode_instruction(input: &mut Decoder<'_>) -> Result<SelfhostCirInstruction, SelfhostCirError> {
    let kind = input.u8()?;
    Ok(match kind {
        1 => SelfhostCirInstruction::Set {
            destination: input.u8()?,
            value: input.u8()?,
        },
        2 => SelfhostCirInstruction::Copy {
            destination: input.u8()?,
            source: input.u8()?,
        },
        3 => SelfhostCirInstruction::CopyAbiValue {
            destination: input.u8()?,
        },
        4 => SelfhostCirInstruction::Input {
            destination: input.u8()?,
        },
        5 => SelfhostCirInstruction::Output {
            source: input.u8()?,
        },
        6..=7 | 11..=16 => SelfhostCirInstruction::Binary {
            op: binary_from_tag(kind).expect("matched binary tag"),
            destination: input.u8()?,
            source: input.u8()?,
        },
        8..=10 => SelfhostCirInstruction::Unary {
            op: unary_from_tag(kind).expect("matched unary tag"),
            destination: input.u8()?,
        },
        17..=21 => SelfhostCirInstruction::Global {
            op: global_from_tag(kind).expect("matched global tag"),
            data: input.u8()?,
            address: input.u24()?,
        },
        22..=25 => SelfhostCirInstruction::Array {
            op: array_from_tag(kind).expect("matched array tag"),
            data: input.u8()?,
            offset_low: input.u8()?,
            offset_high: input.u8()?,
            base: input.u24()?,
            cells: input.u24()?,
            storage: match input.u8()? {
                0 => SelfhostCirStorage::Frame,
                1 => SelfhostCirStorage::Global,
                _ => return Err(input.error("invalid dynamic storage tag")),
            },
        },
        26 => SelfhostCirInstruction::CopyOutbox {
            destination: input.u8()?,
            index: input.u8()?,
        },
        27 => SelfhostCirInstruction::OffsetAddScaled {
            low: input.u8()?,
            high: input.u8()?,
            source: input.u8()?,
            amount: input.u16()?,
        },
        28 => SelfhostCirInstruction::OffsetAddConstant {
            low: input.u8()?,
            high: input.u8()?,
            amount: input.u16()?,
        },
        _ => return Err(input.error(format!("unknown instruction tag {kind}"))),
    })
}

fn encode_terminator(
    output: &mut Vec<u8>,
    terminator: &SelfhostCirTerminator,
) -> Result<(), SelfhostCirError> {
    match terminator {
        SelfhostCirTerminator::Goto { target } => {
            output.push(1);
            push_u16(output, *target);
        }
        SelfhostCirTerminator::Branch {
            condition,
            then_target,
            else_target,
        } => {
            output.extend_from_slice(&[2, *condition]);
            push_u16(output, *then_target);
            push_u16(output, *else_target);
        }
        SelfhostCirTerminator::Call {
            callee,
            arguments,
            return_to,
        } => {
            output.push(3);
            push_u16(output, *callee);
            push_u16(output, *return_to);
            output.push(narrow_count("call arguments", arguments.len())?);
            for argument in arguments {
                output.extend_from_slice(&[argument.source, argument.destination, argument.cells]);
            }
        }
        SelfhostCirTerminator::ReturnVoid => output.push(4),
        SelfhostCirTerminator::ReturnCell { source } => {
            output.extend_from_slice(&[5, *source]);
        }
        SelfhostCirTerminator::Halt => output.push(6),
        SelfhostCirTerminator::ReturnAggregate { source, cells } => {
            output.extend_from_slice(&[7, *source, *cells]);
        }
        SelfhostCirTerminator::Abort => output.push(8),
    }
    Ok(())
}

fn decode_terminator(input: &mut Decoder<'_>) -> Result<SelfhostCirTerminator, SelfhostCirError> {
    let kind = input.u8()?;
    Ok(match kind {
        1 => SelfhostCirTerminator::Goto {
            target: input.u16()?,
        },
        2 => SelfhostCirTerminator::Branch {
            condition: input.u8()?,
            then_target: input.u16()?,
            else_target: input.u16()?,
        },
        3 => {
            let callee = input.u16()?;
            let return_to = input.u16()?;
            let argument_count = usize::from(input.u8()?);
            let mut arguments = Vec::with_capacity(argument_count);
            for _ in 0..argument_count {
                arguments.push(SelfhostCirCallArgument {
                    source: input.u8()?,
                    destination: input.u8()?,
                    cells: input.u8()?,
                });
            }
            SelfhostCirTerminator::Call {
                callee,
                arguments,
                return_to,
            }
        }
        4 => SelfhostCirTerminator::ReturnVoid,
        5 => SelfhostCirTerminator::ReturnCell {
            source: input.u8()?,
        },
        6 => SelfhostCirTerminator::Halt,
        7 => SelfhostCirTerminator::ReturnAggregate {
            source: input.u8()?,
            cells: input.u8()?,
        },
        8 => SelfhostCirTerminator::Abort,
        _ => return Err(input.error(format!("unknown terminator tag {kind}"))),
    })
}

const fn unary_tag(op: SelfhostCirUnaryOp) -> u8 {
    match op {
        SelfhostCirUnaryOp::Negate => 8,
        SelfhostCirUnaryOp::Not => 9,
        SelfhostCirUnaryOp::Booleanize => 10,
    }
}

const fn unary_from_tag(tag: u8) -> Option<SelfhostCirUnaryOp> {
    match tag {
        8 => Some(SelfhostCirUnaryOp::Negate),
        9 => Some(SelfhostCirUnaryOp::Not),
        10 => Some(SelfhostCirUnaryOp::Booleanize),
        _ => None,
    }
}

const fn binary_tag(op: SelfhostCirBinaryOp) -> u8 {
    match op {
        SelfhostCirBinaryOp::Add => 6,
        SelfhostCirBinaryOp::Subtract => 7,
        SelfhostCirBinaryOp::Less => 11,
        SelfhostCirBinaryOp::LessEqual => 12,
        SelfhostCirBinaryOp::Greater => 13,
        SelfhostCirBinaryOp::GreaterEqual => 14,
        SelfhostCirBinaryOp::Equal => 15,
        SelfhostCirBinaryOp::NotEqual => 16,
    }
}

const fn binary_from_tag(tag: u8) -> Option<SelfhostCirBinaryOp> {
    match tag {
        6 => Some(SelfhostCirBinaryOp::Add),
        7 => Some(SelfhostCirBinaryOp::Subtract),
        11 => Some(SelfhostCirBinaryOp::Less),
        12 => Some(SelfhostCirBinaryOp::LessEqual),
        13 => Some(SelfhostCirBinaryOp::Greater),
        14 => Some(SelfhostCirBinaryOp::GreaterEqual),
        15 => Some(SelfhostCirBinaryOp::Equal),
        16 => Some(SelfhostCirBinaryOp::NotEqual),
        _ => None,
    }
}

const fn global_tag(op: SelfhostCirGlobalOp) -> u8 {
    match op {
        SelfhostCirGlobalOp::Set => 17,
        SelfhostCirGlobalOp::Copy => 18,
        SelfhostCirGlobalOp::Store => 19,
        SelfhostCirGlobalOp::Add => 20,
        SelfhostCirGlobalOp::Subtract => 21,
    }
}

const fn global_from_tag(tag: u8) -> Option<SelfhostCirGlobalOp> {
    match tag {
        17 => Some(SelfhostCirGlobalOp::Set),
        18 => Some(SelfhostCirGlobalOp::Copy),
        19 => Some(SelfhostCirGlobalOp::Store),
        20 => Some(SelfhostCirGlobalOp::Add),
        21 => Some(SelfhostCirGlobalOp::Subtract),
        _ => None,
    }
}

const fn array_tag(op: SelfhostCirArrayOp) -> u8 {
    match op {
        SelfhostCirArrayOp::Load => 22,
        SelfhostCirArrayOp::Store => 23,
        SelfhostCirArrayOp::Add => 24,
        SelfhostCirArrayOp::Subtract => 25,
    }
}

const fn array_from_tag(tag: u8) -> Option<SelfhostCirArrayOp> {
    match tag {
        22 => Some(SelfhostCirArrayOp::Load),
        23 => Some(SelfhostCirArrayOp::Store),
        24 => Some(SelfhostCirArrayOp::Add),
        25 => Some(SelfhostCirArrayOp::Subtract),
        _ => None,
    }
}

fn narrow_count(label: &str, count: usize) -> Result<u8, SelfhostCirError> {
    u8::try_from(count)
        .map_err(|_| SelfhostCirError::Invalid(format!("{label} exceed 255 entries")))
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u24(output: &mut Vec<u8>, value: u32) -> Result<(), SelfhostCirError> {
    if value > MAX_U24 {
        return Err(SelfhostCirError::Invalid(
            "24-bit CIR value is out of range".into(),
        ));
    }
    output.extend_from_slice(&value.to_le_bytes()[..3]);
    Ok(())
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn take(&mut self, cells: usize) -> Result<&'a [u8], SelfhostCirError> {
        let end = self
            .offset
            .checked_add(cells)
            .ok_or_else(|| self.error("record length overflow"))?;
        let result = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| self.error("unexpected end of file"))?;
        self.offset = end;
        Ok(result)
    }

    fn u8(&mut self) -> Result<u8, SelfhostCirError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SelfhostCirError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u24(&mut self) -> Result<u32, SelfhostCirError> {
        let bytes = self.take(3)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
    }

    fn error(&self, message: impl Into<String>) -> SelfhostCirError {
        SelfhostCirError::Decode {
            offset: self.offset,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_program() -> SelfhostCirProgram {
        SelfhostCirProgram::new(
            0x01_0203,
            7,
            vec![SelfhostCirFunction {
                id: 7,
                entry: 0x0201,
                frame_cells: 6,
                return_type: SelfhostCirReturnType::Void,
                parameters: vec![],
            }],
            vec![SelfhostCirContinuation {
                id: 0x0201,
                function: 7,
                instructions: vec![
                    SelfhostCirInstruction::Set {
                        destination: 0,
                        value: b'A',
                    },
                    SelfhostCirInstruction::Output { source: 0 },
                    SelfhostCirInstruction::Array {
                        op: SelfhostCirArrayOp::Load,
                        data: 1,
                        offset_low: 2,
                        offset_high: 3,
                        base: 0x010000,
                        cells: 0x0203,
                        storage: SelfhostCirStorage::Global,
                    },
                    SelfhostCirInstruction::OffsetAddScaled {
                        low: 2,
                        high: 3,
                        source: 4,
                        amount: 0x1234,
                    },
                ],
                terminator: SelfhostCirTerminator::Halt,
            }],
        )
        .unwrap()
    }

    #[test]
    fn binary_round_trip_preserves_flat_cir() {
        let program = sample_program();
        let encoded = program.encode().unwrap();
        assert_eq!(&encoded[..MAGIC.len()], MAGIC);
        assert_eq!(SelfhostCirProgram::decode(&encoded).unwrap(), program);
    }

    #[test]
    fn decoder_rejects_truncation_at_every_byte() {
        let encoded = sample_program().encode().unwrap();
        for length in 0..encoded.len() {
            assert!(SelfhostCirProgram::decode(&encoded[..length]).is_err());
        }
    }

    #[test]
    fn validation_rejects_a_global_region_past_static_storage() {
        let mut program = sample_program();
        let SelfhostCirInstruction::Array { cells, .. } =
            &mut program.continuations[0].instructions[2]
        else {
            unreachable!()
        };
        *cells = 0x0204;
        assert!(program.validate().is_err());
    }
}
