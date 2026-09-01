//! Direct execution of validated continuation IR.
//!
//! This VM is primarily a development tool. It preserves the cell and frame
//! semantics of the Brainfuck ABI without first expanding them into a very
//! large Brainfuck program, and writes output through a bounded buffer.

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::{
    Address, AggregateRegion, Continuation, ContinuationId, ContinuationProgram, FrameAggregateId,
    FrameInstruction, FunctionDescriptor, FunctionId, GlobalDescriptor, GlobalId, LogicalOffset,
    ParameterLocation, Terminator, ValueOperand, ValueType,
};

const OUTPUT_BUFFER_CELLS: usize = 64 * 1024;
const PROGRESS_CHECK_MASK: u64 = (1 << 20) - 1;

/// Configuration for direct continuation-IR execution.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinuationRunOptions {
    /// Minimum wall-clock interval between progress callbacks.
    pub progress_interval: Option<Duration>,
}

/// Cumulative counters from a direct continuation-IR run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContinuationRunStats {
    pub executed_continuations: u64,
    pub executed_frame_instructions: u64,
    pub loop_iterations: u64,
    pub calls: u64,
    pub returns: u64,
    pub array_loads: u64,
    pub array_stores: u64,
    pub aggregate_loads: u64,
    pub aggregate_stores: u64,
    pub input_operations: u64,
    pub output_bytes: u64,
    pub max_call_depth: usize,
    pub aborted: bool,
    pub final_continuation: Option<ContinuationId>,
    pub final_function_stack: Vec<FunctionId>,
    continuation_counts: Vec<u64>,
}

impl ContinuationRunStats {
    /// Continuations ordered from most to least frequently dispatched.
    pub fn hottest_continuations(&self, limit: usize) -> Vec<(ContinuationId, u64)> {
        let mut counts = self
            .continuation_counts
            .iter()
            .enumerate()
            .filter_map(|(id, &count)| {
                (count != 0)
                    .then(|| ContinuationId::new(id as u16).map(|id| (id, count)))
                    .flatten()
            })
            .collect::<Vec<_>>();
        counts.sort_unstable_by_key(|&(id, count)| (std::cmp::Reverse(count), id));
        counts.truncate(limit);
        counts
    }
}

/// A cheap snapshot passed to periodic progress observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationRunProgress {
    pub elapsed: Duration,
    pub current_continuation: ContinuationId,
    pub executed_continuations: u64,
    pub executed_frame_instructions: u64,
    pub loop_iterations: u64,
    pub calls: u64,
    pub output_bytes: u64,
    pub call_depth: usize,
    pub max_call_depth: usize,
}

/// A runtime failure while directly executing continuation IR.
#[derive(Debug)]
pub enum ContinuationVmError {
    Io(io::Error),
    Runtime(String),
}

impl fmt::Display for ContinuationVmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Runtime(message) => f.write_str(message),
        }
    }
}

impl Error for ContinuationVmError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Runtime(_) => None,
        }
    }
}

impl From<io::Error> for ContinuationVmError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Execute validated continuation IR with streaming input and output.
pub fn run_continuations_with_io<R, W, F>(
    program: &ContinuationProgram,
    input: &mut R,
    output: &mut W,
    options: ContinuationRunOptions,
    mut progress: F,
) -> Result<ContinuationRunStats, ContinuationVmError>
where
    R: Read,
    W: Write,
    F: FnMut(ContinuationRunProgress),
{
    let mut machine = Machine::new(program, input, output, options)?;
    machine.run(&mut progress)?;
    machine.flush_output()?;
    Ok(machine.stats)
}

#[derive(Debug)]
enum RuntimeValue {
    Cell(u8),
    Aggregate(Vec<u8>),
}

#[derive(Debug)]
struct Frame {
    function: FunctionId,
    slots: Vec<u8>,
    aggregates: Vec<Option<Vec<u8>>>,
    outbox: Vec<u8>,
    abi_value: u8,
    return_to: Option<ContinuationId>,
}

#[derive(Debug)]
enum OwnedOperand {
    Cell(u8),
    Aggregate(Vec<u8>),
}

struct Machine<'a, R, W> {
    input: &'a mut R,
    output: &'a mut W,
    output_buffer: Vec<u8>,
    globals: Vec<Option<RuntimeValue>>,
    functions: Vec<Option<&'a FunctionDescriptor>>,
    continuations: Vec<Option<&'a Continuation>>,
    stack: Vec<Frame>,
    current: ContinuationId,
    stats: ContinuationRunStats,
    started: Instant,
    last_progress: Instant,
    progress_interval: Option<Duration>,
}

impl<'a, R: Read, W: Write> Machine<'a, R, W> {
    fn new(
        program: &'a ContinuationProgram,
        input: &'a mut R,
        output: &'a mut W,
        options: ContinuationRunOptions,
    ) -> Result<Self, ContinuationVmError> {
        let functions_len = program
            .functions()
            .iter()
            .map(|function| function.id().index())
            .max()
            .map_or(0, |index| index + 1);
        let mut functions = vec![None; functions_len];
        for function in program.functions() {
            functions[function.id().index()] = Some(function);
        }

        let continuations_len = program
            .continuations()
            .iter()
            .map(|continuation| usize::from(continuation.id().get()))
            .max()
            .map_or(1, |index| index + 1);
        let mut continuations = vec![None; continuations_len];
        for continuation in program.continuations() {
            continuations[usize::from(continuation.id().get())] = Some(continuation);
        }

        let globals_len = program
            .globals()
            .iter()
            .map(|global| global.id().index())
            .max()
            .map_or(0, |index| index + 1);
        let mut globals = (0..globals_len).map(|_| None).collect::<Vec<_>>();
        for global in program.globals() {
            globals[global.id().index()] = Some(initial_global(*global));
        }

        let main = function_at(&functions, program.main())?;
        let current = main.entry();
        let stack = vec![new_frame(main, None)];
        let started = Instant::now();
        Ok(Self {
            input,
            output,
            output_buffer: Vec::with_capacity(OUTPUT_BUFFER_CELLS),
            globals,
            functions,
            continuations,
            stack,
            current,
            stats: ContinuationRunStats {
                max_call_depth: 1,
                continuation_counts: vec![0; continuations_len],
                ..ContinuationRunStats::default()
            },
            started,
            last_progress: started,
            progress_interval: options.progress_interval,
        })
    }

    fn run<F>(&mut self, progress: &mut F) -> Result<(), ContinuationVmError>
    where
        F: FnMut(ContinuationRunProgress),
    {
        loop {
            let continuation = self.continuation(self.current)?;
            let function = self.current_frame()?.function;
            if continuation.function() != function {
                return Err(runtime(format!(
                    "continuation {} belongs to function {}, but frame belongs to {}",
                    continuation.id().get(),
                    continuation.function().index(),
                    function.index()
                )));
            }
            self.stats.executed_continuations += 1;
            self.stats.continuation_counts[usize::from(self.current.get())] += 1;
            self.maybe_progress(progress, false);

            for instruction in continuation.body() {
                self.execute_instruction(instruction, progress)?;
            }
            if !self.execute_terminator(continuation.terminator())? {
                return Ok(());
            }
        }
    }

    fn execute_instruction<F>(
        &mut self,
        instruction: &FrameInstruction,
        progress: &mut F,
    ) -> Result<(), ContinuationVmError>
    where
        F: FnMut(ContinuationRunProgress),
    {
        self.stats.executed_frame_instructions += 1;
        self.maybe_progress(progress, false);
        match instruction {
            FrameInstruction::Set { dst, value } => self.write_address(*dst, *value)?,
            FrameInstruction::AddConst { dst, value } => {
                let result = self.read_address(*dst)?.wrapping_add(*value);
                self.write_address(*dst, result)?;
            }
            FrameInstruction::Copy { src, dst } => {
                let value = self.read_address(*src)?;
                self.write_address(*dst, value)?;
            }
            FrameInstruction::Transfer { src, targets } => {
                let value = self.read_address(*src)?;
                for target in targets {
                    let result = self
                        .read_address(target.dst)?
                        .wrapping_add(value.wrapping_mul(target.factor));
                    self.write_address(target.dst, result)?;
                }
                self.write_address(*src, 0)?;
            }
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                let value = self.read_region(*src, 0, *cells)?;
                self.write_region(*dst, 0, &value)?;
            }
            FrameInstruction::Input { dst } => {
                let mut byte = [0];
                let value = match self.input.read(&mut byte)? {
                    0 => 0,
                    _ => byte[0],
                };
                self.stats.input_operations += 1;
                self.write_address(*dst, value)?;
            }
            FrameInstruction::Output { src } => {
                let value = self.read_address(*src)?;
                self.output_buffer.push(value);
                self.stats.output_bytes += 1;
                if self.output_buffer.len() == OUTPUT_BUFFER_CELLS {
                    self.flush_output()?;
                }
            }
            FrameInstruction::Loop { condition, body } => {
                while self.read_address(*condition)? != 0 {
                    self.stats.loop_iterations += 1;
                    for instruction in body {
                        self.execute_instruction(instruction, progress)?;
                    }
                }
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                let condition_value = self.read_address(*condition)?;
                self.write_address(*condition, 0)?;
                let body = if condition_value != 0 {
                    then_body
                } else {
                    else_body
                };
                for instruction in body {
                    self.execute_instruction(instruction, progress)?;
                }
            }
        }
        Ok(())
    }

    /// Return true to continue dispatching.
    fn execute_terminator(&mut self, terminator: &Terminator) -> Result<bool, ContinuationVmError> {
        match terminator {
            Terminator::Goto { target } => self.current = *target,
            Terminator::Branch {
                condition,
                then_target,
                else_target,
            } => {
                let condition_value = self.read_address(*condition)?;
                self.write_address(*condition, 0)?;
                self.current = if condition_value != 0 {
                    *then_target
                } else {
                    *else_target
                };
            }
            Terminator::Call {
                callee,
                arguments,
                return_to,
            } => {
                let arguments = arguments
                    .iter()
                    .map(|operand| self.read_operand(*operand))
                    .collect::<Result<Vec<_>, _>>()?;
                let function = self.function(*callee)?;
                let mut frame = new_frame(function, Some(*return_to));
                for (argument, parameter) in
                    arguments.into_iter().zip(function.parameter_locations())
                {
                    match (argument, parameter) {
                        (OwnedOperand::Cell(value), ParameterLocation::Cell(slot)) => {
                            frame.slots[slot.index()] = value;
                        }
                        (
                            OwnedOperand::Aggregate(value),
                            ParameterLocation::Array(id) | ParameterLocation::Aggregate(id),
                        ) => {
                            *frame_aggregate_mut(&mut frame, *id)? = value;
                        }
                        _ => unreachable!("validated call operand types match parameters"),
                    }
                }
                self.current = function.entry();
                self.stack.push(frame);
                self.stats.calls += 1;
                self.stats.max_call_depth = self.stats.max_call_depth.max(self.stack.len());
            }
            Terminator::Return { value } => {
                let value = value
                    .map(|operand| self.read_operand(operand))
                    .transpose()?;
                let frame = self
                    .stack
                    .pop()
                    .ok_or_else(|| runtime("return with an empty call stack"))?;
                let return_to = frame
                    .return_to
                    .ok_or_else(|| runtime("main function attempted to return"))?;
                let caller = self.current_frame_mut()?;
                match value {
                    Some(OwnedOperand::Cell(value)) => caller.abi_value = value,
                    Some(OwnedOperand::Aggregate(value)) => {
                        if caller.outbox.len() < value.len() {
                            return Err(runtime("aggregate return exceeds caller outbox"));
                        }
                        caller.outbox[..value.len()].copy_from_slice(&value);
                        caller.abi_value = 0;
                    }
                    None => caller.abi_value = 0,
                }
                self.current = return_to;
                self.stats.returns += 1;
            }
            Terminator::ArrayLoad {
                array,
                index,
                destination,
                return_to,
            } => {
                let index = usize::from(self.read_address(*index)?);
                let value = self.read_region_cell(*array, index)?;
                self.write_address(*destination, value)?;
                self.current = *return_to;
                self.stats.array_loads += 1;
            }
            Terminator::ArrayStore {
                array,
                index,
                value,
                return_to,
            } => {
                let index = usize::from(self.read_address(*index)?);
                let value = self.read_address(*value)?;
                self.write_region_cell(*array, index, value)?;
                self.current = *return_to;
                self.stats.array_stores += 1;
            }
            Terminator::AggregateLoad {
                source,
                offset,
                destination,
                cells,
                return_to,
            } => {
                let start = self.read_logical_offset(*offset)?;
                let value = self.read_region(*source, start, *cells)?;
                self.write_operand(*destination, &value)?;
                self.advance_logical_offset(*offset, cells.saturating_sub(1))?;
                self.current = *return_to;
                self.stats.aggregate_loads += 1;
            }
            Terminator::AggregateStore {
                destination,
                offset,
                source,
                cells,
                return_to,
            } => {
                let start = self.read_logical_offset(*offset)?;
                let value = self.read_operand_cells(*source, *cells)?;
                self.write_region(*destination, start, &value)?;
                self.advance_logical_offset(*offset, cells.saturating_sub(1))?;
                self.current = *return_to;
                self.stats.aggregate_stores += 1;
            }
            Terminator::Abort => {
                self.stats.aborted = true;
                self.record_termination();
                return Ok(false);
            }
            Terminator::Halt => {
                self.record_termination();
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn record_termination(&mut self) {
        self.stats.final_continuation = Some(self.current);
        self.stats.final_function_stack = self.stack.iter().map(|frame| frame.function).collect();
    }

    fn read_address(&self, address: Address) -> Result<u8, ContinuationVmError> {
        match address {
            Address::Frame(slot) => self
                .current_frame()?
                .slots
                .get(slot.index())
                .copied()
                .ok_or_else(|| runtime(format!("frame slot {} is out of bounds", slot.index()))),
            Address::Global(id) => match self.global(id)? {
                RuntimeValue::Cell(value) => Ok(*value),
                RuntimeValue::Aggregate(_) => Err(runtime("aggregate global used as a cell")),
            },
            Address::ArrayElement { array, index } => self.read_region_cell(array, index),
            Address::AbiValue => Ok(self.current_frame()?.abi_value),
        }
    }

    fn write_address(&mut self, address: Address, value: u8) -> Result<(), ContinuationVmError> {
        match address {
            Address::Frame(slot) => {
                let cell = self
                    .current_frame_mut()?
                    .slots
                    .get_mut(slot.index())
                    .ok_or_else(|| {
                        runtime(format!("frame slot {} is out of bounds", slot.index()))
                    })?;
                *cell = value;
            }
            Address::Global(id) => match self.global_mut(id)? {
                RuntimeValue::Cell(cell) => *cell = value,
                RuntimeValue::Aggregate(_) => {
                    return Err(runtime("aggregate global used as a cell"));
                }
            },
            Address::ArrayElement { array, index } => {
                self.write_region_cell(array, index, value)?;
            }
            Address::AbiValue => self.current_frame_mut()?.abi_value = value,
        }
        Ok(())
    }

    fn read_operand(&self, operand: ValueOperand) -> Result<OwnedOperand, ContinuationVmError> {
        Ok(match operand {
            ValueOperand::Cell(address) => OwnedOperand::Cell(self.read_address(address)?),
            ValueOperand::Array(region) => {
                OwnedOperand::Aggregate(self.read_region(region, 0, self.region_len(region)?)?)
            }
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } => OwnedOperand::Aggregate(self.read_region(region, offset, cells)?),
        })
    }

    fn read_operand_cells(
        &self,
        operand: ValueOperand,
        cells: usize,
    ) -> Result<Vec<u8>, ContinuationVmError> {
        match self.read_operand(operand)? {
            OwnedOperand::Cell(value) if cells == 1 => Ok(vec![value]),
            OwnedOperand::Aggregate(value) if value.len() == cells => Ok(value),
            _ => Err(runtime("operand size does not match aggregate access")),
        }
    }

    fn write_operand(
        &mut self,
        operand: ValueOperand,
        value: &[u8],
    ) -> Result<(), ContinuationVmError> {
        match operand {
            ValueOperand::Cell(address) if value.len() == 1 => {
                self.write_address(address, value[0])
            }
            ValueOperand::Array(region) => self.write_region(region, 0, value),
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } if cells == value.len() => self.write_region(region, offset, value),
            _ => Err(runtime("destination size does not match aggregate access")),
        }
    }

    fn read_region(
        &self,
        region: AggregateRegion,
        offset: usize,
        cells: usize,
    ) -> Result<Vec<u8>, ContinuationVmError> {
        let end = offset
            .checked_add(cells)
            .ok_or_else(|| runtime("aggregate range overflows"))?;
        let value = self.region(region)?;
        value
            .get(offset..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| runtime(format!("aggregate range {offset}..{end} is out of bounds")))
    }

    fn write_region(
        &mut self,
        region: AggregateRegion,
        offset: usize,
        value: &[u8],
    ) -> Result<(), ContinuationVmError> {
        let end = offset
            .checked_add(value.len())
            .ok_or_else(|| runtime("aggregate range overflows"))?;
        self.region_mut(region)?
            .get_mut(offset..end)
            .ok_or_else(|| runtime(format!("aggregate range {offset}..{end} is out of bounds")))?
            .copy_from_slice(value);
        Ok(())
    }

    fn read_region_cell(
        &self,
        region: AggregateRegion,
        index: usize,
    ) -> Result<u8, ContinuationVmError> {
        self.region(region)?
            .get(index)
            .copied()
            .ok_or_else(|| runtime(format!("aggregate index {index} is out of bounds")))
    }

    fn write_region_cell(
        &mut self,
        region: AggregateRegion,
        index: usize,
        value: u8,
    ) -> Result<(), ContinuationVmError> {
        *self
            .region_mut(region)?
            .get_mut(index)
            .ok_or_else(|| runtime(format!("aggregate index {index} is out of bounds")))? = value;
        Ok(())
    }

    fn region(&self, region: AggregateRegion) -> Result<&[u8], ContinuationVmError> {
        match region {
            AggregateRegion::Frame(id) => Ok(frame_aggregate(self.current_frame()?, id)?),
            AggregateRegion::Global(id) => match self.global(id)? {
                RuntimeValue::Aggregate(value) => Ok(value),
                RuntimeValue::Cell(_) => Err(runtime("scalar global used as an aggregate")),
            },
            AggregateRegion::Outbox => Ok(&self.current_frame()?.outbox),
        }
    }

    fn region_mut(&mut self, region: AggregateRegion) -> Result<&mut [u8], ContinuationVmError> {
        match region {
            AggregateRegion::Frame(id) => Ok(frame_aggregate_mut(self.current_frame_mut()?, id)?),
            AggregateRegion::Global(id) => match self.global_mut(id)? {
                RuntimeValue::Aggregate(value) => Ok(value),
                RuntimeValue::Cell(_) => Err(runtime("scalar global used as an aggregate")),
            },
            AggregateRegion::Outbox => Ok(&mut self.current_frame_mut()?.outbox),
        }
    }

    fn region_len(&self, region: AggregateRegion) -> Result<usize, ContinuationVmError> {
        Ok(self.region(region)?.len())
    }

    fn read_logical_offset(&self, offset: LogicalOffset) -> Result<usize, ContinuationVmError> {
        Ok(usize::from(self.read_address(offset.low)?)
            | (usize::from(self.read_address(offset.high)?) << 8))
    }

    fn advance_logical_offset(
        &mut self,
        offset: LogicalOffset,
        amount: usize,
    ) -> Result<(), ContinuationVmError> {
        let advanced = (self.read_logical_offset(offset)? + amount) & 0xffff;
        self.write_address(offset.low, advanced as u8)?;
        self.write_address(offset.high, (advanced >> 8) as u8)
    }

    fn function(&self, id: FunctionId) -> Result<&'a FunctionDescriptor, ContinuationVmError> {
        function_at(&self.functions, id)
    }

    fn continuation(&self, id: ContinuationId) -> Result<&'a Continuation, ContinuationVmError> {
        self.continuations
            .get(usize::from(id.get()))
            .and_then(Option::as_ref)
            .copied()
            .ok_or_else(|| runtime(format!("unknown continuation {}", id.get())))
    }

    fn global(&self, id: GlobalId) -> Result<&RuntimeValue, ContinuationVmError> {
        self.globals
            .get(id.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| runtime(format!("unknown global {}", id.index())))
    }

    fn global_mut(&mut self, id: GlobalId) -> Result<&mut RuntimeValue, ContinuationVmError> {
        self.globals
            .get_mut(id.index())
            .and_then(Option::as_mut)
            .ok_or_else(|| runtime(format!("unknown global {}", id.index())))
    }

    fn current_frame(&self) -> Result<&Frame, ContinuationVmError> {
        self.stack
            .last()
            .ok_or_else(|| runtime("execution has no current frame"))
    }

    fn current_frame_mut(&mut self) -> Result<&mut Frame, ContinuationVmError> {
        self.stack
            .last_mut()
            .ok_or_else(|| runtime("execution has no current frame"))
    }

    fn flush_output(&mut self) -> Result<(), ContinuationVmError> {
        if !self.output_buffer.is_empty() {
            self.output.write_all(&self.output_buffer)?;
            self.output_buffer.clear();
        }
        Ok(())
    }

    fn maybe_progress<F>(&mut self, progress: &mut F, force: bool)
    where
        F: FnMut(ContinuationRunProgress),
    {
        let Some(interval) = self.progress_interval else {
            return;
        };
        let work = self.stats.executed_continuations + self.stats.executed_frame_instructions;
        if !force && work & PROGRESS_CHECK_MASK != 0 {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.last_progress) < interval {
            return;
        }
        self.last_progress = now;
        progress(ContinuationRunProgress {
            elapsed: now.duration_since(self.started),
            current_continuation: self.current,
            executed_continuations: self.stats.executed_continuations,
            executed_frame_instructions: self.stats.executed_frame_instructions,
            loop_iterations: self.stats.loop_iterations,
            calls: self.stats.calls,
            output_bytes: self.stats.output_bytes,
            call_depth: self.stack.len(),
            max_call_depth: self.stats.max_call_depth,
        });
    }
}

fn initial_global(global: GlobalDescriptor) -> RuntimeValue {
    match global.value_type() {
        ValueType::Cell => RuntimeValue::Cell(0),
        ValueType::Array(cells) | ValueType::Aggregate { cells } => {
            RuntimeValue::Aggregate(vec![0; cells])
        }
        ValueType::Void => unreachable!("validated globals are not void"),
    }
}

fn new_frame(function: &FunctionDescriptor, return_to: Option<ContinuationId>) -> Frame {
    let aggregates_len = function
        .frame_aggregates()
        .iter()
        .map(|aggregate| aggregate.id().index())
        .max()
        .map_or(0, |index| index + 1);
    let mut aggregates = (0..aggregates_len).map(|_| None).collect::<Vec<_>>();
    for aggregate in function.frame_aggregates() {
        aggregates[aggregate.id().index()] = Some(vec![0; aggregate.cells()]);
    }
    Frame {
        function: function.id(),
        slots: vec![0; function.frame_slots()],
        aggregates,
        outbox: vec![0; function.outbox_cells()],
        abi_value: 0,
        return_to,
    }
}

fn frame_aggregate(frame: &Frame, id: FrameAggregateId) -> Result<&[u8], ContinuationVmError> {
    frame
        .aggregates
        .get(id.index())
        .and_then(Option::as_deref)
        .ok_or_else(|| runtime(format!("unknown frame aggregate {}", id.index())))
}

fn frame_aggregate_mut(
    frame: &mut Frame,
    id: FrameAggregateId,
) -> Result<&mut Vec<u8>, ContinuationVmError> {
    frame
        .aggregates
        .get_mut(id.index())
        .and_then(Option::as_mut)
        .ok_or_else(|| runtime(format!("unknown frame aggregate {}", id.index())))
}

fn function_at<'a>(
    functions: &[Option<&'a FunctionDescriptor>],
    id: FunctionId,
) -> Result<&'a FunctionDescriptor, ContinuationVmError> {
    functions
        .get(id.index())
        .and_then(Option::as_ref)
        .copied()
        .ok_or_else(|| runtime(format!("unknown function {}", id.index())))
}

fn runtime(message: impl Into<String>) -> ContinuationVmError {
    ContinuationVmError::Runtime(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execute(source: &str, input: &[u8]) -> (Vec<u8>, ContinuationRunStats) {
        let program = crate::lower_source(source).unwrap();
        let mut input = input;
        let mut output = Vec::new();
        let stats = run_continuations_with_io(
            &program,
            &mut input,
            &mut output,
            ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        (output, stats)
    }

    #[test]
    fn executes_cells_calls_and_control_flow() {
        let source = "cell twice(cell value) { return value + value; } void main() { cell value = input(); while (value != 0) { output(twice(value)); value = value - 1; } }";
        let (output, stats) = execute(source, &[3]);
        assert_eq!(output, [6, 4, 2]);
        assert_eq!(stats.calls, 3);
        assert_eq!(stats.returns, 3);
        assert!(stats.executed_frame_instructions > 0);
        assert!(stats.max_call_depth >= 2);
    }

    #[test]
    fn executes_dynamic_aggregate_access_and_return() {
        let source = "struct Pair { cell a; cell b; } Pair choose(Pair[2] values, cell index) { return values[index]; } void main() { Pair[2] values; values[1].a = 'O'; values[1].b = 'K'; Pair result = choose(values, 1); output(result.a); output(result.b); }";
        let (output, stats) = execute(source, &[]);
        assert_eq!(output, b"OK");
        assert!(stats.aggregate_loads > 0);
        assert_eq!(stats.calls, 1);
    }

    #[test]
    fn eof_input_is_zero() {
        let (output, stats) = execute("void main() { output(input()); output(input()); }", &[]);
        assert_eq!(output, [0, 0]);
        assert_eq!(stats.input_operations, 2);
    }
}
