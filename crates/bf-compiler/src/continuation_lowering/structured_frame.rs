//! Classify HIR effects before allocating any continuation IDs. Frame-only
//! control flow reuses the ordinary value/place lowering inside Branch/Loop.
use super::*;

impl FunctionLowerer<'_, '_> {
    pub(super) fn statement_is_frame_only(&self, statement: &HirStatement) -> bool {
        match &statement.kind {
            HirStatementKind::Empty => true,
            HirStatementKind::Block(statements) => {
                for statement in statements {
                    if !self.statement_is_frame_only(statement) {
                        return false;
                    }
                    if statement_stops(statement) {
                        break;
                    }
                }
                true
            }
            HirStatementKind::Declaration { initializer, .. } => initializer
                .as_ref()
                .is_none_or(|value| self.expression_is_frame_only(value)),
            HirStatementKind::Assignment { value, target, .. } => {
                self.expression_is_frame_only(value) && self.place_is_frame_only(target)
            }
            HirStatementKind::Output(value) => self.expression_is_frame_only(value),
            HirStatementKind::Call { .. }
            | HirStatementKind::Return(_)
            | HirStatementKind::Abort => false,
            HirStatementKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if let Some(value) = hir::constant_cell_value(condition) {
                    return if value != 0 {
                        self.statement_is_frame_only(then_branch)
                    } else {
                        else_branch
                            .as_deref()
                            .is_none_or(|branch| self.statement_is_frame_only(branch))
                    };
                }
                self.expression_is_frame_only(condition)
                    && self.statement_is_frame_only(then_branch)
                    && else_branch
                        .as_deref()
                        .is_none_or(|branch| self.statement_is_frame_only(branch))
            }
            HirStatementKind::While { condition, body } => {
                hir::constant_cell_value(condition) == Some(0)
                    || (self.expression_is_frame_only(condition)
                        && self.statement_is_frame_only(body))
            }
        }
    }

    pub(super) fn expression_is_frame_only(&self, expression: &HirExpression) -> bool {
        match &expression.kind {
            HirExpressionKind::Literal(_)
            | HirExpressionKind::EnumVariant(_)
            | HirExpressionKind::StringLiteral(_)
            | HirExpressionKind::Input => true,
            HirExpressionKind::Place(place) => self.place_is_frame_only(place),
            HirExpressionKind::Project { base, projections } => {
                self.expression_is_frame_only(base)
                    && self.projections_are_frame_only(base.ty, expression.ty, projections)
            }
            HirExpressionKind::Unary { operand, .. } => self.expression_is_frame_only(operand),
            HirExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                if !self.expression_is_frame_only(left) {
                    return false;
                }
                match (operator, hir::constant_cell_value(left)) {
                    (BinaryOperator::LogicalAnd, Some(0)) => true,
                    (BinaryOperator::LogicalOr, Some(value)) if value != 0 => true,
                    _ => self.expression_is_frame_only(right),
                }
            }
            HirExpressionKind::Call { .. } => false,
        }
    }

    fn place_is_frame_only(&self, place: &HirPlace) -> bool {
        self.variable_type(place.root).is_ok_and(|root_type| {
            self.projections_are_frame_only(root_type, place.ty, &place.projections)
        })
    }

    fn projections_are_frame_only(
        &self,
        root_type: TypeId,
        result_type: TypeId,
        projections: &[Projection],
    ) -> bool {
        projections.iter().all(|projection| match projection {
            Projection::Index {
                index: ArrayIndex::Dynamic(index),
                ..
            } => {
                // Empty accesses emit no portal, but still evaluate each index.
                (self.program.types.cells(root_type) == 0
                    || self.program.types.cells(result_type) == 0)
                    && self.expression_is_frame_only(index)
            }
            _ => true,
        })
    }

    /// Collect instructions for an already-classified frame-only region. No
    /// speculative continuations or ID allocation are permitted here.
    pub(super) fn capture_frame_instructions(
        &mut self,
        lower: impl FnOnce(&mut Self) -> Result<(), ContinuationLoweringError>,
    ) -> Result<Vec<FrameInstruction>, ContinuationLoweringError> {
        let current = self
            .current
            .as_mut()
            .expect("frame region needs a continuation");
        let id = current.id;
        let prefix = std::mem::take(&mut current.body);
        let next_id = self.ids.next;
        let capturing = std::mem::replace(&mut self.capturing_frame, true);
        let result = lower(self);
        self.capturing_frame = capturing;
        result?;
        assert_eq!(
            self.ids.next, next_id,
            "frame-only lowering allocated a continuation"
        );
        let current = self
            .current
            .as_mut()
            .expect("frame-only lowering terminated a continuation");
        assert_eq!(current.id, id);
        Ok(std::mem::replace(&mut current.body, prefix))
    }

    /// Find a block with one direct call and frame-only regions around it.
    /// Such a block can be represented as a branch arm followed by a call
    /// terminator, instead of dispatching once for the prefix first.
    pub(super) fn yielding_call_split<'b>(
        &self,
        body: &'b HirStatement,
    ) -> Option<(&'b [HirStatement], &'b HirStatement, &'b [HirStatement])> {
        let HirStatementKind::Block(statements) = &body.kind else {
            return None;
        };
        let call_indices = statements
            .iter()
            .enumerate()
            .filter_map(|(index, statement)| {
                matches!(&statement.kind, HirStatementKind::Call { .. }).then_some(index)
            })
            .collect::<Vec<_>>();
        let [call_index] = call_indices.as_slice() else {
            return None;
        };
        let HirStatementKind::Call { arguments, .. } = &statements[*call_index].kind else {
            unreachable!("call index was classified above");
        };
        if !arguments
            .iter()
            .all(|argument| self.expression_is_frame_only(argument))
        {
            return None;
        }
        let prefix = &statements[..*call_index];
        let suffix = &statements[*call_index + 1..];
        if prefix
            .iter()
            .any(|statement| !self.statement_is_frame_only(statement) || statement_stops(statement))
            || suffix.iter().any(|statement| {
                !self.statement_is_frame_only(statement) || statement_stops(statement)
            })
        {
            return None;
        }
        Some((prefix, &statements[*call_index], suffix))
    }
}

/// Source paths that cannot reach the next statement. In particular, a
/// structured infinite loop needs no dispatcher edge, even in a value function.
pub(super) fn statement_stops(statement: &HirStatement) -> bool {
    match &statement.kind {
        HirStatementKind::Return(_) | HirStatementKind::Abort => true,
        HirStatementKind::Block(statements) => statements.iter().any(statement_stops),
        HirStatementKind::While { condition, .. } => {
            hir::constant_cell_value(condition).is_some_and(|value| value != 0)
        }
        HirStatementKind::If {
            condition,
            then_branch,
            else_branch,
        } => match hir::constant_cell_value(condition) {
            Some(0) => else_branch.as_deref().is_some_and(statement_stops),
            Some(_) => statement_stops(then_branch),
            None => {
                statement_stops(then_branch) && else_branch.as_deref().is_some_and(statement_stops)
            }
        },
        _ => false,
    }
}
