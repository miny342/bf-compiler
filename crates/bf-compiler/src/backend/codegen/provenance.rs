//! Source and instruction provenance attached to emitted BF operations.

use super::*;

pub(super) fn instruction_kind(instruction: &FrameInstruction) -> &'static str {
    match instruction {
        FrameInstruction::SubWithBorrow { .. } => "sub_with_borrow",
        FrameInstruction::Compare { .. } => "compare",
        FrameInstruction::Set { .. } => "set",
        FrameInstruction::AddConst { .. } => "add_const",
        FrameInstruction::Copy { .. } => "copy",
        FrameInstruction::Transfer { .. } => "transfer",
        FrameInstruction::AggregateCopy { .. } => "aggregate_copy",
        FrameInstruction::Input { .. } => "input",
        FrameInstruction::Output { .. } => "output",
        FrameInstruction::Loop { .. } => "loop",
        FrameInstruction::Branch { .. } => "branch",
    }
}

pub(super) fn terminator_kind(terminator: &Terminator) -> &'static str {
    match terminator {
        Terminator::Goto { .. } => "goto",
        Terminator::Branch { .. } => "branch",
        Terminator::Call { .. } => "call",
        Terminator::Return { .. } => "return",
        Terminator::ArrayLoad { .. } => "array_load",
        Terminator::ArrayStore { .. } => "array_store",
        Terminator::AggregateLoad { .. } => "aggregate_load",
        Terminator::AggregateStore { .. } => "aggregate_store",
        Terminator::Abort => "abort",
        Terminator::Halt => "halt",
    }
}

impl<'a> AbiEmitter<'a> {
    pub(super) fn current_profile_site(&self) -> bf_profiling::ProfileSiteId {
        *self.site_stack.last().expect("root profile site")
    }

    pub(super) fn with_profile_site<T>(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        self.with_profile_attributes(kind, stable_key, label, BTreeMap::new(), emit)
    }

    pub(super) fn with_profile_attributes<T>(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        attributes: BTreeMap<String, String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        let site = self.sites.intern(
            Some(self.current_profile_site()),
            kind,
            stable_key,
            label,
            attributes,
        );
        if matches!(self.granularity, ProfileGranularity::Source) {
            self.sites.set_source(
                site,
                self.current_source
                    .map(|source| bf_profiling::ProfileSourceSpan {
                        file_id: source.file_id,
                        start_byte: source.start_byte,
                        end_byte: source.end_byte,
                    }),
            );
        }
        self.site_stack.push(site);
        let result = emit(self);
        self.site_stack.pop();
        result
    }

    pub(super) fn with_instruction_path<T>(
        &mut self,
        segment: impl Into<String>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        self.instruction_path.push(segment.into());
        let result = emit(self);
        self.instruction_path.pop();
        result
    }

    pub(super) fn with_source_span<T>(
        &mut self,
        source: Option<SourceSpan>,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        let previous = self.current_source;
        self.current_source = source;
        let result = emit(self);
        self.current_source = previous;
        result
    }

    pub(super) fn instruction_path_key(&self) -> String {
        self.instruction_path.join(".")
    }

    pub(super) fn emit_operation(&mut self, operation: AnnotatedBfOperation) {
        self.output.push(AnnotatedBfInstruction::new(
            self.current_profile_site(),
            operation,
        ));
    }

    pub(super) fn emit_loop(&mut self, body: Vec<AnnotatedBfInstruction>) {
        self.emit_operation(AnnotatedBfOperation::Loop(body));
    }

    pub(super) fn with_profile_site_infallible(
        &mut self,
        kind: &str,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        emit: impl FnOnce(&mut Self),
    ) {
        let site = self.sites.intern(
            Some(self.current_profile_site()),
            kind,
            stable_key,
            label,
            BTreeMap::new(),
        );
        if matches!(self.granularity, ProfileGranularity::Source) {
            self.sites.set_source(
                site,
                self.current_source
                    .map(|source| bf_profiling::ProfileSourceSpan {
                        file_id: source.file_id,
                        start_byte: source.start_byte,
                        end_byte: source.end_byte,
                    }),
            );
        }
        self.site_stack.push(site);
        emit(self);
        self.site_stack.pop();
    }

    pub(super) fn with_continuation_site(
        &mut self,
        continuation: &Continuation,
        emit: impl FnOnce(&mut Self) -> Result<(), AbiCodegenError>,
    ) -> Result<(), AbiCodegenError> {
        if matches!(self.granularity, ProfileGranularity::Abi) {
            emit(self)
        } else {
            let function = continuation.function().index();
            let continuation_id = continuation.id().get();
            self.with_source_span(continuation.primary_source(), |emitter| {
                emitter.with_profile_site(
                    "function",
                    format!("function.{function}"),
                    format!("function {function}"),
                    |emitter| {
                        emitter.with_profile_site(
                            "continuation",
                            format!("function.{function}.continuation.{continuation_id}"),
                            format!("continuation {continuation_id}"),
                            emit,
                        )
                    },
                )
            })
        }
    }

    pub(super) fn emit_instruction_site<T>(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
        emit: impl FnOnce(&mut Self) -> Result<T, AbiCodegenError>,
    ) -> Result<T, AbiCodegenError> {
        if matches!(
            self.granularity,
            ProfileGranularity::Abi | ProfileGranularity::Continuation
        ) {
            emit(self)
        } else if matches!(self.granularity, ProfileGranularity::Source)
            && let Some(source) = self.current_source
        {
            // Source maps are intended to answer which source operation is
            // hot, not to repeat a million frame-instruction leaves. Keep
            // the ABI child sites below this source site, but intern all
            // instructions from the same source span together.
            self.with_profile_site(
                "source",
                format!(
                    "source.{}.{}.{}",
                    source.file_id, source.start_byte, source.end_byte
                ),
                format!(
                    "source file {} bytes {}..{}",
                    source.file_id, source.start_byte, source.end_byte
                ),
                emit,
            )
        } else {
            self.with_profile_site(
                "frame_instruction",
                format!(
                    "function.{}.frame_instruction.{}.{}",
                    function.index(),
                    self.instruction_path_key(),
                    instruction_kind(instruction)
                ),
                instruction_kind(instruction),
                emit,
            )
        }
    }

    pub(super) fn emit_instruction_with_abi_site(
        &mut self,
        instruction: &FrameInstruction,
        function: FunctionId,
    ) -> Result<(), AbiCodegenError> {
        let kind = instruction_kind(instruction);
        self.with_profile_site(
            "abi",
            format!("abi.frame.{kind}"),
            format!("frame {kind}"),
            |emitter| emitter.emit_instruction_inner(instruction, function),
        )
    }
}
