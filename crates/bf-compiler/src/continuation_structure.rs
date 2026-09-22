//! Region reduction on source/CIR continuations with virtual or allocated slots.
//!
//! Only single-entry regions are consumed. Calls, portal requests and terminal
//! exits remain explicit; no operation crosses one of those boundaries.

use std::collections::{HashMap, HashSet};

use crate::continuation_ir::{
    Address, Continuation, ContinuationId, ContinuationIrError, ContinuationProgram,
    FrameInstruction, FrameSlot, FunctionId, Terminator,
};
use crate::continuation_optimizer::{
    ContinuationOptimizationOptions, optimize_continuations_with_options,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LocalStructureStats {
    pub continuations_removed: usize,
    pub straight_blocks: usize,
    pub branches: usize,
    pub loops: usize,
    pub scratch_slots: usize,
}

/// Reduce local regions. Source compilation runs this before allocation, while
/// callers of the public CIR API may also pass allocated storage. The final
/// cleanup does not duplicate branch bodies.
pub fn structure_local_control_flow(
    program: &ContinuationProgram,
) -> Result<(ContinuationProgram, LocalStructureStats), ContinuationIrError> {
    let mut functions = program.functions().to_vec();
    let mut nodes = program.continuations().to_vec();
    let mut scratch = HashMap::<FunctionId, Address>::new();
    let mut stats = LocalStructureStats::default();
    loop {
        let by_id: HashMap<_, _> = nodes.iter().map(|node| (node.id(), node)).collect();
        let mut incoming = HashMap::<ContinuationId, usize>::new();
        // Entry, return and resume references prevent consuming a node even
        // when its only ordinary CFG predecessor is inside the region.
        for function in &functions {
            *incoming.entry(function.entry()).or_default() += 1;
        }
        for node in &nodes {
            for target in successors(node.terminator()) {
                *incoming.entry(target).or_default() += 1;
            }
        }
        let mut replacement = None;
        for node in &nodes {
            let single_entry = |target: ContinuationId| {
                target != node.id()
                    && incoming.get(&target) == Some(&1)
                    && by_id[&target].function() == node.function()
            };
            let mut body = node.body().to_vec();
            let mut body_sources = node.body_sources().to_vec();
            let mut removed = HashSet::new();
            let exit;
            let terminator_source;
            match *node.terminator() {
                Terminator::Goto { target } if single_entry(target) => {
                    let next = by_id[&target];
                    body.extend_from_slice(next.body());
                    body_sources.extend_from_slice(next.body_sources());
                    removed.insert(target);
                    stats.straight_blocks += 1;
                    terminator_source = next.terminator_source();
                    replacement = Some((
                        node.id(),
                        body,
                        body_sources,
                        next.terminator().clone(),
                        terminator_source,
                        removed,
                    ));
                    break;
                }
                Terminator::Branch {
                    condition,
                    then_target,
                    else_target,
                } => {
                    if then_target == else_target {
                        body.push(FrameInstruction::Set {
                            dst: condition,
                            value: 0,
                        });
                        body_sources
                            .push(node.terminator_source().or_else(|| node.primary_source()));
                        exit = then_target;
                        terminator_source = by_id[&exit].terminator_source();
                    } else if then_target == node.id()
                        || (single_entry(then_target)
                            && matches!(by_id[&then_target].terminator(),
                                Terminator::Goto { target } if *target == node.id()))
                    {
                        // The terminal Branch clears its condition before the
                        // back-edge. Loop does not, so reproduce that explicitly.
                        let mut iteration = vec![FrameInstruction::Set {
                            dst: condition,
                            value: 0,
                        }];
                        if then_target != node.id() {
                            iteration.extend_from_slice(by_id[&then_target].body());
                            removed.insert(then_target);
                        }
                        iteration.extend_from_slice(node.body());
                        body.push(FrameInstruction::Loop {
                            condition,
                            body: iteration,
                        });
                        body_sources
                            .push(node.terminator_source().or_else(|| node.primary_source()));
                        exit = else_target;
                        terminator_source = by_id[&exit].terminator_source();
                        stats.loops += 1;
                    } else {
                        // Find the join of a diamond (including one empty arm).
                        let arm_exit = |target| {
                            if !single_entry(target) {
                                return None;
                            }
                            match by_id[&target].terminator() {
                                Terminator::Goto { target } => Some(*target),
                                _ => None,
                            }
                        };
                        let left = arm_exit(then_target);
                        let right = arm_exit(else_target);
                        let join = if left == Some(else_target) {
                            Some(else_target)
                        } else if right == Some(then_target) {
                            Some(then_target)
                        } else if left.is_some() && left == right {
                            left
                        } else {
                            None
                        };
                        let Some(join) = join else {
                            continue;
                        };
                        if join == node.id() {
                            continue;
                        }
                        let mut arm_body = |target| {
                            if target == join {
                                Vec::new()
                            } else {
                                removed.insert(target);
                                by_id[&target].body().to_vec()
                            }
                        };
                        let then_body = arm_body(then_target);
                        let else_body = arm_body(else_target);
                        let guard = *scratch.entry(node.function()).or_insert_with(|| {
                            let descriptor = functions
                                .iter_mut()
                                .find(|function| function.id() == node.function())
                                .unwrap();
                            let slot = descriptor.frame_slots();
                            *descriptor = descriptor.clone().with_frame_slots(slot + 1);
                            stats.scratch_slots += 1;
                            Address::Frame(FrameSlot::new(slot))
                        });
                        // Structured Branch also clears its condition AFTER
                        // the selected arm. The original terminal only cleared
                        // it before dispatch; allocated arms may reuse that slot.
                        body.push(FrameInstruction::Copy {
                            src: condition,
                            dst: guard,
                        });
                        body.push(FrameInstruction::Set {
                            dst: condition,
                            value: 0,
                        });
                        body.push(FrameInstruction::Branch {
                            condition: guard,
                            then_body,
                            else_body,
                        });
                        body_sources
                            .push(node.terminator_source().or_else(|| node.primary_source()));
                        exit = join;
                        terminator_source = by_id[&exit].terminator_source();
                        stats.branches += 1;
                    }
                }
                _ => continue,
            }
            replacement = Some((
                node.id(),
                body,
                body_sources,
                Terminator::Goto { target: exit },
                terminator_source,
                removed,
            ));
            break;
        }
        let Some((id, body, body_sources, terminator, terminator_source, removed)) = replacement
        else {
            break;
        };
        let function = by_id[&id].function();
        nodes.retain(|node| !removed.contains(&node.id()));
        *nodes.iter_mut().find(|node| node.id() == id).unwrap() =
            Continuation::new(id, function, body, terminator)
                .with_source_spans(body_sources, terminator_source);
    }
    let reduced = ContinuationProgram::new_with_globals(
        program.main(),
        program.globals().to_vec(),
        functions,
        nodes,
    )?
    .with_source_files(program.source_files().to_vec());
    let (compacted, _) = optimize_continuations_with_options(
        &reduced,
        ContinuationOptimizationOptions {
            inline_branch_successors: false,
            structure_local_control_flow: false,
        },
    )?;
    stats.continuations_removed = program.continuations().len() - compacted.continuations().len();
    Ok((compacted, stats))
}

fn successors(terminator: &Terminator) -> Vec<ContinuationId> {
    terminator.edges().map(|(target, _)| target).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbiConfig, ContinuationRunOptions, FunctionDescriptor, ValueType};

    fn execute(
        program: &ContinuationProgram,
        mut input: &[u8],
    ) -> (Vec<u8>, crate::ContinuationRunStats) {
        let mut output = Vec::new();
        let stats = crate::run_continuations_with_io(
            program,
            &mut input,
            &mut output,
            ContinuationRunOptions::default(),
            |_| {},
        )
        .unwrap();
        (output, stats)
    }

    fn check(program: &ContinuationProgram, input: &[u8]) -> LocalStructureStats {
        let (candidate, stats) = structure_local_control_flow(program).unwrap();
        let (expected, before) = execute(program, input);
        let (actual, after) = execute(&candidate, input);
        assert_eq!(actual, expected);
        assert_eq!(
            (
                before.calls,
                before.returns,
                before.aborted,
                before.array_loads,
                before.array_stores,
                before.aggregate_loads,
                before.aggregate_stores
            ),
            (
                after.calls,
                after.returns,
                after.aborted,
                after.array_loads,
                after.array_stores,
                after.aggregate_loads,
                after.aggregate_stores
            )
        );
        {
            let d = 16;
            let bf = crate::optimize_bf(
                &crate::lower_continuations_with_config(&candidate, AbiConfig::new(d).unwrap())
                    .unwrap(),
            )
            .to_source();
            assert_eq!(
                bf_interpreter::run(bf.as_bytes(), input).unwrap(),
                expected,
                "D={d}"
            );
        }
        stats
    }

    #[test]
    fn diamond_preserves_condition_slot_reused_by_either_arm() {
        let id = |n| ContinuationId::new(n).unwrap();
        let f = FunctionId::new(0);
        let condition = Address::Frame(FrameSlot::new(0));
        let program = ContinuationProgram::new(
            f,
            vec![FunctionDescriptor::new(
                f,
                vec![],
                1,
                ValueType::Void,
                id(1),
            )],
            vec![
                Continuation::new(
                    id(1),
                    f,
                    vec![FrameInstruction::Input { dst: condition }],
                    Terminator::Branch {
                        condition,
                        then_target: id(2),
                        else_target: id(3),
                    },
                ),
                Continuation::new(
                    id(2),
                    f,
                    vec![FrameInstruction::AddConst {
                        dst: condition,
                        value: 17,
                    }],
                    Terminator::Goto { target: id(4) },
                ),
                Continuation::new(
                    id(3),
                    f,
                    vec![FrameInstruction::AddConst {
                        dst: condition,
                        value: 29,
                    }],
                    Terminator::Goto { target: id(4) },
                ),
                Continuation::new(
                    id(4),
                    f,
                    vec![FrameInstruction::Output { src: condition }],
                    Terminator::Halt,
                ),
            ],
        )
        .unwrap();
        for input in [0, 1, 255] {
            let stats = check(&program, &[input]);
            assert_eq!(stats.branches, 1);
            assert_eq!(stats.scratch_slots, 1);
        }
    }

    #[test]
    fn source_nested_regions_are_already_structured_and_preserve_io() {
        let source = r"void main() {
            cell n = input();
            while (n != 0) {
                cell k = 3;
                while (k != 0) {
                    if (n == 2) { if (k == 1) { output(7); } else { output(8); } }
                    else { output(n); }
                    k -= 1;
                }
                n -= 1;
            }
            output(n);
        }";
        // HIR lowering already preserves these regions, independently of either
        // CFG option. The reconstruction pass must leave them intact.
        for inline_branch_successors in [false, true] {
            let (program, _) = crate::lower_source_with_options(
                source,
                ContinuationOptimizationOptions {
                    inline_branch_successors,
                    structure_local_control_flow: false,
                },
            )
            .unwrap();
            let (integrated, integrated_stats) = crate::lower_source_with_options(
                source,
                ContinuationOptimizationOptions {
                    inline_branch_successors,
                    structure_local_control_flow: true,
                },
            )
            .unwrap();
            let (explicit, explicit_stats) = structure_local_control_flow(&program).unwrap();
            assert_eq!(integrated, explicit);
            assert_eq!(integrated_stats.local_structure, explicit_stats);
            assert_eq!(integrated_stats.continuations_before, 1);
            for input in [0, 3] {
                let stats = check(&program, &[input]);
                assert_eq!(stats, LocalStructureStats::default());
            }
        }
    }

    #[test]
    fn calls_portals_recursion_and_abort_keep_their_boundaries() {
        let source = r"cell[32] values;
            cell recurse(cell n) {
                if (n == 0) { return 1; }
                cell saved = n;
                cell result = recurse(n - 1);
                return result + saved;
            }
            void main() {
                cell n = input();
                while (n != 0) {
                    values[n] = recurse(n);
                    output(values[n]);
                    if (n == 2) { abort(); }
                    n -= 1;
                }
                output(99);
            }";
        let program = crate::lower_source(source).unwrap();
        for input in [0, 1, 3, 17] {
            check(&program, &[input]);
        }
    }

    #[test]
    fn cir_adapter_loop_preserves_flat_condition_and_wrap_boundaries() {
        use crate::{
            SelfhostCirBinaryOp as Op, SelfhostCirContinuation as C, SelfhostCirFunction as F,
            SelfhostCirInstruction as I, SelfhostCirProgram as P, SelfhostCirReturnType as R,
            SelfhostCirTerminator as T,
        };
        let flat = P::new(
            0,
            0,
            vec![F {
                id: 0,
                entry: 1,
                frame_cells: 3,
                return_type: R::Void,
                parameters: vec![],
            }],
            vec![
                C {
                    id: 1,
                    function: 0,
                    instructions: vec![
                        I::Input { destination: 0 },
                        I::Set {
                            destination: 2,
                            value: 1,
                        },
                    ],
                    terminator: T::Goto { target: 2 },
                },
                C {
                    id: 2,
                    function: 0,
                    instructions: vec![I::Copy {
                        destination: 1,
                        source: 0,
                    }],
                    terminator: T::Branch {
                        condition: 1,
                        then_target: 3,
                        else_target: 4,
                    },
                },
                C {
                    id: 3,
                    function: 0,
                    instructions: vec![
                        I::Output { source: 0 },
                        // CIR Binary consumes its source operand.
                        I::Set {
                            destination: 2,
                            value: 1,
                        },
                        I::Binary {
                            op: Op::Subtract,
                            destination: 0,
                            source: 2,
                        },
                    ],
                    terminator: T::Goto { target: 2 },
                },
                C {
                    id: 4,
                    function: 0,
                    instructions: vec![I::Output { source: 1 }],
                    terminator: T::Halt,
                },
            ],
        )
        .unwrap();
        let decoded = P::decode(&flat.encode().unwrap()).unwrap();
        for inline_branch_successors in [false, true] {
            let (program, _) = crate::lower_selfhost_cir_with_options(
                &decoded,
                ContinuationOptimizationOptions {
                    inline_branch_successors,
                    structure_local_control_flow: false,
                },
            )
            .unwrap();
            let (integrated, integrated_stats) = crate::lower_selfhost_cir_with_options(
                &decoded,
                ContinuationOptimizationOptions {
                    inline_branch_successors,
                    structure_local_control_flow: true,
                },
            )
            .unwrap();
            let (explicit, explicit_stats) = structure_local_control_flow(&program).unwrap();
            assert_eq!(integrated, explicit);
            assert_eq!(integrated_stats.local_structure, explicit_stats);
            for input in [0, 1, 255] {
                assert!(check(&program, &[input]).loops > 0);
            }
        }
    }
}
