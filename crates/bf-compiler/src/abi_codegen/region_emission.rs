//! Acyclic local execution followed by one context-changing terminal.
use super::*;

impl AbiEmitter<'_> {
    pub(super) fn emit_region(
        &mut self,
        region: &Region,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        self.branch_temporary_depth = 0;
        let layout = self.layout(function)?;
        let selector =
            Location::Relative(layout.frame.frame_offset(layout.region_selector.unwrap()));
        self.emit_region_node(&region.root, selector)?;
        debug_assert_eq!(self.branch_temporary_depth, 0);

        self.with_profile_site(
            "abi",
            "abi.region.select",
            "region terminal selection",
            |emitter| {
                // PcLow was consumed by the outer dispatch gate. Local operations
                // use other ABI scratch, so stage the selection only after every
                // frame-relative branch has closed. The scalar slot is consumed.
                let pc = emitter.current_abi_offset(AbiField::PcLow)?;
                emitter.move_location(selector, Location::Relative(pc));
                emitter.add_abi_field(AbiField::PcLow, 255)?;
                emitter.set_abi_field(AbiField::Branch, 1)?;
                emitter.emit_region_terminal_level(&region.terminals, 0)
            },
        )
    }

    fn emit_region_node(
        &mut self,
        node: &RegionNode,
        selector: Location,
    ) -> Result<(), AbiCodegenError> {
        let continuation = self
            .program
            .continuation(node.id)
            .expect("validated region node");
        self.with_continuation_site(continuation, |emitter| {
            emitter.emit_all(
                continuation.body(),
                continuation.function(),
                Some(continuation.body_sources()),
            )?;
            emitter.with_source_span(continuation.terminator_source(), |emitter| {
                match &node.flow {
                    RegionFlow::Terminal(index) => emitter.with_profile_site(
                        "abi",
                        "abi.region.select",
                        "region terminal selection",
                        |emitter| {
                            emitter.set_location(selector, *index);
                            Ok(())
                        },
                    ),
                    RegionFlow::Goto(next) => emitter.emit_region_node(next, selector),
                    RegionFlow::Branch(then_node, else_node) => {
                        emitter.emit_region_branch(continuation, then_node, else_node, selector)
                    }
                }
            })
        })
    }

    fn emit_region_branch(
        &mut self,
        continuation: &Continuation,
        then_node: &RegionNode,
        else_node: &RegionNode,
        selector: Location,
    ) -> Result<(), AbiCodegenError> {
        let (condition, then_body, else_body, clear_after_arm) = match continuation.terminator() {
            Terminator::Branch { condition, .. } => (*condition, &[][..], &[][..], false),
            Terminator::BranchWithBodies {
                condition,
                then_body,
                else_body,
                ..
            } => (*condition, then_body.as_slice(), else_body.as_slice(), true),
            _ => unreachable!("region branch"),
        };
        let function = continuation.function();
        let condition = self.address_location(condition, function)?;
        let then_gate = Location::Relative(self.acquire_branch_temporary(function)?);
        let else_gate = Location::Relative(self.acquire_branch_temporary(function)?);
        self.move_location(condition, then_gate);
        self.set_location(else_gate, 1);

        self.move_context_to_location(then_gate);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(then_gate);
            emitter.clear_location(else_gate);
            emitter.with_instruction_path("then", |emitter| {
                emitter.emit_all(then_body, function, None)
            })?;
            if clear_after_arm {
                emitter.clear_location(condition);
            }
            emitter.emit_region_node(then_node, selector)?;
            // Never clear the original condition here: the successor can
            // reuse that allocated slot, including as a terminal operand.
            emitter.move_context_to_location(then_gate);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_location_to_context(then_gate);

        self.move_context_to_location(else_gate);
        let body = self.capture(|emitter| {
            emitter.clear_current();
            emitter.move_location_to_context(else_gate);
            emitter.with_instruction_path("else", |emitter| {
                emitter.emit_all(else_body, function, None)
            })?;
            if clear_after_arm {
                emitter.clear_location(condition);
            }
            emitter.emit_region_node(else_node, selector)?;
            emitter.move_context_to_location(else_gate);
            Ok(())
        })?;
        self.emit_loop(body);
        self.move_location_to_context(else_gate);
        self.branch_temporary_depth -= 2;
        Ok(())
    }

    fn emit_region_terminal_level(
        &mut self,
        terminals: &[ContinuationId],
        level: usize,
    ) -> Result<(), AbiCodegenError> {
        let pc = self.current_abi_offset(AbiField::PcLow)?;
        self.move_to(pc);
        let nonzero = self.capture(|emitter| {
            if level + 1 == terminals.len() {
                emitter.clear_current();
                emitter.clear_abi_field(AbiField::Branch)?;
            } else {
                emitter.adjust(255);
                emitter.move_to(0);
                emitter.emit_region_terminal_level(terminals, level + 1)?;
            }
            emitter.move_to(pc);
            Ok(())
        })?;
        self.emit_loop(nonzero);
        self.move_to(0);

        let gate = self.current_abi_offset(AbiField::Branch)?;
        self.move_to(gate);
        let body = self.capture(|emitter| {
            emitter.adjust(255);
            emitter.move_to(0);
            let terminal = emitter.program.continuation(terminals[level]).unwrap();
            emitter.with_continuation_site(terminal, |emitter| {
                emitter.with_source_span(terminal.terminator_source(), |emitter| {
                    emitter.emit_terminator(terminal)
                })
            })?;
            // Only common ABI offsets occur after the terminal. Both this
            // Branch gate and all enclosing PcLow gates are zero in the
            // destination context: Call/portal initialize them, Return resumes
            // the caller whose gates were consumed before suspension. Exits
            // keep the current zero gates. This is the dispatcher contract.
            emitter.move_to(gate);
            Ok(())
        })?;
        self.with_profile_site(
            "abi",
            format!("abi.region.enter.{}", terminals[level].get()),
            "region terminal",
            |emitter| {
                emitter.emit_loop(body);
                Ok(())
            },
        )?;
        self.move_to(0);
        Ok(())
    }
}
