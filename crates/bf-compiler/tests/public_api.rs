use bf_compiler::{
    Address, Continuation, ContinuationId, ContinuationProgram, FrameInstruction, FrameSlot,
    FunctionDescriptor, FunctionId, ProfileGranularity, SourceFile, Terminator, ValueType,
    compile_continuations, compile_continuations_unbounded,
    compile_continuations_unbounded_with_profile, compile_continuations_with_profile,
    lower_continuations, lower_source, lower_sources,
};

fn source_without_inline(source: &str) -> ContinuationProgram {
    bf_compiler::lower_source_with_options(
        source,
        bf_compiler::ContinuationOptimizationOptions {
            inline_functions: false,
            ..Default::default()
        },
    )
    .unwrap()
    .0
}

fn continuation_id(value: u16) -> ContinuationId {
    ContinuationId::new(value).unwrap()
}

#[test]
fn profile_artifact_preserves_brainfuck_and_separates_abi_phases() {
    let program = source_without_inline(
        "cell[2] values; cell index; void helper() { while (input() != 0) { values[index] = 1; } } \
         void main() { helper(); helper(); output('x'); \
         cell index; values[index] = 1; output(values[index]); }",
    );
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
            matches!(granularity, ProfileGranularity::Instruction)
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
    let program = source_without_inline(
        r"
        void unused_before() { unused_after(); }
        cell global = initialize();
        cell inline_marker;
        cell initialize() { return 'A'; }
        void unused_between() {}
        void main() { emit(global); emit(global); }
        void unused_after() { unused_before(); }
        void emit(cell value) { output(value); output(next(value)); while (0) { output(0); } }
        cell next(cell value) { cell result = value + 1; inline_marker += 1; return result; }
        ",
    );

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

#[test]
fn source_profile_maps_frame_and_global_backend_work_to_named_source() {
    let source = "cell global; void main() { global = input(); output(global); }";
    let program = lower_sources(&[SourceFile::new("main.bfc", source)]).unwrap();
    let artifact =
        compile_continuations_with_profile(&program, ProfileGranularity::Source).unwrap();

    assert_eq!(
        artifact.map.files,
        vec![bf_profiling::ProfileFile {
            id: 0,
            path: "main.bfc".into(),
        }]
    );
    let source_sites = artifact
        .map
        .sites
        .iter()
        .filter_map(|site| site.source.as_ref())
        .collect::<Vec<_>>();
    assert!(!source_sites.is_empty());
    assert!(source_sites.iter().all(|span| {
        span.file_id == 0 && span.start_byte < span.end_byte && span.end_byte <= source.len() as u64
    }));
    assert!(
        artifact
            .map
            .sites
            .iter()
            .any(|site| { site.stable_key == "abi.navigation.global" && site.source.is_some() })
    );
    assert!(
        artifact
            .map
            .sites
            .iter()
            .any(|site| site.kind == "source" && site.source.is_some())
    );
    assert!(
        !artifact
            .map
            .sites
            .iter()
            .any(|site| site.kind == "frame_instruction")
    );
    artifact
        .map
        .validate_for_source(artifact.source.as_bytes())
        .unwrap();
    let embedded = artifact.embedded_source().unwrap();
    assert_eq!(
        bf_profiling::embedded_profile_map(embedded.as_bytes())
            .unwrap()
            .unwrap(),
        artifact.map
    );
}
