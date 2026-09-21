use bf_compiler::{
    Address, Continuation, ContinuationId, ContinuationProgram, FrameInstruction, FrameSlot,
    FunctionDescriptor, FunctionId, ProfileGranularity, Terminator, ValueType,
    compile_continuations, compile_continuations_unbounded,
    compile_continuations_unbounded_with_profile, compile_continuations_with_profile,
    lower_continuations, lower_source,
};

fn continuation_id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

#[test]
fn profile_artifact_preserves_brainfuck_and_separates_abi_phases() {
    let program = lower_source(
        "cell[2] values; void helper() {} \
         void main() { helper(); helper(); output('x'); \
         cell index; values[index] = 1; output(values[index]); }",
    )
    .unwrap();
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
            "abi.portal.start",
            "abi.portal.accessor",
            "abi.portal.payload.direct",
            "abi.portal.page",
            "abi.portal.resume",
            "abi.portal.router.global.0",
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
    let normal = compile_continuations_unbounded(&program).unwrap();
    assert!(!normal.is_empty());
    let profiled =
        compile_continuations_unbounded_with_profile(&program, ProfileGranularity::Continuation)
            .unwrap();
    assert_eq!(profiled.source, normal);
    profiled
        .map
        .validate_for_source(profiled.source.as_bytes())
        .unwrap();
}

#[test]
fn source_lowering_exposes_validated_continuation_ir() {
    let program: ContinuationProgram = lower_source("void main() {}").unwrap();

    assert_eq!(program.main(), FunctionId::new(0));
    assert_eq!(program.functions().len(), 1);
}

#[test]
fn unused_functions_are_removed_and_live_calls_are_remapped() {
    let program = lower_source(
        r"
        void unused_before() { unused_after(); }
        cell global = initialize();
        cell initialize() { return 'A'; }
        void unused_between() {}
        void main() { emit(global); emit(global); }
        void unused_after() { unused_before(); }
        void emit(cell value) { output(value); output(next(value)); }
        cell next(cell value) { return value + 1; }
        ",
    )
    .unwrap();

    assert_eq!(program.main(), FunctionId::new(1));
    assert_eq!(program.functions().len(), 4);
    for (index, function) in program.functions().iter().enumerate() {
        assert_eq!(function.id(), FunctionId::new(index));
    }
    let brainfuck = compile_continuations(&program).unwrap();
    assert_eq!(
        bf_interpreter::run(brainfuck.as_bytes(), b"").unwrap(),
        b"ABAB"
    );
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

#[test]
fn instruction_and_source_profiles_distinguish_same_kind_frame_instructions() {
    let main = FunctionId::new(0);
    let entry = continuation_id(1);
    let program = ContinuationProgram::new(
        main,
        vec![FunctionDescriptor::new(
            main,
            vec![],
            2,
            ValueType::Void,
            entry,
        )],
        vec![Continuation::new(
            entry,
            main,
            vec![
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(0)),
                    value: 1,
                },
                FrameInstruction::Set {
                    dst: Address::Frame(FrameSlot::new(1)),
                    value: 2,
                },
            ],
            Terminator::Halt,
        )],
    )
    .unwrap();

    for granularity in [ProfileGranularity::Instruction, ProfileGranularity::Source] {
        let artifact = compile_continuations_with_profile(&program, granularity).unwrap();
        let set_sites = artifact
            .map
            .sites
            .iter()
            .filter(|site| site.kind == "frame_instruction" && site.label == "set")
            .collect::<Vec<_>>();

        assert_eq!(set_sites.len(), 2);
        assert!(
            set_sites
                .iter()
                .any(|site| site.stable_key == "function.0.frame_instruction.0.set")
        );
        assert!(
            set_sites
                .iter()
                .any(|site| site.stable_key == "function.0.frame_instruction.1.set")
        );
    }
}
