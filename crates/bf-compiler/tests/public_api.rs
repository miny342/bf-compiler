use bf_compiler::{
    Continuation, ContinuationId, ContinuationProgram, FunctionDescriptor, FunctionId,
    ProfileGranularity, Terminator, ValueType, compile_continuations,
    compile_continuations_unbounded, compile_continuations_unbounded_with_profile,
    compile_continuations_with_profile, lower_continuations, lower_source,
};

fn continuation_id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

#[test]
fn profile_artifact_has_the_same_brainfuck_as_normal_compilation() {
    let program = lower_source("void helper() {} void main() { helper(); output('x'); }").unwrap();
    let normal = compile_continuations(&program).unwrap();
    for granularity in [
        ProfileGranularity::Abi,
        ProfileGranularity::Continuation,
        ProfileGranularity::Instruction,
        ProfileGranularity::Source,
    ] {
        let profiled = compile_continuations_with_profile(&program, granularity).unwrap();
        assert_eq!(profiled.source, normal);
        profiled
            .map
            .validate_for_source(profiled.source.as_bytes())
            .unwrap();
        let keys: Vec<_> = profiled
            .map
            .sites
            .iter()
            .map(|site| site.stable_key.as_str())
            .collect();
        for key in [
            "abi.initialization",
            "abi.dispatcher",
            "abi.call",
            "abi.return",
        ] {
            assert!(keys.contains(&key), "missing profile site {key}");
        }
        assert_eq!(
            profiled
                .map
                .sites
                .iter()
                .any(|site| site.kind == "function"),
            granularity != ProfileGranularity::Abi
        );
        assert_eq!(
            profiled
                .map
                .sites
                .iter()
                .any(|site| site.kind == "frame_instruction"),
            matches!(
                granularity,
                ProfileGranularity::Instruction | ProfileGranularity::Source
            )
        );
    }
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
    let profiled =
        compile_continuations_unbounded_with_profile(&program, ProfileGranularity::Continuation)
            .unwrap();
    assert_eq!(
        profiled.source,
        compile_continuations_unbounded(&program).unwrap()
    );
    profiled
        .map
        .validate_for_source(profiled.source.as_bytes())
        .unwrap();
}

#[test]
fn abi_profile_separates_portal_phases() {
    let program = lower_source(
        "void main() { cell[2] values; cell index; values[index] = 1; output(values[index]); }",
    )
    .unwrap();
    let artifact = compile_continuations_with_profile(&program, ProfileGranularity::Abi).unwrap();
    let keys = artifact
        .map
        .sites
        .iter()
        .map(|site| site.stable_key.as_str())
        .collect::<Vec<_>>();
    for key in [
        "abi.portal.start",
        "abi.portal.accessor",
        "abi.portal.offset",
        "abi.portal.window.right",
        "abi.portal.load",
        "abi.portal.store",
        "abi.portal.resume",
    ] {
        assert!(keys.contains(&key), "missing profile site {key}");
    }
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
