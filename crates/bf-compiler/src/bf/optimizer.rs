//! Semantics-preserving peephole optimization for Brainfuck-shaped IR.

use crate::{
    AnnotatedBfInstruction, AnnotatedBfOperation, AnnotatedBfProgram, BfInstruction, BfProgram,
    ProfileSiteTable,
};

/// Static measurements collected around one BF IR optimization pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BfOptimizationStats {
    /// Recursive BF IR node count before optimization.
    pub instruction_nodes_before: usize,
    /// Recursive BF IR node count after optimization.
    pub instruction_nodes_after: usize,
    /// Serialized BF source size before optimization.
    pub source_bytes_before: usize,
    /// Serialized BF source size after optimization.
    pub source_bytes_after: usize,
}

impl BfOptimizationStats {
    /// Number of recursive BF IR nodes removed.
    pub const fn instruction_nodes_removed(self) -> usize {
        self.instruction_nodes_before
            .saturating_sub(self.instruction_nodes_after)
    }

    /// Number of serialized BF source bytes removed.
    pub const fn source_bytes_removed(self) -> usize {
        self.source_bytes_before
            .saturating_sub(self.source_bytes_after)
    }
}

/// Optimize a complete BF IR program without changing its input/output behavior.
///
/// Pointer movements may be cancelled. As with compiler-generated BF, this
/// assumes that the unoptimized execution does not cross a tape boundary.
pub fn optimize_bf(program: &BfProgram) -> BfProgram {
    BfProgram::new(optimize_instructions(program.instructions()))
}

/// Optimize BF IR and report static size changes.
pub fn optimize_bf_with_stats(program: &BfProgram) -> (BfProgram, BfOptimizationStats) {
    let instruction_nodes_before = instruction_node_count(program.instructions());
    let source_bytes_before = program.to_source().len();
    let optimized = optimize_bf(program);
    let stats = BfOptimizationStats {
        instruction_nodes_before,
        instruction_nodes_after: instruction_node_count(optimized.instructions()),
        source_bytes_before,
        source_bytes_after: optimized.to_source().len(),
    };
    (optimized, stats)
}

/// Optimize annotated BF while preserving provenance.  Merged operations from
/// different sites are attributed to their lowest common ancestor.
pub fn optimize_annotated_bf(program: &AnnotatedBfProgram) -> AnnotatedBfProgram {
    AnnotatedBfProgram::new(
        optimize_annotated_instructions(program.instructions(), program.sites()),
        program.sites().clone(),
    )
}

fn optimize_annotated_instructions(
    instructions: &[AnnotatedBfInstruction],
    sites: &ProfileSiteTable,
) -> Vec<AnnotatedBfInstruction> {
    let mut output = Vec::with_capacity(instructions.len());
    for instruction in instructions {
        match &instruction.operation {
            AnnotatedBfOperation::Move(amount) => {
                push_annotated_move(&mut output, instruction.site, *amount, sites)
            }
            AnnotatedBfOperation::Add(value) => {
                push_annotated_add(&mut output, instruction.site, *value, sites)
            }
            AnnotatedBfOperation::Input => {
                while output.last().is_some_and(|item| {
                    matches!(item.operation, AnnotatedBfOperation::Add(_))
                        || annotated_is_clear(item)
                }) {
                    output.pop();
                }
                output.push(instruction.clone());
            }
            AnnotatedBfOperation::Output => output.push(instruction.clone()),
            AnnotatedBfOperation::Loop(body) => {
                let body = optimize_annotated_instructions(body, sites);
                if annotated_is_clear_body(&body) {
                    push_annotated_clear(&mut output, instruction.site);
                } else {
                    output.push(AnnotatedBfInstruction::new(
                        instruction.site,
                        AnnotatedBfOperation::Loop(body),
                    ));
                }
            }
        }
    }
    output
}

fn merge_site(
    left: bf_profiling::ProfileSiteId,
    right: bf_profiling::ProfileSiteId,
    sites: &ProfileSiteTable,
) -> bf_profiling::ProfileSiteId {
    if left == right {
        left
    } else {
        sites.lowest_common_ancestor(left, right)
    }
}

fn push_annotated_move(
    output: &mut Vec<AnnotatedBfInstruction>,
    site: bf_profiling::ProfileSiteId,
    amount: isize,
    sites: &ProfileSiteTable,
) {
    if amount == 0 {
        return;
    }
    if let Some(last) = output.last_mut()
        && let AnnotatedBfOperation::Move(previous) = &mut last.operation
        && let Some(combined) = previous.checked_add(amount)
    {
        if combined == 0 {
            output.pop();
        } else {
            *previous = combined;
            last.site = merge_site(last.site, site, sites);
        }
        return;
    }
    output.push(AnnotatedBfInstruction::new(
        site,
        AnnotatedBfOperation::Move(amount),
    ));
}

fn push_annotated_add(
    output: &mut Vec<AnnotatedBfInstruction>,
    site: bf_profiling::ProfileSiteId,
    value: u8,
    sites: &ProfileSiteTable,
) {
    if value == 0 {
        return;
    }
    if let Some(last) = output.last_mut()
        && let AnnotatedBfOperation::Add(previous) = &mut last.operation
    {
        let combined = previous.wrapping_add(value);
        if combined == 0 {
            output.pop();
        } else {
            *previous = combined;
            last.site = merge_site(last.site, site, sites);
        }
        return;
    }
    output.push(AnnotatedBfInstruction::new(
        site,
        AnnotatedBfOperation::Add(value),
    ));
}

fn push_annotated_clear(
    output: &mut Vec<AnnotatedBfInstruction>,
    site: bf_profiling::ProfileSiteId,
) {
    while output
        .last()
        .is_some_and(|item| matches!(item.operation, AnnotatedBfOperation::Add(_)))
    {
        output.pop();
    }
    if output.last().is_some_and(annotated_is_clear) {
        return;
    }
    output.push(AnnotatedBfInstruction::new(
        site,
        AnnotatedBfOperation::Loop(vec![AnnotatedBfInstruction::new(
            site,
            AnnotatedBfOperation::Add(u8::MAX),
        )]),
    ));
}

fn annotated_is_clear(instruction: &AnnotatedBfInstruction) -> bool {
    matches!(&instruction.operation, AnnotatedBfOperation::Loop(body) if annotated_is_clear_body(body))
}

fn annotated_is_clear_body(body: &[AnnotatedBfInstruction]) -> bool {
    matches!(body, [AnnotatedBfInstruction { operation: AnnotatedBfOperation::Add(value), .. }] if value % 2 == 1)
}

fn optimize_instructions(instructions: &[BfInstruction]) -> Vec<BfInstruction> {
    let mut output = Vec::with_capacity(instructions.len());
    for instruction in instructions {
        match instruction {
            BfInstruction::Move(amount) => push_move(&mut output, *amount),
            BfInstruction::Add(value) => push_add(&mut output, *value),
            BfInstruction::Input => {
                // Input overwrites the current cell. Adjacent arithmetic and
                // guaranteed-terminating clear loops cannot affect its result.
                while output
                    .last()
                    .is_some_and(|item| matches!(item, BfInstruction::Add(_)) || is_clear(item))
                {
                    output.pop();
                }
                output.push(BfInstruction::Input);
            }
            BfInstruction::Output => output.push(BfInstruction::Output),
            BfInstruction::Loop(body) => {
                let body = optimize_instructions(body);
                if is_clear_body(&body) {
                    push_clear(&mut output);
                } else {
                    output.push(BfInstruction::Loop(body));
                }
            }
        }
    }
    output
}

fn push_move(output: &mut Vec<BfInstruction>, amount: isize) {
    if amount == 0 {
        return;
    }
    if let Some(BfInstruction::Move(previous)) = output.last_mut()
        && let Some(combined) = previous.checked_add(amount)
    {
        if combined == 0 {
            output.pop();
        } else {
            *previous = combined;
        }
        return;
    }
    output.push(BfInstruction::Move(amount));
}

fn push_add(output: &mut Vec<BfInstruction>, value: u8) {
    if value == 0 {
        return;
    }
    if let Some(BfInstruction::Add(previous)) = output.last_mut() {
        let combined = previous.wrapping_add(value);
        if combined == 0 {
            output.pop();
        } else {
            *previous = combined;
        }
        return;
    }
    output.push(BfInstruction::Add(value));
}

fn push_clear(output: &mut Vec<BfInstruction>) {
    while matches!(output.last(), Some(BfInstruction::Add(_))) {
        output.pop();
    }
    if output.last().is_some_and(is_clear) {
        return;
    }
    output.push(canonical_clear());
}

fn canonical_clear() -> BfInstruction {
    BfInstruction::Loop(vec![BfInstruction::Add(u8::MAX)])
}

fn is_clear(instruction: &BfInstruction) -> bool {
    matches!(instruction, BfInstruction::Loop(body) if is_clear_body(body))
}

fn is_clear_body(body: &[BfInstruction]) -> bool {
    matches!(body, [BfInstruction::Add(value)] if value % 2 == 1)
}

fn instruction_node_count(instructions: &[BfInstruction]) -> usize {
    instructions
        .iter()
        .map(|instruction| match instruction {
            BfInstruction::Loop(body) => 1 + instruction_node_count(body),
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;

    #[test]
    fn folds_moves_additions_and_nested_bodies() {
        let program = BfProgram::new(vec![
            BfInstruction::Move(3),
            BfInstruction::Move(-1),
            BfInstruction::Move(0),
            BfInstruction::Add(10),
            BfInstruction::Add(253),
            BfInstruction::Add(0),
            BfInstruction::Loop(vec![
                BfInstruction::Move(0),
                BfInstruction::Add(2),
                BfInstruction::Add(254),
            ]),
        ]);

        assert_eq!(
            optimize_bf(&program).into_instructions(),
            vec![
                BfInstruction::Move(2),
                BfInstruction::Add(7),
                BfInstruction::Loop(vec![]),
            ]
        );
    }

    #[test]
    fn simplifies_clear_and_overwritten_cell_updates() {
        let clear = canonical_clear();
        let program = BfProgram::new(vec![
            BfInstruction::Add(7),
            clear.clone(),
            BfInstruction::Add(3),
            BfInstruction::Add(4),
            BfInstruction::Output,
            BfInstruction::Add(9),
            clear.clone(),
            clear,
            BfInstruction::Add(5),
            BfInstruction::Input,
            BfInstruction::Output,
        ]);

        assert_eq!(
            optimize_bf(&program).into_instructions(),
            vec![
                canonical_clear(),
                BfInstruction::Add(7),
                BfInstruction::Output,
                BfInstruction::Input,
                BfInstruction::Output,
            ]
        );
    }

    #[test]
    fn canonicalizes_every_guaranteed_terminating_addition_loop() {
        let program = BfProgram::new(vec![
            BfInstruction::Loop(vec![BfInstruction::Add(3)]),
            BfInstruction::Loop(vec![BfInstruction::Add(2)]),
        ]);

        assert_eq!(
            optimize_bf(&program).into_instructions(),
            vec![
                canonical_clear(),
                BfInstruction::Loop(vec![BfInstruction::Add(2)]),
            ]
        );
    }

    #[test]
    fn keeps_unmergeable_move_overflow_separate() {
        let program = BfProgram::new(vec![
            BfInstruction::Move(isize::MAX),
            BfInstruction::Move(1),
        ]);
        assert_eq!(optimize_bf(&program), program);
    }

    #[test]
    fn optimized_program_has_the_same_observable_result() {
        let program = BfProgram::new(vec![
            BfInstruction::Add(2),
            BfInstruction::Add(1),
            BfInstruction::Loop(vec![
                BfInstruction::Add(255),
                BfInstruction::Move(2),
                BfInstruction::Move(-1),
                BfInstruction::Add(2),
                BfInstruction::Add(255),
                BfInstruction::Move(-1),
            ]),
            BfInstruction::Move(1),
            BfInstruction::Output,
            BfInstruction::Add(4),
            BfInstruction::Input,
            BfInstruction::Output,
        ]);
        let optimized = optimize_bf(&program);

        assert_eq!(
            run(program.to_source().as_bytes(), b"A"),
            run(optimized.to_source().as_bytes(), b"A")
        );
    }

    #[test]
    fn reports_static_reductions() {
        let program = BfProgram::new(vec![
            BfInstruction::Move(2),
            BfInstruction::Move(-1),
            BfInstruction::Add(2),
            BfInstruction::Add(255),
        ]);
        let (optimized, stats) = optimize_bf_with_stats(&program);

        assert_eq!(optimized.to_source(), ">+");
        assert_eq!(stats.instruction_nodes_before, 4);
        assert_eq!(stats.instruction_nodes_after, 2);
        assert_eq!(stats.source_bytes_before, 6);
        assert_eq!(stats.source_bytes_after, 2);
        assert_eq!(stats.instruction_nodes_removed(), 2);
        assert_eq!(stats.source_bytes_removed(), 4);
    }

    #[test]
    fn annotated_merge_uses_lowest_common_ancestor() {
        let root = bf_profiling::ProfileSiteId(0);
        let left = bf_profiling::ProfileSiteId(1);
        let right = bf_profiling::ProfileSiteId(2);
        let program = AnnotatedBfProgram::new(
            vec![
                AnnotatedBfInstruction::new(left, AnnotatedBfOperation::Add(1)),
                AnnotatedBfInstruction::new(right, AnnotatedBfOperation::Add(2)),
            ],
            ProfileSiteTable::new(vec![None, Some(root), Some(root)]),
        );
        let optimized = optimize_annotated_bf(&program);
        assert_eq!(optimized.instructions()[0].site, root);
        assert_eq!(optimized.to_source(), "+".repeat(3));
    }
}
