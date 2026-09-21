//! Reuse frame storage whose values are never needed at the same time.
//!
//! Lowering first assigns virtual slots. This pass builds an instruction-level
//! CFG (including structured loops), solves backwards liveness, and colors the
//! interference graph. Calls preserve the caller's live values across resume
//! edges; callees have independent frames. Equal-sized aggregate regions can share storage too. Partial writes preserve
//! the rest of a region; only complete overwrites kill its previous value.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::{
    Address, AggregateRegion, Continuation, ContinuationId, FrameAggregateDescriptor,
    FrameAggregateId, FrameInstruction, FrameSlot, FunctionDescriptor, ParameterLocation,
    Terminator, ValueOperand,
};

type Slots = BTreeSet<usize>;

#[derive(Default)]
struct Node {
    scalar_slots: usize,
    uses: Slots,
    defs: Slots,
    successors: Vec<usize>,
}

impl Node {
    fn new(function: &FunctionDescriptor) -> Self {
        Self {
            scalar_slots: function.frame_slots(),
            ..Self::default()
        }
    }
    fn region_slot(&self, region: AggregateRegion) -> Option<usize> {
        match region {
            AggregateRegion::Frame(id) => Some(self.scalar_slots + id.index()),
            _ => None,
        }
    }
    fn read_region(&mut self, region: AggregateRegion) {
        if let Some(slot) = self.region_slot(region) {
            self.uses.insert(slot);
        }
    }
    fn write_region(&mut self, region: AggregateRegion, complete: bool) {
        if let Some(slot) = self.region_slot(region) {
            self.defs.insert(slot);
            if !complete {
                self.uses.insert(slot);
            }
        }
    }
    fn read(&mut self, address: Address) {
        match address {
            Address::Frame(slot) => {
                self.uses.insert(slot.index());
            }
            Address::ArrayElement { array, .. } => self.read_region(array),
            _ => {}
        }
    }
    fn write(&mut self, address: Address) {
        match address {
            Address::Frame(slot) => {
                self.defs.insert(slot.index());
            }
            Address::ArrayElement { array, .. } => self.write_region(array, false),
            _ => {}
        }
    }
    fn operand(&mut self, operand: ValueOperand, write: bool, function: &FunctionDescriptor) {
        match operand {
            ValueOperand::Cell(address) => {
                if write {
                    self.write(address);
                } else {
                    self.read(address);
                }
            }
            ValueOperand::Array(region) => {
                if write {
                    self.write_region(region, true);
                } else {
                    self.read_region(region);
                }
            }
            ValueOperand::Aggregate {
                region,
                offset,
                cells,
            } => {
                if write {
                    self.write_region(region, full_region(region, offset, cells, function));
                } else {
                    self.read_region(region);
                }
            }
        }
    }
}

fn full_region(
    region: AggregateRegion,
    offset: usize,
    cells: usize,
    function: &FunctionDescriptor,
) -> bool {
    matches!(region, AggregateRegion::Frame(id) if offset == 0 && function.frame_aggregate(id).unwrap().cells() == cells)
}

pub(crate) fn allocate(
    function: FunctionDescriptor,
    continuations: Vec<Continuation>,
) -> (FunctionDescriptor, Vec<Continuation>) {
    let mut nodes = Vec::new();
    // Reserve continuation entry nodes so forward and back edges can be wired
    // before their instruction bodies have been built.
    let entries: HashMap<_, _> = continuations
        .iter()
        .enumerate()
        .map(|(index, continuation)| (continuation.id(), index))
        .collect();
    nodes.resize_with(continuations.len(), || Node::new(&function));
    for continuation in &continuations {
        let terminal = terminator_node(continuation.terminator(), &entries, &function);
        let terminal_id = nodes.len();
        nodes.push(terminal);
        let first = build_body(continuation.body(), terminal_id, &mut nodes, &function);
        nodes[entries[&continuation.id()]].successors.push(first);
    }
    let live = liveness(&nodes);
    let mut interference =
        vec![Slots::new(); function.frame_slots() + function.frame_aggregates().len()];
    for (index, node) in nodes.iter().enumerate() {
        // Backend operations must retain distinct operands even when an input
        // dies at this instruction (notably destructive transfers and portals).
        let operands: Slots = node.uses.union(&node.defs).copied().collect();
        clique(&mut interference, &operands);
        for successor in &node.successors {
            for &definition in &node.defs {
                for &other in &live[*successor] {
                    edge(&mut interference, definition, other);
                }
            }
        }
        // Covers values initialized by frame entry, before any explicit write.
        if index == entries[&function.entry()] {
            clique(&mut interference, &live[index]);
            let parameters: Slots = function
                .parameter_locations()
                .iter()
                .map(|parameter| match parameter {
                    ParameterLocation::Cell(slot) => slot.index(),
                    ParameterLocation::Array(id)
                    | ParameterLocation::Aggregate(id)
                    | ParameterLocation::AggregateElement { aggregate: id, .. } => {
                        function.frame_slots() + id.index()
                    }
                })
                .collect();
            clique(&mut interference, &parameters);
            for parameter in parameters {
                for &other in &live[index] {
                    edge(&mut interference, parameter, other);
                }
            }
        }
    }
    let mut classes = vec![None; function.frame_slots()];
    classes.extend(
        function
            .frame_aggregates()
            .iter()
            .map(|region| Some(region.cells())),
    );
    let mapping = color(&interference, &classes);
    let count = mapping[..function.frame_slots()]
        .iter()
        .max()
        .map_or(0, |slot| slot + 1);
    let mut aggregates = Vec::new();
    let mut aggregate_ids = HashMap::new();
    let aggregate_mapping: Vec<_> = function
        .frame_aggregates()
        .iter()
        .map(|region| {
            let color = mapping[function.frame_slots() + region.id().index()];
            *aggregate_ids.entry(color).or_insert_with(|| {
                let id = FrameAggregateId::new(aggregates.len());
                aggregates.push(FrameAggregateDescriptor::new(id, region.cells()));
                id
            })
        })
        .collect();
    let parameters = function
        .parameter_locations()
        .iter()
        .map(|parameter| match parameter {
            ParameterLocation::Cell(slot) => {
                ParameterLocation::Cell(FrameSlot::new(mapping[slot.index()]))
            }
            ParameterLocation::Array(id) => ParameterLocation::Array(aggregate_mapping[id.index()]),
            ParameterLocation::Aggregate(id) => {
                ParameterLocation::Aggregate(aggregate_mapping[id.index()])
            }
            ParameterLocation::AggregateElement { aggregate, index } => {
                ParameterLocation::AggregateElement {
                    aggregate: aggregate_mapping[aggregate.index()],
                    index: *index,
                }
            }
        })
        .collect();
    let descriptor = FunctionDescriptor::new_aggregates(
        function.id(),
        parameters,
        count,
        aggregates,
        function.outbox_cells(),
        function.return_type(),
        function.entry(),
    );
    let descriptor = match function.name() {
        Some(name) => descriptor.with_name(name),
        None => descriptor,
    };
    let continuations = continuations
        .into_iter()
        .map(|continuation| {
            let mut body = continuation.body().to_vec();
            let mut terminator = continuation.terminator().clone();
            let remap = &mut |address: &mut Address| match address {
                Address::Frame(slot) => *slot = FrameSlot::new(mapping[slot.index()]),
                Address::ArrayElement {
                    array: AggregateRegion::Frame(id),
                    ..
                } => *id = aggregate_mapping[id.index()],
                _ => {}
            };
            map_body(&mut body, remap);
            map_terminator(&mut terminator, remap);
            Continuation::new(continuation.id(), continuation.function(), body, terminator)
                .with_source_spans(
                    continuation.body_sources().to_vec(),
                    continuation.terminator_source(),
                )
        })
        .collect();
    (descriptor, continuations)
}

fn build_body(
    body: &[FrameInstruction],
    mut next: usize,
    nodes: &mut Vec<Node>,
    function: &FunctionDescriptor,
) -> usize {
    let mut end = body.len();
    while end > 0 {
        // Lowering clears aggregates with a contiguous run of element sets.
        // Treat a full run as one definition so stale contents are not live.
        if let Some((start, region)) = full_initialization(body, end, function) {
            let mut node = Node::new(function);
            node.successors.push(next);
            node.write_region(region, true);
            next = nodes.len();
            nodes.push(node);
            end = start;
            continue;
        }
        end -= 1;
        let instruction = &body[end];
        let mut node = Node {
            successors: vec![next],
            ..Node::new(function)
        };
        match instruction {
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                ..
            } => {
                node.read(*left);
                node.read(*right);
                for address in [left, right, difference, borrow] {
                    node.write(*address);
                }
            }
            FrameInstruction::Compare {
                left, right, dst, ..
            } => {
                node.read(*left);
                node.read(*right);
                node.write(*left);
                node.write(*right);
                node.write(*dst);
            }
            FrameInstruction::Set { dst, .. } | FrameInstruction::Input { dst } => node.write(*dst),
            FrameInstruction::AddConst { dst, .. } => {
                node.read(*dst);
                node.write(*dst);
            }
            FrameInstruction::Copy { src, dst } => {
                node.read(*src);
                node.write(*dst);
            }
            FrameInstruction::Transfer { src, targets } => {
                node.read(*src);
                node.write(*src);
                for target in targets {
                    node.read(target.dst);
                    node.write(target.dst);
                }
            }
            FrameInstruction::AggregateCopy { src, dst, cells } => {
                node.read_region(*src);
                node.write_region(*dst, full_region(*dst, 0, *cells, function));
            }
            FrameInstruction::Output { src } => node.read(*src),
            FrameInstruction::Loop { condition, body } => {
                node.read(*condition);
                let header = nodes.len();
                nodes.push(node);
                let first = build_body(body, header, nodes, function);
                nodes[header].successors.push(first);
                next = header;
                continue;
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                // The backend owns the condition until the structured branch
                // finishes. Keep it separate from writes in either body.
                node.read(*condition);
                let exit = nodes.len();
                nodes.push(node);
                let then_entry = build_body(then_body, exit, nodes, function);
                let else_entry = build_body(else_body, exit, nodes, function);
                node = Node {
                    successors: vec![then_entry, else_entry],
                    ..Node::new(function)
                };
                node.read(*condition);
                node.write(*condition);
            }
        }
        next = nodes.len();
        nodes.push(node);
    }
    next
}

fn terminator_node(
    terminator: &Terminator,
    entries: &HashMap<ContinuationId, usize>,
    function: &FunctionDescriptor,
) -> Node {
    let mut node = Node::new(function);
    match terminator {
        Terminator::Goto { target } => node.successors.push(entries[target]),
        Terminator::Branch {
            condition,
            then_target,
            else_target,
        } => {
            node.read(*condition);
            node.write(*condition);
            node.successors
                .extend([entries[then_target], entries[else_target]]);
        }
        Terminator::BranchWithBodies {
            condition,
            then_body,
            then_target,
            else_body,
            else_target,
        } => {
            node.read(*condition);
            node.write(*condition);
            for instruction in then_body.iter().chain(else_body) {
                record_frame_effects(&mut node, instruction);
            }
            node.successors
                .extend([entries[then_target], entries[else_target]]);
        }
        Terminator::Call {
            arguments,
            return_to,
            ..
        } => {
            for argument in arguments {
                node.operand(*argument, false, function);
            }
            node.successors.push(entries[return_to]);
        }
        Terminator::Return { value } => {
            if let Some(value) = value {
                node.operand(*value, false, function);
            }
        }
        Terminator::ArrayLoad {
            array,
            index,
            destination,
            return_to,
            ..
        } => {
            node.read_region(*array);
            node.read(*index);
            node.write(*destination);
            node.successors.push(entries[return_to]);
        }
        Terminator::ArrayStore {
            array,
            index,
            value,
            return_to,
            ..
        } => {
            node.write_region(*array, false);
            node.read(*index);
            node.read(*value);
            node.successors.push(entries[return_to]);
        }
        Terminator::AggregateLoad {
            source,
            offset,
            destination,
            return_to,
            ..
        } => {
            node.read(offset.low);
            node.read(offset.high);
            node.read_region(*source);
            node.operand(*destination, true, function);
            node.successors.push(entries[return_to]);
        }
        Terminator::AggregateStore {
            destination,
            offset,
            source,
            return_to,
            ..
        } => {
            node.read(offset.low);
            node.read(offset.high);
            node.write_region(*destination, false);
            node.operand(*source, false, function);
            node.successors.push(entries[return_to]);
        }
        Terminator::Abort | Terminator::Halt => {}
    }
    node
}

/// Record the addresses touched by a structured arm on the terminal CFG node.
/// The arm is conditional: the other successor does not execute its writes.
/// Therefore all touched values are kept live across both successors and no
/// arm write is treated as an unconditional kill. This is conservative, but
/// it prevents a value needed by the untouched arm from being reused for a
/// destination written by the selected arm.
fn record_frame_effects(node: &mut Node, instruction: &FrameInstruction) {
    match instruction {
        FrameInstruction::SubWithBorrow {
            left,
            right,
            difference,
            borrow,
            ..
        } => {
            node.read(*left);
            node.read(*right);
            for address in [left, right, difference, borrow] {
                node.read(*address);
            }
        }
        FrameInstruction::Compare {
            left, right, dst, ..
        } => {
            node.read(*left);
            node.read(*right);
            node.read(*dst);
        }
        FrameInstruction::Set { dst, .. } | FrameInstruction::Input { dst } => node.read(*dst),
        FrameInstruction::AddConst { dst, .. } => {
            node.read(*dst);
        }
        FrameInstruction::Copy { src, dst } => {
            node.read(*src);
            node.read(*dst);
        }
        FrameInstruction::Transfer { src, targets } => {
            node.read(*src);
            for target in targets {
                node.read(target.dst);
            }
        }
        FrameInstruction::AggregateCopy { src, dst, .. } => {
            node.read_region(*src);
            node.read_region(*dst);
        }
        FrameInstruction::Output { src } => node.read(*src),
        FrameInstruction::Loop { condition, body } => {
            node.read(*condition);
            for instruction in body {
                record_frame_effects(node, instruction);
            }
        }
        FrameInstruction::Branch {
            condition,
            then_body,
            else_body,
        } => {
            node.read(*condition);
            for instruction in then_body.iter().chain(else_body) {
                record_frame_effects(node, instruction);
            }
        }
    }
}

fn liveness(nodes: &[Node]) -> Vec<Slots> {
    let mut predecessors = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for &successor in &node.successors {
            predecessors[successor].push(index);
        }
    }
    let mut live = vec![Slots::new(); nodes.len()];
    let mut pending: VecDeque<_> = (0..nodes.len()).rev().collect();
    let mut queued = vec![true; nodes.len()];
    while let Some(index) = pending.pop_front() {
        queued[index] = false;
        let node = &nodes[index];
        let mut updated = node.uses.clone();
        for successor in &node.successors {
            updated.extend(live[*successor].difference(&node.defs).copied());
        }
        if updated != live[index] {
            live[index] = updated;
            for &predecessor in &predecessors[index] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    pending.push_back(predecessor);
                }
            }
        }
    }
    live
}

fn edge(graph: &mut [Slots], left: usize, right: usize) {
    if left != right {
        graph[left].insert(right);
        graph[right].insert(left);
    }
}
fn clique(graph: &mut [Slots], slots: &Slots) {
    for &left in slots {
        for &right in slots.range(..left) {
            edge(graph, left, right);
        }
    }
}
fn color(graph: &[Slots], classes: &[Option<usize>]) -> Vec<usize> {
    let mut order: Vec<_> = (0..graph.len()).collect();
    order.sort_by_key(|&slot| (std::cmp::Reverse(graph[slot].len()), slot));
    let mut mapping = vec![usize::MAX; graph.len()];
    let mut color_classes = Vec::new();
    for slot in order {
        let used: Slots = graph[slot]
            .iter()
            .map(|&neighbor| mapping[neighbor])
            .collect();
        mapping[slot] = color_classes
            .iter()
            .enumerate()
            .find(|(color, class)| **class == classes[slot] && !used.contains(color))
            .map(|(color, _)| color)
            .unwrap_or_else(|| {
                color_classes.push(classes[slot]);
                color_classes.len() - 1
            });
    }
    let mut scalar_colors = HashMap::new();
    for (slot, class) in classes.iter().enumerate() {
        if class.is_none() {
            let next = scalar_colors.len();
            mapping[slot] = *scalar_colors.entry(mapping[slot]).or_insert(next);
        }
    }
    mapping
}

fn map_region(region: &mut AggregateRegion, map: &mut impl FnMut(&mut Address)) {
    let mut address = Address::ArrayElement {
        array: *region,
        index: 0,
    };
    map(&mut address);
    let Address::ArrayElement { array, .. } = address else {
        unreachable!()
    };
    *region = array;
}
fn map_operand(operand: &mut ValueOperand, map: &mut impl FnMut(&mut Address)) {
    match operand {
        ValueOperand::Cell(address) => map(address),
        ValueOperand::Array(region) | ValueOperand::Aggregate { region, .. } => {
            map_region(region, map)
        }
    }
}
fn map_body(body: &mut [FrameInstruction], map: &mut impl FnMut(&mut Address)) {
    for instruction in body {
        match instruction {
            FrameInstruction::Set { dst, .. }
            | FrameInstruction::AddConst { dst, .. }
            | FrameInstruction::Input { dst } => map(dst),
            FrameInstruction::Copy { src, dst } => {
                map(src);
                map(dst);
            }
            FrameInstruction::Compare {
                left, right, dst, ..
            } => {
                map(left);
                map(right);
                map(dst);
            }
            FrameInstruction::SubWithBorrow {
                left,
                right,
                difference,
                borrow,
                ..
            } => {
                map(left);
                map(right);
                map(difference);
                map(borrow);
            }
            FrameInstruction::Transfer { src, targets } => {
                map(src);
                for target in targets {
                    map(&mut target.dst);
                }
            }
            FrameInstruction::AggregateCopy { src, dst, .. } => {
                map_region(src, map);
                map_region(dst, map);
            }
            FrameInstruction::Output { src } => map(src),
            FrameInstruction::Loop { condition, body } => {
                map(condition);
                map_body(body, map);
            }
            FrameInstruction::Branch {
                condition,
                then_body,
                else_body,
            } => {
                map(condition);
                map_body(then_body, map);
                map_body(else_body, map);
            }
        }
    }
}
fn map_terminator(terminator: &mut Terminator, map: &mut impl FnMut(&mut Address)) {
    match terminator {
        Terminator::Branch { condition, .. } => map(condition),
        Terminator::BranchWithBodies {
            condition,
            then_body,
            else_body,
            ..
        } => {
            map(condition);
            map_body(then_body, map);
            map_body(else_body, map);
        }
        Terminator::Call { arguments, .. } => {
            for argument in arguments {
                map_operand(argument, map);
            }
        }
        Terminator::Return { value } => {
            if let Some(value) = value {
                map_operand(value, map);
            }
        }
        Terminator::ArrayLoad {
            array,
            index,
            destination,
            ..
        } => {
            map_region(array, map);
            map(index);
            map(destination);
        }
        Terminator::ArrayStore {
            array,
            index,
            value,
            ..
        } => {
            map_region(array, map);
            map(index);
            map(value);
        }
        Terminator::AggregateLoad {
            source,
            offset,
            destination,
            ..
        } => {
            map(&mut offset.low);
            map(&mut offset.high);
            map_region(source, map);
            map_operand(destination, map);
        }
        Terminator::AggregateStore {
            destination,
            offset,
            source,
            ..
        } => {
            map(&mut offset.low);
            map(&mut offset.high);
            map_region(destination, map);
            map_operand(source, map);
        }
        Terminator::Goto { .. } | Terminator::Abort | Terminator::Halt => {}
    }
}

/// Recognize complete straight-line aggregate initialization without expanding
/// liveness into one virtual register per payload cell.
fn full_initialization(
    body: &[FrameInstruction],
    end: usize,
    function: &FunctionDescriptor,
) -> Option<(usize, AggregateRegion)> {
    let FrameInstruction::Set {
        dst:
            Address::ArrayElement {
                array: AggregateRegion::Frame(id),
                index,
            },
        ..
    } = &body[end - 1]
    else {
        return None;
    };
    let cells = function.frame_aggregate(*id)?.cells();
    if *index + 1 != cells || end < cells {
        return None;
    }
    let start = end - cells;
    let region = AggregateRegion::Frame(*id);
    body[start..end].iter().enumerate().all(|(index, instruction)| {
        matches!(instruction, FrameInstruction::Set { dst: Address::ArrayElement { array, index: actual }, .. } if *array == region && *actual == index)
    }).then_some((start, region))
}

#[cfg(test)]
mod tests {
    use crate::{
        AbiConfig, ContinuationProgram, ContinuationRunOptions, continuation_lowering, hir_inline,
        lexer, macro_expansion, parser, run_continuations_with_io, semantic,
    };

    fn lower(source: &str, inline: bool, reuse: bool) -> ContinuationProgram {
        let ast = parser::parse(lexer::lex(source).unwrap()).unwrap();
        let ast = macro_expansion::expand(ast).unwrap();
        let mut hir = semantic::analyze(&ast).unwrap();
        if inline {
            hir_inline::inline_single_use_functions(&mut hir);
        }
        if reuse {
            continuation_lowering::lower_hir(&hir).unwrap()
        } else {
            continuation_lowering::lower_hir_without_slot_reuse(&hir).unwrap()
        }
    }

    fn execute(program: &ContinuationProgram, input: &[u8]) -> Vec<u8> {
        let mut input = input;
        let mut output = Vec::new();
        run_continuations_with_io(
            program,
            &mut input,
            &mut output,
            ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        output
    }

    fn check(source: &str, input: &[u8], expected: &[u8]) {
        for inline in [false, true] {
            let original = lower(source, inline, false);
            let allocated = lower(source, inline, true);
            assert_eq!(execute(&original, input), expected);
            assert_eq!(execute(&allocated, input), expected);
            {
                let chunk_cells = 16;
                let bf = crate::lower_continuations_with_config(
                    &allocated,
                    AbiConfig::new(chunk_cells).unwrap(),
                )
                .unwrap()
                .to_source();
                assert_eq!(
                    bf_interpreter::run(bf.as_bytes(), input).unwrap(),
                    expected,
                    "inline={inline}, D={chunk_cells}"
                );
            }
        }
    }

    #[test]
    fn reuses_dead_locals_and_temporaries_without_growing_with_statement_count() {
        let mut source = String::from("void main() {");
        for index in 0..100 {
            source.push_str(&format!("{{ cell value = {}; output(value + 1); }}", index));
        }
        source.push('}');
        let original = lower(&source, false, false);
        let allocated = lower(&source, false, true);
        assert!(original.functions()[0].frame_slots() >= 300);
        assert!(allocated.functions()[0].frame_slots() <= 4);
        assert_eq!(execute(&allocated, b""), (1..=100).collect::<Vec<_>>());
    }

    #[test]
    fn loop_back_edges_and_values_live_across_calls_remain_distinct() {
        check(
            r"
            cell recurse(cell n) {
                if (n == 0) { return 1; }
                cell saved = n;
                return recurse(n - 1) + saved;
            }
            void main() {
                cell n = 3;
                cell total;
                while (n) {
                    cell saved = n + 10;
                    total += recurse(n);
                    output(saved);
                    n -= 1;
                }
                output(total);
            }
        ",
            b"",
            &[13, 12, 11, 13],
        );
    }

    #[test]
    fn short_circuit_and_structured_arithmetic_preserve_live_values() {
        check(
            r"
            cell count;
            cell tick() { count += 1; return 1; }
            void main() {
                cell a = input();
                cell b = input();
                output(a < b);
                output(a == b);
                output((a && tick()) || tick());
                output(count);
                output(a); output(b);
            }
        ",
            &[3, 5],
            &[1, 0, 1, 1, 3, 5],
        );
    }

    #[test]
    fn aggregate_reuse_preserves_partial_stores_snapshots_and_portal_offsets() {
        check(
            r"
            struct Pair { cell a; cell b; }
            Pair[3] values;
            cell index;
            cell next() { index += 1; return index; }
            Pair change(Pair value) { value.a += 1; return value; }
            void emit(Pair first, Pair second) {
                output(first.a); output(first.b); output(second.a); output(second.b);
            }
            void main() {
                Pair original; original.a = 10; original.b = 20;
                values[next()] = change(original);
                Pair copy = values[index];
                copy.b += 1;
                emit(copy, change(copy));
                output(original.a); output(original.b);
            }
        ",
            b"",
            &[11, 21, 12, 21, 10, 20],
        );
    }

    #[test]
    fn whole_initializations_allow_aggregate_reuse_but_partial_writes_do_not_kill() {
        let source = r"
            void main() {
                { cell[3] a; a[1] = 7; output(a[1]); }
                { cell[3] b; output(b[1]); b[2] = 8; output(b[2]); }
                { cell[3] c; c[0] = 9; output(c[0]); }
            }
        ";
        let original = lower(source, false, false);
        let allocated = lower(source, false, true);
        assert_eq!(original.functions()[0].frame_aggregates().len(), 3);
        assert_eq!(allocated.functions()[0].frame_aggregates().len(), 1);
        check(source, b"", &[7, 0, 8, 9]);
    }

    #[test]
    fn non_frame_arguments_keep_void_calls_out_of_inline() {
        let source = r"
            cell counter;
            cell next() { counter += 1; return counter; }
            void emit(cell first, cell second) {
                cell local;
                cell[2] array;
                output(first); output(second); output(local); output(array[1]);
                local = 9; array[1] = 9;
            }
            void main() {
                { cell[2] available; output(available[0]); }
                cell first = 2;
                while (first) { emit(next(), next()); first -= 1; }
            }
        ";
        assert_eq!(lower(source, false, true).functions().len(), 3);
        assert_eq!(lower(source, true, true).functions().len(), 3);
        check(source, b"", &[0, 1, 2, 0, 0, 3, 4, 0, 0]);
    }

    #[test]
    fn global_free_inline_may_grow_caller_storage() {
        let source = r"
            void large() { cell[100] scratch; scratch[0] = 7; output(scratch[0]); }
            void main() { large(); }
        ";
        let program = lower(source, true, true);
        assert_eq!(program.functions().len(), 1);
        assert!(
            program
                .function(program.main())
                .unwrap()
                .frame_aggregates()
                .iter()
                .any(|aggregate| aggregate.cells() == 100)
        );
        check(source, b"", &[7]);
    }

    #[test]
    fn global_using_callers_still_reject_frame_growth() {
        let source = r"
            cell global;
            void large() { cell[100] scratch; scratch[0] = 7; output(scratch[0]); }
            void main() { large(); global = 1; output(global); }
        ";
        let program = lower(source, true, true);
        assert_eq!(program.functions().len(), 2);
        assert!(
            program
                .function(program.main())
                .unwrap()
                .frame_aggregates()
                .is_empty()
        );
        check(source, b"", &[7, 1]);
    }

    #[test]
    fn recursive_and_early_returning_void_functions_are_not_inlined() {
        let source = r"
            void recursive(cell n) { if (n) { recursive(n - 1); } }
            void early(cell n) { if (n) { return; } output(1); }
            void main() { recursive(2); early(1); output(2); }
        ";
        assert_eq!(lower(source, true, true).functions().len(), 3);
        check(source, b"", &[2]);
    }
}
