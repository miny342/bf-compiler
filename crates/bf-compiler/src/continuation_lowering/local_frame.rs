//! Direct lowering for local scalar operations that need no continuation edge.
//! This builder is read-only: unsupported control/effects fall back atomically.
use super::*;

impl FunctionLowerer<'_, '_> {
    fn direct_local(&self, place: &HirPlace) -> Option<Address> {
        if !place.projections.is_empty() {
            return None;
        }
        let VariableRef::Local(local) = place.root else {
            return None;
        };
        match self.local_storage.get(local.index())? {
            LocalStorage::Scalar(slot) => Some(Address::Frame(*slot)),
            _ => None,
        }
    }

    fn direct_read(&self, expression: &HirExpression) -> Option<Address> {
        match &expression.kind {
            HirExpressionKind::Place(place) => self.direct_local(place),
            HirExpressionKind::Unary {
                operator: UnaryOperator::Plus,
                operand,
            } => self.direct_read(operand),
            _ => None,
        }
    }

    /// Truth contexts need zero/nonzero, not a materialized canonical 0/1.
    fn direct_loop_condition(&self, expression: &HirExpression) -> Option<Address> {
        if expression.ty != TypeId::CELL {
            return None;
        }
        if let HirExpressionKind::Binary {
            operator: BinaryOperator::NotEqual,
            left,
            right,
        } = &expression.kind
        {
            if hir::constant_cell_value(right) == Some(0) {
                return self.direct_read(left);
            }
            if hir::constant_cell_value(left) == Some(0) {
                return self.direct_read(right);
            }
        }
        self.direct_read(expression)
    }

    fn direct_store(&self, dst: Address, expression: &HirExpression) -> Option<FrameInstruction> {
        if let Some(value) = hir::constant_cell_value(expression) {
            Some(FrameInstruction::Set { dst, value })
        } else if matches!(expression.kind, HirExpressionKind::Input) {
            Some(FrameInstruction::Input { dst })
        } else {
            Some(FrameInstruction::Copy {
                src: self.direct_read(expression)?,
                dst,
            })
        }
    }

    pub(super) fn direct_frame_statement(
        &self,
        statement: &HirStatement,
    ) -> Option<Vec<FrameInstruction>> {
        match &statement.kind {
            HirStatementKind::Empty => Some(vec![]),
            HirStatementKind::Block(statements) => {
                let mut instructions = Vec::new();
                for statement in statements {
                    instructions.extend(self.direct_frame_statement(statement)?);
                }
                Some(instructions)
            }
            HirStatementKind::Output(expression) => Some(vec![FrameInstruction::Output {
                src: self.direct_read(expression)?,
            }]),
            HirStatementKind::Declaration { local, initializer } => {
                let LocalStorage::Scalar(slot) = self.local_storage.get(local.index())? else {
                    return None;
                };
                let dst = Address::Frame(*slot);
                // Preserve zero initialization before evaluating even x=x.
                let mut instructions = vec![FrameInstruction::Set { dst, value: 0 }];
                if let Some(value) = initializer {
                    instructions.push(self.direct_store(dst, value)?);
                }
                Some(instructions)
            }
            HirStatementKind::Assignment {
                target,
                operator,
                value,
            } => {
                let dst = self.direct_local(target)?;
                Some(vec![match operator {
                    AssignmentOperator::Set => self.direct_store(dst, value)?,
                    AssignmentOperator::Add => FrameInstruction::AddConst {
                        dst,
                        value: hir::constant_cell_value(value)?,
                    },
                    AssignmentOperator::Subtract => FrameInstruction::AddConst {
                        dst,
                        value: 0u8.wrapping_sub(hir::constant_cell_value(value)?),
                    },
                }])
            }
            HirStatementKind::While { condition, body } => Some(vec![FrameInstruction::Loop {
                condition: self.direct_loop_condition(condition)?,
                body: self.direct_frame_statement(body)?,
            }]),
            // Calls, early exits, dynamic addresses, general predicates and
            // branches retain the established continuation-based lowering.
            _ => None,
        }
    }
}
