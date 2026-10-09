//! Prove a wrapper only returns its last Call's scalar/void result. Local
//! snapshots may be discarded, but I/O, globals and looping suffixes may not.
use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Value {
    Result,
    Zero,
    Unknown,
}

fn local(address: Address) -> bool {
    matches!(
        address,
        Address::Frame(_)
            | Address::ArrayElement {
                array: AggregateRegion::Frame(_),
                ..
            }
    )
}

impl Graph {
    pub(super) fn forwards_calls(&self, function: FunctionId) -> bool {
        let mut calls = false;
        for node in self.nodes.iter().filter(|c| c.function() == function) {
            match node.terminator() {
                Terminator::Call { .. } => {
                    calls = true;
                    if !self.forwards_result(node) {
                        return false;
                    }
                }
                terminal if terminal.boundary() == crate::cir::ir::BoundaryKind::Hard => {
                    return false;
                }
                _ => {}
            }
        }
        calls
    }

    pub(super) fn forwards_result(&self, call: &Continuation) -> bool {
        let Terminator::Call {
            callee, return_to, ..
        } = call.terminator()
        else {
            return false;
        };
        let kind = self
            .functions
            .iter()
            .find(|f| f.id() == call.function())
            .unwrap()
            .return_type();
        if !matches!(kind, ValueType::Cell | ValueType::Void)
            || self
                .functions
                .iter()
                .find(|f| f.id() == *callee)
                .unwrap()
                .return_type()
                != kind
        {
            return false;
        }
        let mut values = HashMap::new();
        if let Some(address) = self.results[&call.id()].scalar {
            values.insert(address, Value::Result);
        }
        let mut next = *return_to;
        let mut visited = HashSet::new();
        while visited.insert(next) {
            let Some(node) = self.nodes.iter().find(|c| c.id() == next) else {
                return false;
            };
            for instruction in node.body() {
                match instruction {
                    I::Set { dst, value } if local(*dst) => {
                        values.insert(
                            *dst,
                            if *value == 0 {
                                Value::Zero
                            } else {
                                Value::Unknown
                            },
                        );
                    }
                    I::Copy { src, dst } if local(*src) && local(*dst) => {
                        values.insert(*dst, values.get(src).copied().unwrap_or(Value::Unknown));
                    }
                    I::AddConst { dst, value } if local(*dst) => {
                        if *value != 0 {
                            values.insert(*dst, Value::Unknown);
                        }
                    }
                    I::Transfer { src, targets }
                        if local(*src) && targets.iter().all(|t| local(t.dst) && t.dst != *src) =>
                    {
                        let input = values.get(src).copied().unwrap_or(Value::Unknown);
                        for target in targets {
                            let previous =
                                values.get(&target.dst).copied().unwrap_or(Value::Unknown);
                            let result = if target.factor == 0 || input == Value::Zero {
                                previous
                            } else if target.factor == 1 && previous == Value::Zero {
                                input
                            } else {
                                Value::Unknown
                            };
                            values.insert(target.dst, result);
                        }
                        values.insert(*src, Value::Zero);
                    }
                    _ => return false,
                }
            }
            match node.terminator() {
                Terminator::Goto { target } => next = *target,
                Terminator::Return { value: None } => return kind == ValueType::Void,
                Terminator::Return {
                    value: Some(ValueOperand::Cell(address)),
                } => {
                    return kind == ValueType::Cell && values.get(address) == Some(&Value::Result);
                }
                _ => return false,
            }
        }
        false
    }
}
