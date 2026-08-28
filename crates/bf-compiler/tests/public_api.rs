use bf_compiler::{
    Continuation, ContinuationId, ContinuationProgram, FunctionDescriptor, FunctionId, Terminator,
    ValueType, compile_continuations, compile_continuations_unbounded, lower_continuations,
    lower_source,
};

fn continuation_id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

#[test]
fn unbounded_backend_accepts_a_self_host_sized_static_region() {
    let program = lower_source("cell[256][256] arena; void main() {}").unwrap();
    assert!(compile_continuations(&program).is_err());
    assert!(
        !compile_continuations_unbounded(&program)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn source_lowering_exposes_validated_continuation_ir() {
    let program: ContinuationProgram = lower_source("void main() {}").unwrap();

    assert_eq!(program.main(), FunctionId::new(0));
    assert_eq!(program.functions().len(), 1);
}

#[test]
fn public_continuation_ir_can_be_constructed_and_compiled() {
    let main = FunctionId::new(0);
    let entry = continuation_id(1);
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            0,
            ValueType::Void,
            entry,
        )],
        vec![Continuation::new(entry, main, vec![], Terminator::Halt)],
    )
    .unwrap();

    assert!(
        !lower_continuations(&program)
            .unwrap()
            .instructions()
            .is_empty()
    );
    assert!(!compile_continuations(&program).unwrap().is_empty());
}
