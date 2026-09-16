//! Adapter from the stage-2 selfhost's flat CIR to the typed Rust ABI IR.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use crate::continuation_ir::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationIrError,
    ContinuationProgram, FrameAggregateDescriptor, FrameAggregateId, FrameInstruction,
    FrameTransferTarget, FunctionDescriptor, FunctionId, GlobalDescriptor, GlobalId, LogicalOffset,
    ParameterLocation, Terminator, ValueOperand, ValueType,
};
use crate::continuation_optimizer::{
    ContinuationOptimizationOptions, ContinuationOptimizationStats,
};
use crate::selfhost_cir::{
    SelfhostCirArrayOp, SelfhostCirBinaryOp, SelfhostCirContinuation, SelfhostCirGlobalOp,
    SelfhostCirInstruction, SelfhostCirProgram, SelfhostCirReturnType, SelfhostCirStorage,
    SelfhostCirTerminator, SelfhostCirUnaryOp,
};

const FLAT_FRAME: FrameAggregateId = FrameAggregateId::new(0);
// Arithmetic expansion uses these cells after the imported flat frame.
const ADAPTER_SCRATCH_CELLS: usize = 5;

#[derive(Debug)]
pub enum SelfhostCirLoweringError {
    Invalid(String),
    ContinuationIdsExhausted,
    ContinuationIr(ContinuationIrError),
}

impl fmt::Display for SelfhostCirLoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "cannot lower selfhost CIR: {message}"),
            Self::ContinuationIdsExhausted => {
                write!(f, "cannot lower selfhost CIR: continuation IDs exhausted")
            }
            Self::ContinuationIr(error) => error.fmt(f),
        }
    }
}

impl Error for SelfhostCirLoweringError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ContinuationIr(error) => Some(error),
            Self::Invalid(_) | Self::ContinuationIdsExhausted => None,
        }
    }
}

impl From<ContinuationIrError> for SelfhostCirLoweringError {
    fn from(error: ContinuationIrError) -> Self {
        Self::ContinuationIr(error)
    }
}

/// Convert flat selfhost CIR into the existing chunked continuation ABI IR.
///
/// The imported frame remains one aggregate so every flat slot has a single
/// physical identity, including slots reused for different temporaries. Large
/// global regions touched through a dynamic portal become independent static
/// aggregates; only statically referenced cells outside those regions become
/// scalar globals.
pub fn lower_selfhost_cir(
    source: &SelfhostCirProgram,
) -> Result<ContinuationProgram, SelfhostCirLoweringError> {
    lower_selfhost_cir_with_options(source, ContinuationOptimizationOptions::default())
        .map(|(program, _)| program)
}

/// Convert flat selfhost CIR with explicitly selected continuation optimizations.
pub fn lower_selfhost_cir_with_options(
    source: &SelfhostCirProgram,
    options: ContinuationOptimizationOptions,
) -> Result<(ContinuationProgram, ContinuationOptimizationStats), SelfhostCirLoweringError> {
    source
        .validate()
        .map_err(|error| SelfhostCirLoweringError::Invalid(error.to_string()))?;
    let globals = GlobalPlan::new(source)?;
    let mut ids = HiddenIds::new(source)?;
    let maximum_outbox = source
        .functions
        .iter()
        .filter_map(|function| match function.return_type {
            SelfhostCirReturnType::Aggregate(cells) => Some(usize::from(cells)),
            SelfhostCirReturnType::Void | SelfhostCirReturnType::Cell => None,
        })
        .max()
        .unwrap_or(0);

    let functions = source
        .functions
        .iter()
        .map(|function| {
            let function_id = FunctionId::new(usize::from(function.id));
            let parameters = function
                .parameters
                .iter()
                .flat_map(|parameter| {
                    (0..parameter.cells).map(move |index| ParameterLocation::AggregateElement {
                        aggregate: FLAT_FRAME,
                        index: usize::from(parameter.destination) + usize::from(index),
                    })
                })
                .collect();
            let frame_cells = usize::from(function.frame_cells) + ADAPTER_SCRATCH_CELLS;
            Ok(FunctionDescriptor::new_aggregates(
                function_id,
                parameters,
                0,
                vec![FrameAggregateDescriptor::new(FLAT_FRAME, frame_cells)],
                maximum_outbox,
                match function.return_type {
                    SelfhostCirReturnType::Void => ValueType::Void,
                    SelfhostCirReturnType::Cell => ValueType::Cell,
                    SelfhostCirReturnType::Aggregate(cells) => ValueType::Aggregate {
                        cells: usize::from(cells),
                    },
                },
                continuation_id(function.entry)?,
            ))
        })
        .collect::<Result<Vec<_>, SelfhostCirLoweringError>>()?;

    let frame_sizes = source
        .functions
        .iter()
        .map(|function| (function.id, usize::from(function.frame_cells)))
        .collect::<HashMap<_, _>>();
    let mut continuations = Vec::new();
    for continuation in &source.continuations {
        lower_continuation(
            continuation,
            *frame_sizes
                .get(&continuation.function)
                .expect("validated continuation owner"),
            &globals,
            &mut ids,
            &mut continuations,
        )?;
    }

    let order: HashMap<_, _> = continuations
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id(), i))
        .collect();
    let mut by_function: HashMap<_, Vec<_>> = HashMap::new();
    for continuation in continuations {
        by_function
            .entry(continuation.function())
            .or_default()
            .push(continuation);
    }
    let mut continuations = Vec::new();
    let functions = functions
        .into_iter()
        .map(|function| {
            let body = by_function.remove(&function.id()).unwrap_or_default();
            let (function, fused) = crate::frame_fusion::fuse_function(function, body);
            continuations.extend(fused);
            function
        })
        .collect();
    continuations.sort_by_key(|c| order[&c.id()]);
    let lowered = ContinuationProgram::new_with_globals(
        FunctionId::new(usize::from(source.main_function)),
        globals.descriptors,
        functions,
        continuations,
    )?;
    Ok(crate::continuation_optimizer::optimize_continuations_with_options(&lowered, options)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlobalRegion {
    base: u32,
    cells: u32,
    id: GlobalId,
}

#[derive(Debug)]
struct GlobalPlan {
    descriptors: Vec<GlobalDescriptor>,
    regions: Vec<GlobalRegion>,
    scalars: HashMap<u32, GlobalId>,
}

impl GlobalPlan {
    fn new(program: &SelfhostCirProgram) -> Result<Self, SelfhostCirLoweringError> {
        let mut ranges = BTreeSet::new();
        let mut scalar_addresses = BTreeSet::new();
        for continuation in &program.continuations {
            for instruction in &continuation.instructions {
                match *instruction {
                    SelfhostCirInstruction::Array {
                        base,
                        cells,
                        storage: SelfhostCirStorage::Global,
                        ..
                    } => {
                        ranges.insert((base, cells));
                    }
                    SelfhostCirInstruction::Global { address, .. } => {
                        scalar_addresses.insert(address);
                    }
                    _ => {}
                }
            }
        }
        let ranges = ranges.into_iter().collect::<Vec<_>>();
        for (index, &(base, cells)) in ranges.iter().enumerate() {
            let end = base + cells;
            for &(other_base, other_cells) in &ranges[index + 1..] {
                let other_end = other_base + other_cells;
                if base < other_end && other_base < end {
                    return Err(SelfhostCirLoweringError::Invalid(format!(
                        "overlapping dynamic global regions {base}..{end} and {other_base}..{other_end}"
                    )));
                }
            }
        }

        let mut descriptors = Vec::new();
        let mut regions = Vec::new();
        for (base, cells) in ranges {
            let id = GlobalId::new(descriptors.len());
            descriptors.push(GlobalDescriptor::aggregate(id, cells as usize));
            regions.push(GlobalRegion { base, cells, id });
        }
        let mut scalars = HashMap::new();
        for address in scalar_addresses {
            if regions.iter().any(|region| region.contains(address)) {
                continue;
            }
            let id = GlobalId::new(descriptors.len());
            descriptors.push(GlobalDescriptor::cell(id));
            scalars.insert(address, id);
        }
        Ok(Self {
            descriptors,
            regions,
            scalars,
        })
    }

    fn address(&self, address: u32) -> Result<Address, SelfhostCirLoweringError> {
        if let Some(region) = self.regions.iter().find(|region| region.contains(address)) {
            return Ok(Address::ArrayElement {
                array: AggregateRegion::Global(region.id),
                index: (address - region.base) as usize,
            });
        }
        self.scalars
            .get(&address)
            .copied()
            .map(Address::Global)
            .ok_or_else(|| {
                SelfhostCirLoweringError::Invalid(format!(
                    "global address {address} has no imported storage"
                ))
            })
    }

    fn region(&self, base: u32, cells: u32) -> Result<AggregateRegion, SelfhostCirLoweringError> {
        self.regions
            .iter()
            .find(|region| region.base == base && region.cells == cells)
            .map(|region| AggregateRegion::Global(region.id))
            .ok_or_else(|| {
                SelfhostCirLoweringError::Invalid(format!(
                    "unknown dynamic global region {base}..{}",
                    base + cells
                ))
            })
    }
}

impl GlobalRegion {
    fn contains(self, address: u32) -> bool {
        self.base <= address && address < self.base + self.cells
    }
}

struct HiddenIds {
    used: HashSet<u16>,
    next: u16,
}

impl HiddenIds {
    fn new(program: &SelfhostCirProgram) -> Result<Self, SelfhostCirLoweringError> {
        let used = program
            .continuations
            .iter()
            .map(|continuation| continuation.id)
            .collect::<HashSet<_>>();
        let next = used.iter().copied().max().unwrap_or(0).saturating_add(1);
        Ok(Self { used, next })
    }

    fn allocate(&mut self) -> Result<ContinuationId, SelfhostCirLoweringError> {
        let start = self.next.max(1);
        let mut candidate = start;
        loop {
            if self.used.insert(candidate) {
                self.next = candidate.wrapping_add(1).max(1);
                return continuation_id(candidate);
            }
            candidate = candidate.wrapping_add(1).max(1);
            if candidate == start {
                return Err(SelfhostCirLoweringError::ContinuationIdsExhausted);
            }
        }
    }
}

fn continuation_id(id: u16) -> Result<ContinuationId, SelfhostCirLoweringError> {
    ContinuationId::new(id)
        .ok_or_else(|| SelfhostCirLoweringError::Invalid("continuation ID zero is reserved".into()))
}

fn frame(index: usize) -> Address {
    Address::ArrayElement {
        array: AggregateRegion::Frame(FLAT_FRAME),
        index,
    }
}

fn lower_continuation(
    source: &SelfhostCirContinuation,
    frame_cells: usize,
    globals: &GlobalPlan,
    ids: &mut HiddenIds,
    output: &mut Vec<Continuation>,
) -> Result<(), SelfhostCirLoweringError> {
    let function = FunctionId::new(usize::from(source.function));
    let mut current_id = continuation_id(source.id)?;
    let mut body = Vec::new();
    let scratch = (0..ADAPTER_SCRATCH_CELLS)
        .map(|index| frame(frame_cells + index))
        .collect::<Vec<_>>();

    for instruction in &source.instructions {
        let SelfhostCirInstruction::Array {
            op,
            data,
            offset_low,
            offset_high,
            base,
            cells,
            storage,
        } = *instruction
        else {
            lower_plain_instruction(instruction, globals, &scratch, &mut body)?;
            continue;
        };

        let region = match storage {
            SelfhostCirStorage::Frame => {
                add_u16_constant(
                    frame(usize::from(offset_low)),
                    frame(usize::from(offset_high)),
                    scratch[0],
                    base as u16,
                    &mut body,
                );
                AggregateRegion::Frame(FLAT_FRAME)
            }
            SelfhostCirStorage::Global => globals.region(base, cells)?,
        };
        let offset = LogicalOffset::new(
            frame(usize::from(offset_low)),
            frame(usize::from(offset_high)),
        );
        let data_address = frame(usize::from(data));

        match op {
            SelfhostCirArrayOp::Load => {
                let resume = ids.allocate()?;
                output.push(Continuation::new(
                    current_id,
                    function,
                    body,
                    Terminator::AggregateLoad {
                        source: region,
                        offset,
                        destination: ValueOperand::Cell(data_address),
                        cells: 1,
                        return_to: resume,
                    },
                ));
                current_id = resume;
                body = clear_offsets(offset);
            }
            SelfhostCirArrayOp::Store => {
                let resume = ids.allocate()?;
                output.push(Continuation::new(
                    current_id,
                    function,
                    body,
                    Terminator::AggregateStore {
                        destination: region,
                        offset,
                        source: ValueOperand::Cell(data_address),
                        cells: 1,
                        return_to: resume,
                    },
                ));
                current_id = resume;
                body = clear_offsets(offset);
                body.push(FrameInstruction::Set {
                    dst: data_address,
                    value: 0,
                });
            }
            SelfhostCirArrayOp::Add | SelfhostCirArrayOp::Subtract => {
                let arithmetic = ids.allocate()?;
                let resume = ids.allocate()?;
                output.push(Continuation::new(
                    current_id,
                    function,
                    body,
                    Terminator::AggregateLoad {
                        source: region,
                        offset,
                        destination: ValueOperand::Cell(scratch[1]),
                        cells: 1,
                        return_to: arithmetic,
                    },
                ));
                let arithmetic_body = vec![FrameInstruction::Transfer {
                    src: data_address,
                    targets: vec![FrameTransferTarget {
                        dst: scratch[1],
                        factor: if op == SelfhostCirArrayOp::Add {
                            1
                        } else {
                            255
                        },
                    }],
                }];
                output.push(Continuation::new(
                    arithmetic,
                    function,
                    arithmetic_body,
                    Terminator::AggregateStore {
                        destination: region,
                        offset,
                        source: ValueOperand::Cell(scratch[1]),
                        cells: 1,
                        return_to: resume,
                    },
                ));
                current_id = resume;
                body = clear_offsets(offset);
                body.push(FrameInstruction::Set {
                    dst: scratch[1],
                    value: 0,
                });
            }
        }
    }
    output.push(Continuation::new(
        current_id,
        function,
        body,
        lower_terminator(&source.terminator)?,
    ));
    Ok(())
}

fn clear_offsets(offset: LogicalOffset) -> Vec<FrameInstruction> {
    vec![
        FrameInstruction::Set {
            dst: offset.low,
            value: 0,
        },
        FrameInstruction::Set {
            dst: offset.high,
            value: 0,
        },
    ]
}

fn lower_plain_instruction(
    instruction: &SelfhostCirInstruction,
    globals: &GlobalPlan,
    scratch: &[Address],
    body: &mut Vec<FrameInstruction>,
) -> Result<(), SelfhostCirLoweringError> {
    match *instruction {
        SelfhostCirInstruction::Set { destination, value } => body.push(FrameInstruction::Set {
            dst: frame(usize::from(destination)),
            value,
        }),
        SelfhostCirInstruction::Copy {
            destination,
            source,
        } => body.push(FrameInstruction::Copy {
            src: frame(usize::from(source)),
            dst: frame(usize::from(destination)),
        }),
        SelfhostCirInstruction::CopyAbiValue { destination } => {
            body.push(FrameInstruction::Copy {
                src: Address::AbiValue,
                dst: frame(usize::from(destination)),
            });
        }
        SelfhostCirInstruction::Input { destination } => body.push(FrameInstruction::Input {
            dst: frame(usize::from(destination)),
        }),
        SelfhostCirInstruction::Output { source } => body.push(FrameInstruction::Output {
            src: frame(usize::from(source)),
        }),
        SelfhostCirInstruction::Unary { op, destination } => {
            lower_unary(op, frame(usize::from(destination)), scratch, body);
        }
        SelfhostCirInstruction::Binary {
            op,
            destination,
            source,
        } => lower_binary(
            op,
            frame(usize::from(destination)),
            frame(usize::from(source)),
            scratch,
            body,
        ),
        SelfhostCirInstruction::Global { op, data, address } => {
            let global = globals.address(address)?;
            match op {
                SelfhostCirGlobalOp::Set => body.push(FrameInstruction::Set {
                    dst: global,
                    value: data,
                }),
                SelfhostCirGlobalOp::Copy => {
                    let data = frame(usize::from(data));
                    body.push(FrameInstruction::Copy {
                        src: global,
                        dst: data,
                    });
                }
                SelfhostCirGlobalOp::Store => {
                    let data = frame(usize::from(data));
                    body.push(FrameInstruction::Set {
                        dst: global,
                        value: 0,
                    });
                    transfer(data, global, 1, body);
                }
                SelfhostCirGlobalOp::Add => {
                    transfer(frame(usize::from(data)), global, 1, body);
                }
                SelfhostCirGlobalOp::Subtract => {
                    transfer(frame(usize::from(data)), global, 255, body);
                }
            }
        }
        SelfhostCirInstruction::CopyOutbox { destination, index } => {
            body.push(FrameInstruction::Copy {
                src: Address::ArrayElement {
                    array: AggregateRegion::Outbox,
                    index: usize::from(index),
                },
                dst: frame(usize::from(destination)),
            });
        }
        SelfhostCirInstruction::OffsetAddScaled {
            low,
            high,
            source,
            amount,
        } => {
            let low = frame(usize::from(low));
            let high = frame(usize::from(high));
            let source = frame(usize::from(source));
            let mut loop_body = vec![FrameInstruction::AddConst {
                dst: source,
                value: 255,
            }];
            add_u16_constant(low, high, scratch[0], amount, &mut loop_body);
            body.push(FrameInstruction::Loop {
                condition: source,
                body: loop_body,
            });
        }
        SelfhostCirInstruction::OffsetAddConstant { low, high, amount } => add_u16_constant(
            frame(usize::from(low)),
            frame(usize::from(high)),
            scratch[0],
            amount,
            body,
        ),
        SelfhostCirInstruction::Array { .. } => unreachable!("array instructions split blocks"),
    }
    Ok(())
}

fn lower_unary(
    op: SelfhostCirUnaryOp,
    destination: Address,
    scratch: &[Address],
    body: &mut Vec<FrameInstruction>,
) {
    match op {
        SelfhostCirUnaryOp::Negate => {
            set(scratch[0], 0, body);
            transfer(destination, scratch[0], 255, body);
            transfer(scratch[0], destination, 1, body);
        }
        SelfhostCirUnaryOp::Not => boolean_from(destination, scratch[0], destination, 0, 1, body),
        SelfhostCirUnaryOp::Booleanize => {
            boolean_from(destination, scratch[0], destination, 1, 0, body);
        }
    }
}

fn lower_binary(
    op: SelfhostCirBinaryOp,
    destination: Address,
    source: Address,
    scratch: &[Address],
    body: &mut Vec<FrameInstruction>,
) {
    match op {
        SelfhostCirBinaryOp::Add => transfer(source, destination, 1, body),
        SelfhostCirBinaryOp::Subtract => transfer(source, destination, 255, body),
        SelfhostCirBinaryOp::Equal | SelfhostCirBinaryOp::NotEqual => {
            transfer(source, destination, 255, body);
            let (nonzero, zero) = if op == SelfhostCirBinaryOp::Equal {
                (0, 1)
            } else {
                (1, 0)
            };
            boolean_from(destination, scratch[0], destination, nonzero, zero, body);
        }
        SelfhostCirBinaryOp::Less
        | SelfhostCirBinaryOp::GreaterEqual
        | SelfhostCirBinaryOp::Greater
        | SelfhostCirBinaryOp::LessEqual => {
            let reverse = matches!(
                op,
                SelfhostCirBinaryOp::Greater | SelfhostCirBinaryOp::LessEqual
            );
            let invert = matches!(
                op,
                SelfhostCirBinaryOp::GreaterEqual | SelfhostCirBinaryOp::LessEqual
            );
            body.push(FrameInstruction::Compare {
                left: if reverse { source } else { destination },
                right: if reverse { destination } else { source },
                dst: destination,
                true_value: u8::from(!invert),
                false_value: u8::from(invert),
            });
        }
    }
}

fn set(dst: Address, value: u8, body: &mut Vec<FrameInstruction>) {
    body.push(FrameInstruction::Set { dst, value });
}

fn transfer(src: Address, dst: Address, factor: u8, body: &mut Vec<FrameInstruction>) {
    body.push(FrameInstruction::Transfer {
        src,
        targets: vec![FrameTransferTarget { dst, factor }],
    });
}

fn boolean_from(
    source: Address,
    test: Address,
    destination: Address,
    nonzero: u8,
    zero: u8,
    body: &mut Vec<FrameInstruction>,
) {
    body.push(FrameInstruction::Copy {
        src: source,
        dst: test,
    });
    body.push(FrameInstruction::Branch {
        condition: test,
        then_body: vec![FrameInstruction::Set {
            dst: destination,
            value: nonzero,
        }],
        else_body: vec![FrameInstruction::Set {
            dst: destination,
            value: zero,
        }],
    });
}

fn add_u16_constant(
    low: Address,
    high: Address,
    carry_test: Address,
    amount: u16,
    body: &mut Vec<FrameInstruction>,
) {
    let [amount_low, amount_high] = amount.to_le_bytes();
    if amount_high != 0 {
        body.push(FrameInstruction::AddConst {
            dst: high,
            value: amount_high,
        });
    }
    for _ in 0..amount_low {
        body.push(FrameInstruction::AddConst { dst: low, value: 1 });
        body.push(FrameInstruction::Copy {
            src: low,
            dst: carry_test,
        });
        body.push(FrameInstruction::Branch {
            condition: carry_test,
            then_body: vec![],
            else_body: vec![FrameInstruction::AddConst {
                dst: high,
                value: 1,
            }],
        });
    }
}

fn lower_terminator(
    terminator: &SelfhostCirTerminator,
) -> Result<Terminator, SelfhostCirLoweringError> {
    Ok(match terminator {
        SelfhostCirTerminator::Goto { target } => Terminator::Goto {
            target: continuation_id(*target)?,
        },
        SelfhostCirTerminator::Branch {
            condition,
            then_target,
            else_target,
        } => Terminator::Branch {
            condition: frame(usize::from(*condition)),
            then_target: continuation_id(*then_target)?,
            else_target: continuation_id(*else_target)?,
        },
        SelfhostCirTerminator::Call {
            callee,
            arguments,
            return_to,
        } => Terminator::Call {
            callee: FunctionId::new(usize::from(*callee)),
            arguments: arguments
                .iter()
                .flat_map(|argument| {
                    (0..argument.cells).map(move |index| {
                        ValueOperand::Cell(frame(usize::from(argument.source) + usize::from(index)))
                    })
                })
                .collect(),
            return_to: continuation_id(*return_to)?,
        },
        SelfhostCirTerminator::ReturnVoid => Terminator::Return { value: None },
        SelfhostCirTerminator::ReturnCell { source } => Terminator::Return {
            value: Some(ValueOperand::Cell(frame(usize::from(*source)))),
        },
        SelfhostCirTerminator::ReturnAggregate { source, cells } => Terminator::Return {
            value: Some(ValueOperand::Aggregate {
                region: AggregateRegion::Frame(FLAT_FRAME),
                offset: usize::from(*source),
                cells: usize::from(*cells),
            }),
        },
        SelfhostCirTerminator::Halt => Terminator::Halt,
        SelfhostCirTerminator::Abort => Terminator::Abort,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuation_vm::{ContinuationRunOptions, run_continuations_with_io};
    use crate::selfhost_cir::{SelfhostCirCallArgument, SelfhostCirFunction, SelfhostCirParameter};
    use std::time::Duration;

    fn run(program: &SelfhostCirProgram) -> Vec<u8> {
        let lowered = lower_selfhost_cir(program).unwrap();
        let mut output = Vec::new();
        run_continuations_with_io(
            &lowered,
            &mut &[][..],
            &mut output,
            ContinuationRunOptions {
                progress_interval: Some(Duration::ZERO),
                collect_transitions: false,
                ..ContinuationRunOptions::default()
            },
            |_| {},
        )
        .unwrap();
        output
    }

    #[test]
    fn flat_arithmetic_and_output_execute_through_typed_ir() {
        let program = SelfhostCirProgram::new(
            0,
            0,
            vec![SelfhostCirFunction {
                id: 0,
                entry: 1,
                frame_cells: 3,
                return_type: SelfhostCirReturnType::Void,
                parameters: vec![],
            }],
            vec![SelfhostCirContinuation {
                id: 1,
                function: 0,
                instructions: vec![
                    SelfhostCirInstruction::Set {
                        destination: 0,
                        value: 40,
                    },
                    SelfhostCirInstruction::Set {
                        destination: 1,
                        value: 25,
                    },
                    SelfhostCirInstruction::Binary {
                        op: SelfhostCirBinaryOp::Add,
                        destination: 0,
                        source: 1,
                    },
                    SelfhostCirInstruction::Output { source: 0 },
                ],
                terminator: SelfhostCirTerminator::Halt,
            }],
        )
        .unwrap();
        assert_eq!(run(&program), b"A");
    }

    #[test]
    fn dynamic_global_portal_and_flattened_call_parameters_execute() {
        let program = SelfhostCirProgram::new(
            300,
            0,
            vec![
                SelfhostCirFunction {
                    id: 0,
                    entry: 1,
                    frame_cells: 6,
                    return_type: SelfhostCirReturnType::Void,
                    parameters: vec![],
                },
                SelfhostCirFunction {
                    id: 1,
                    entry: 3,
                    frame_cells: 2,
                    return_type: SelfhostCirReturnType::Cell,
                    parameters: vec![SelfhostCirParameter {
                        destination: 0,
                        cells: 2,
                    }],
                },
            ],
            vec![
                SelfhostCirContinuation {
                    id: 1,
                    function: 0,
                    instructions: vec![
                        SelfhostCirInstruction::Set {
                            destination: 0,
                            value: b'A',
                        },
                        SelfhostCirInstruction::Set {
                            destination: 1,
                            value: b'B',
                        },
                    ],
                    terminator: SelfhostCirTerminator::Call {
                        callee: 1,
                        arguments: vec![SelfhostCirCallArgument {
                            source: 0,
                            destination: 0,
                            cells: 2,
                        }],
                        return_to: 2,
                    },
                },
                SelfhostCirContinuation {
                    id: 2,
                    function: 0,
                    instructions: vec![
                        SelfhostCirInstruction::CopyAbiValue { destination: 2 },
                        SelfhostCirInstruction::Set {
                            destination: 3,
                            value: 1,
                        },
                        SelfhostCirInstruction::Set {
                            destination: 4,
                            value: 0,
                        },
                        SelfhostCirInstruction::Array {
                            op: SelfhostCirArrayOp::Store,
                            data: 2,
                            offset_low: 3,
                            offset_high: 4,
                            base: 100,
                            cells: 4,
                            storage: SelfhostCirStorage::Global,
                        },
                        SelfhostCirInstruction::Set {
                            destination: 3,
                            value: 1,
                        },
                        SelfhostCirInstruction::Array {
                            op: SelfhostCirArrayOp::Load,
                            data: 5,
                            offset_low: 3,
                            offset_high: 4,
                            base: 100,
                            cells: 4,
                            storage: SelfhostCirStorage::Global,
                        },
                        SelfhostCirInstruction::Output { source: 5 },
                    ],
                    terminator: SelfhostCirTerminator::Halt,
                },
                SelfhostCirContinuation {
                    id: 3,
                    function: 1,
                    instructions: vec![SelfhostCirInstruction::Binary {
                        op: SelfhostCirBinaryOp::Add,
                        destination: 0,
                        source: 1,
                    }],
                    terminator: SelfhostCirTerminator::ReturnCell { source: 0 },
                },
            ],
        )
        .unwrap();
        assert_eq!(run(&program), &[b'A'.wrapping_add(b'B')]);
    }
}
