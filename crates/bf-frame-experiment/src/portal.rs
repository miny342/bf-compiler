//! Pointer-relative array portal and migrating continuation-dispatch probe.

use crate::ChunkLayout;

const PORTAL_CELLS: usize = 16;

const VALUE: usize = 0;
const RUN: usize = 1;
const PC_LOW: usize = 2;
const PC_HIGH: usize = 3;
const NEXT_PC_LOW: usize = 4;
const NEXT_PC_HIGH: usize = 5;
const CONDITION: usize = 6;
const RESTORE: usize = 7;
const BRANCH: usize = 8;
const INDEX: usize = 9;
const RETURN_PC_LOW: usize = 10;
const RETURN_PC_HIGH: usize = 11;
const QUOTIENT: usize = 12;
const REMAINDER: usize = 13;
const PHASE: usize = 14;
const ZERO_FLAG: usize = 15;

const CALL_LOCAL: u16 = 1;
const ARRAY_COPY: u16 = 2;
const RESUME_LOCAL: u16 = 3;
const CALL_GLOBAL: u16 = 4;
const RESUME_GLOBAL: u16 = 5;
const CALL_LOCAL_STORE: u16 = 6;
const ARRAY_STORE: u16 = 7;
const RESUME_LOCAL_STORE: u16 = 8;
const CALL_LOCAL_RELOAD: u16 = 9;
const RESUME_LOCAL_RELOAD: u16 = 10;
const CALL_GLOBAL_STORE: u16 = 11;
const RESUME_GLOBAL_STORE: u16 = 12;
const CALL_GLOBAL_RELOAD: u16 = 13;
const RESUME_GLOBAL_RELOAD: u16 = 14;
const HALT: u16 = 0x0106;

/// Metadata and generated source for the pointer-relative portal probe.
#[derive(Debug, Clone)]
pub struct PortalProbe {
    pub source: String,
    pub array_length: usize,
    pub portal_chunks: usize,
    pub array_chunks: usize,
}

/// Whole-domain measurements for a portal probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalMeasurements {
    pub source_bytes: usize,
    pub array_length: usize,
    pub min_steps: u64,
    pub mean_steps: u64,
    pub max_steps: u64,
    pub max_pointer: usize,
}

/// Metadata for the recursive call/return probe.
#[derive(Debug, Clone)]
pub struct CallProbe {
    pub source: String,
    pub frame_chunks: usize,
}

/// Metadata for the recursive aggregate-return probe.
#[derive(Debug, Clone)]
pub struct AggregateProbe {
    pub source: String,
    pub frame_chunks: usize,
    pub result_cells: usize,
}

/// Metadata for the differently-sized mutual-recursion probe.
#[derive(Debug, Clone)]
pub struct MutualCallProbe {
    pub source: String,
    pub a_frame_chunks: usize,
    pub b_frame_chunks: usize,
}

const PARAMETER: usize = 12;
const ROOT_ENTRY: u16 = 1;
const FUNCTION_ENTRY: u16 = 2;
const FUNCTION_BASE: u16 = 3;
const FUNCTION_RECURSE: u16 = 4;
const FUNCTION_AFTER_RECURSE: u16 = 5;
const FUNCTION_RETURN: u16 = 6;
const ROOT_RESUME: u16 = 7;
const CALL_HALT: u16 = 0x0108;

const AGG_ROOT_ENTRY: u16 = 1;
const AGG_FUNCTION_ENTRY: u16 = 2;
const AGG_FUNCTION_BASE: u16 = 3;
const AGG_FUNCTION_RECURSE: u16 = 4;
const AGG_FUNCTION_AFTER_RECURSE: u16 = 5;
const AGG_FUNCTION_RETURN: u16 = 6;
const AGG_ROOT_RESUME: u16 = 7;
const AGG_HALT: u16 = 0x0208;

const MUTUAL_ROOT_ENTRY: u16 = 1;
const A_ENTRY: u16 = 2;
const A_BASE: u16 = 3;
const A_RECURSE: u16 = 4;
const A_AFTER: u16 = 5;
const A_RETURN: u16 = 6;
const B_ENTRY: u16 = 7;
const B_BASE: u16 = 8;
const B_RECURSE: u16 = 9;
const B_AFTER: u16 = 10;
const B_RETURN: u16 = 11;
const MUTUAL_ROOT_RESUME: u16 = 12;
const MUTUAL_HALT: u16 = 0x030d;

/// Builds a direct-recursion probe. For input `n`, the function recursively
/// returns `n + 1`, exercising a distinct activation and return PC per depth.
pub fn build_call_probe<const CELLS: usize>() -> CallProbe {
    assert!(CELLS == 8 || CELLS == 16);
    let stride = ChunkLayout::<CELLS>::STRIDE;
    let frame_chunks = PORTAL_CELLS / CELLS;
    let anchor = 4;
    let root_base = anchor + stride;

    let mut init = AbsoluteEmitter::default();
    for chunk in 0..frame_chunks {
        init.set(root_base + chunk * stride, 1);
    }
    init.set(root_base + field_offset::<CELLS>(RUN), 1);
    init.set(root_base + field_offset::<CELLS>(PC_LOW), ROOT_ENTRY as u8);
    init.move_to(root_base + field_offset::<CELLS>(RUN));

    let mut dispatch = RelativeEmitter::<CELLS>::new(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push('[');
    dispatch.move_to(0);

    dispatch.dispatch_case(ROOT_ENTRY, |emitter| {
        emitter.input_field(PARAMETER);
        emitter.call_frame(frame_chunks, PARAMETER, false, FUNCTION_ENTRY, ROOT_RESUME);
    });
    dispatch.dispatch_case(FUNCTION_ENTRY, |emitter| {
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, FUNCTION_RECURSE);
        emitter.if_field_equals(PARAMETER, 0, |emitter| {
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, FUNCTION_BASE);
        });
    });
    dispatch.dispatch_case(FUNCTION_BASE, |emitter| {
        emitter.set_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, FUNCTION_RETURN);
    });
    dispatch.dispatch_case(FUNCTION_RECURSE, |emitter| {
        emitter.call_frame(
            frame_chunks,
            PARAMETER,
            true,
            FUNCTION_ENTRY,
            FUNCTION_AFTER_RECURSE,
        );
    });
    dispatch.dispatch_case(FUNCTION_AFTER_RECURSE, |emitter| {
        emitter.add_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, FUNCTION_RETURN);
    });
    dispatch.dispatch_case(FUNCTION_RETURN, |emitter| {
        emitter.return_scalar(frame_chunks);
    });
    dispatch.dispatch_case(ROOT_RESUME, |emitter| {
        emitter.output_field(VALUE);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, CALL_HALT);
    });
    dispatch.dispatch_case(CALL_HALT, |emitter| {
        emitter.clear_field(RUN);
    });

    dispatch.move_field(NEXT_PC_LOW, PC_LOW);
    dispatch.move_field(NEXT_PC_HIGH, PC_HIGH);
    dispatch.move_to(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push(']');
    init.source.push_str(&dispatch.source);

    CallProbe {
        source: init.source,
        frame_chunks,
    }
}

/// Builds a recursive aggregate-return probe with a two-chunk caller outbox.
/// Each activation receives its child's result in its own outbox before
/// moving the updated aggregate into its parent's outbox.
pub fn build_aggregate_probe<const CELLS: usize>() -> AggregateProbe {
    assert!(CELLS == 8 || CELLS == 16);
    let stride = ChunkLayout::<CELLS>::STRIDE;
    let portal_chunks = PORTAL_CELLS / CELLS;
    let outbox_chunks = 2;
    let frame_chunks = portal_chunks + outbox_chunks;
    let result_cells = CELLS + 3;
    let anchor = 4;
    let root_bottom = anchor + stride;
    let root_base = root_bottom + outbox_chunks * stride;

    let mut init = AbsoluteEmitter::default();
    for chunk in 0..frame_chunks {
        init.set(root_bottom + chunk * stride, 1);
    }
    init.set(root_base + field_offset::<CELLS>(RUN), 1);
    init.set(
        root_base + field_offset::<CELLS>(PC_LOW),
        AGG_ROOT_ENTRY as u8,
    );
    init.move_to(root_base + field_offset::<CELLS>(RUN));

    let mut dispatch = RelativeEmitter::<CELLS>::new(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push('[');
    dispatch.move_to(0);

    dispatch.dispatch_case(AGG_ROOT_ENTRY, |emitter| {
        emitter.input_field(PARAMETER);
        emitter.call_frame(
            frame_chunks,
            PARAMETER,
            false,
            AGG_FUNCTION_ENTRY,
            AGG_ROOT_RESUME,
        );
    });
    dispatch.dispatch_case(AGG_FUNCTION_ENTRY, |emitter| {
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, AGG_FUNCTION_RECURSE);
        emitter.if_field_equals(PARAMETER, 0, |emitter| {
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, AGG_FUNCTION_BASE);
        });
    });
    dispatch.dispatch_case(AGG_FUNCTION_BASE, |emitter| {
        emitter.build_parent_outbox(frame_chunks, outbox_chunks, result_cells);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, AGG_FUNCTION_RETURN);
    });
    dispatch.dispatch_case(AGG_FUNCTION_RECURSE, |emitter| {
        emitter.call_frame(
            frame_chunks,
            PARAMETER,
            true,
            AGG_FUNCTION_ENTRY,
            AGG_FUNCTION_AFTER_RECURSE,
        );
    });
    dispatch.dispatch_case(AGG_FUNCTION_AFTER_RECURSE, |emitter| {
        emitter.increment_outbox(outbox_chunks, result_cells);
        emitter.move_outbox_to_parent(frame_chunks, outbox_chunks, result_cells);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, AGG_FUNCTION_RETURN);
    });
    dispatch.dispatch_case(AGG_FUNCTION_RETURN, |emitter| {
        emitter.return_aggregate(frame_chunks);
    });
    dispatch.dispatch_case(AGG_ROOT_RESUME, |emitter| {
        emitter.output_outbox(outbox_chunks, result_cells);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, AGG_HALT);
    });
    dispatch.dispatch_case(AGG_HALT, |emitter| {
        emitter.clear_field(RUN);
    });

    dispatch.move_field(NEXT_PC_LOW, PC_LOW);
    dispatch.move_field(NEXT_PC_HIGH, PC_HIGH);
    dispatch.move_to(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push(']');
    init.source.push_str(&dispatch.source);

    AggregateProbe {
        source: init.source,
        frame_chunks,
        result_cells,
    }
}

/// Builds mutually recursive functions A and B with different frame sizes.
/// For input `n`, alternating calls return `n + 1`.
pub fn build_mutual_call_probe<const CELLS: usize>() -> MutualCallProbe {
    assert!(CELLS == 8 || CELLS == 16);
    let stride = ChunkLayout::<CELLS>::STRIDE;
    let portal_chunks = PORTAL_CELLS / CELLS;
    let a_frame_chunks = portal_chunks + 1;
    let b_frame_chunks = portal_chunks + 2;
    let anchor = 4;
    let root_base = anchor + stride;

    let mut init = AbsoluteEmitter::default();
    for chunk in 0..portal_chunks {
        init.set(root_base + chunk * stride, 1);
    }
    init.set(root_base + field_offset::<CELLS>(RUN), 1);
    init.set(
        root_base + field_offset::<CELLS>(PC_LOW),
        MUTUAL_ROOT_ENTRY as u8,
    );
    init.move_to(root_base + field_offset::<CELLS>(RUN));

    let mut dispatch = RelativeEmitter::<CELLS>::new(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push('[');
    dispatch.move_to(0);

    dispatch.dispatch_case(MUTUAL_ROOT_ENTRY, |emitter| {
        emitter.input_field(PARAMETER);
        emitter.call_frame(
            a_frame_chunks,
            PARAMETER,
            false,
            A_ENTRY,
            MUTUAL_ROOT_RESUME,
        );
    });
    dispatch.dispatch_case(A_ENTRY, |emitter| {
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, A_RECURSE);
        emitter.if_field_equals(PARAMETER, 0, |emitter| {
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, A_BASE);
        });
    });
    dispatch.dispatch_case(A_BASE, |emitter| {
        emitter.set_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, A_RETURN);
    });
    dispatch.dispatch_case(A_RECURSE, |emitter| {
        emitter.call_frame(b_frame_chunks, PARAMETER, true, B_ENTRY, A_AFTER);
    });
    dispatch.dispatch_case(A_AFTER, |emitter| {
        emitter.add_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, A_RETURN);
    });
    dispatch.dispatch_case(A_RETURN, |emitter| {
        emitter.return_scalar(a_frame_chunks);
    });
    dispatch.dispatch_case(B_ENTRY, |emitter| {
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, B_RECURSE);
        emitter.if_field_equals(PARAMETER, 0, |emitter| {
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, B_BASE);
        });
    });
    dispatch.dispatch_case(B_BASE, |emitter| {
        emitter.set_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, B_RETURN);
    });
    dispatch.dispatch_case(B_RECURSE, |emitter| {
        emitter.call_frame(a_frame_chunks, PARAMETER, true, A_ENTRY, B_AFTER);
    });
    dispatch.dispatch_case(B_AFTER, |emitter| {
        emitter.add_field(VALUE, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, B_RETURN);
    });
    dispatch.dispatch_case(B_RETURN, |emitter| {
        emitter.return_scalar(b_frame_chunks);
    });
    dispatch.dispatch_case(MUTUAL_ROOT_RESUME, |emitter| {
        emitter.output_field(VALUE);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, MUTUAL_HALT);
    });
    dispatch.dispatch_case(MUTUAL_HALT, |emitter| {
        emitter.clear_field(RUN);
    });

    dispatch.move_field(NEXT_PC_LOW, PC_LOW);
    dispatch.move_field(NEXT_PC_HIGH, PC_HIGH);
    dispatch.move_to(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push(']');
    init.source.push_str(&dispatch.source);

    MutualCallProbe {
        source: init.source,
        a_frame_chunks,
        b_frame_chunks,
    }
}

/// Executes every valid index and summarizes generated size and BF steps.
pub fn measure_portal_probe<const CELLS: usize>() -> PortalMeasurements {
    measure_portal_probe_for_length::<CELLS>(CELLS * 2)
}

/// Measures a portal probe at an explicit source-level array length.
pub fn measure_portal_probe_for_length<const CELLS: usize>(
    array_length: usize,
) -> PortalMeasurements {
    let probe = build_portal_probe_for_length::<CELLS>(array_length);
    let mut min_steps = u64::MAX;
    let mut max_steps = 0;
    let mut total_steps = 0_u128;
    let mut max_pointer = 0;
    for index in 0..probe.array_length {
        let result =
            bf_interpreter::run_with_stats(probe.source.as_bytes(), &[index as u8, index as u8])
                .expect("generated portal probe must execute");
        let mut expected = vec![
            10_u8.wrapping_add(index as u8),
            100_u8.wrapping_add(index as u8),
        ];
        expected.extend((0..probe.array_chunks).map(|chunk| 201_u8.wrapping_add(chunk as u8)));
        assert_eq!(result.output, expected, "portal measurement index {index}");
        min_steps = min_steps.min(result.stats.executed_instructions);
        max_steps = max_steps.max(result.stats.executed_instructions);
        total_steps += u128::from(result.stats.executed_instructions);
        max_pointer = max_pointer.max(result.stats.max_pointer);
    }
    PortalMeasurements {
        source_bytes: probe.source.len(),
        array_length: probe.array_length,
        min_steps,
        mean_steps: (total_steps / probe.array_length as u128) as u64,
        max_steps,
        max_pointer,
    }
}

/// Builds a program whose single `ARRAY_COPY` continuation is entered once
/// for a local array and once for a global array.
///
/// The program consumes the same index twice and emits the selected local and
/// global values followed by every preserved global aux head.
pub fn build_portal_probe<const CELLS: usize>() -> PortalProbe {
    build_portal_probe_for_length::<CELLS>(CELLS * 2)
}

/// Builds a portal probe that stores and reloads every selected local/global
/// element in addition to the load checks.
pub fn build_portal_store_probe_for_length<const CELLS: usize>(array_length: usize) -> PortalProbe {
    build_portal_probe_impl::<CELLS>(array_length, true)
}

/// Builds the pointer-relative portal probe at an explicit array length.
pub fn build_portal_probe_for_length<const CELLS: usize>(array_length: usize) -> PortalProbe {
    build_portal_probe_impl::<CELLS>(array_length, false)
}

fn build_portal_probe_impl<const CELLS: usize>(
    array_length: usize,
    include_store: bool,
) -> PortalProbe {
    assert!(
        CELLS >= 8,
        "the portal protocol requires at least eight cells"
    );
    assert_eq!(
        PORTAL_CELLS % CELLS,
        0,
        "the probe expects portal cells to fill complete chunks"
    );
    assert!(array_length > 0, "the probe array must not be empty");
    assert!(array_length <= 256, "cell index must cover the probe array");

    let stride = ChunkLayout::<CELLS>::STRIDE;
    let portal_chunks = PORTAL_CELLS / CELLS;
    let payload_chunks = array_length.div_ceil(CELLS);
    let global_base = 4;
    let array_chunks = portal_chunks + payload_chunks;
    let anchor = global_base + array_chunks * stride;
    let first_stack_head = anchor + stride;
    let local_base = first_stack_head;
    let frame_base = local_base + array_chunks * stride;
    let frame_chunks = portal_chunks;
    let frontier = frame_base + frame_chunks * stride;

    let mut init = AbsoluteEmitter::default();

    // Global heads are live aux cells and must survive both accesses.
    for chunk in 0..array_chunks {
        init.set(
            global_base + chunk * stride,
            201_u8.wrapping_add(chunk as u8),
        );
    }
    for element in 0..array_length {
        let position =
            global_base + ChunkLayout::<CELLS>::cell_offset_from_head(PORTAL_CELLS + element);
        init.set(position, 100_u8.wrapping_add(element as u8));
    }

    // One root activation owns a local array portal followed by a frame
    // dispatch portal. Every allocated head remains one.
    for chunk in 0..array_chunks + frame_chunks {
        init.set(first_stack_head + chunk * stride, 1);
    }
    for element in 0..array_length {
        let position =
            local_base + ChunkLayout::<CELLS>::cell_offset_from_head(PORTAL_CELLS + element);
        init.set(position, 10_u8.wrapping_add(element as u8));
    }

    init.set(frame_base + field_offset::<CELLS>(RUN), 1);
    init.set(frame_base + field_offset::<CELLS>(PC_LOW), CALL_LOCAL as u8);
    init.move_to(frame_base + field_offset::<CELLS>(RUN));

    let frame_to_local = -((array_chunks * stride) as isize);
    let anchor_from_global = anchor - global_base;
    let mut dispatch = RelativeEmitter::<CELLS>::new(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push('[');
    dispatch.move_to(0);

    dispatch.dispatch_case(CALL_LOCAL, |emitter| {
        emitter.migrate_by(frame_to_local);
        emitter.prepare_array_call(ARRAY_COPY, RESUME_LOCAL);
        emitter.input_field(INDEX);
    });
    dispatch.dispatch_case(ARRAY_COPY, |emitter| {
        emitter.array_load_chunked(array_length);
        emitter.copy_field(RETURN_PC_LOW, NEXT_PC_LOW);
        emitter.copy_field(RETURN_PC_HIGH, NEXT_PC_HIGH);
    });
    dispatch.dispatch_case(RESUME_LOCAL, |emitter| {
        emitter.output_field(VALUE);
        emitter.clear_protocol();
        emitter.migrate_local_array_to_frame(frame_chunks);
        emitter.set_field(RUN, 1);
        emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, CALL_GLOBAL);
    });
    dispatch.dispatch_case(CALL_GLOBAL, |emitter| {
        emitter.migrate_frame_to_global(frame_chunks, anchor_from_global);
        emitter.prepare_array_call(ARRAY_COPY, RESUME_GLOBAL);
        emitter.input_field(INDEX);
    });
    dispatch.dispatch_case(RESUME_GLOBAL, |emitter| {
        emitter.output_field(VALUE);
        emitter.clear_protocol();
        emitter.migrate_global_to_frame(anchor_from_global, frame_chunks);
        emitter.set_field(RUN, 1);
        emitter.set_pc(
            NEXT_PC_LOW,
            NEXT_PC_HIGH,
            if include_store {
                CALL_LOCAL_STORE
            } else {
                HALT
            },
        );
    });
    if include_store {
        dispatch.dispatch_case(CALL_LOCAL_STORE, |emitter| {
            emitter.migrate_by(frame_to_local);
            emitter.prepare_array_call(ARRAY_STORE, RESUME_LOCAL_STORE);
            emitter.input_field(INDEX);
            emitter.set_field(VALUE, 77);
        });
        dispatch.dispatch_case(ARRAY_STORE, |emitter| {
            emitter.array_store_chunked(array_length);
            emitter.copy_field(RETURN_PC_LOW, NEXT_PC_LOW);
            emitter.copy_field(RETURN_PC_HIGH, NEXT_PC_HIGH);
        });
        dispatch.dispatch_case(RESUME_LOCAL_STORE, |emitter| {
            emitter.clear_protocol();
            emitter.migrate_local_array_to_frame(frame_chunks);
            emitter.set_field(RUN, 1);
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, CALL_LOCAL_RELOAD);
        });
        dispatch.dispatch_case(CALL_LOCAL_RELOAD, |emitter| {
            emitter.migrate_by(frame_to_local);
            emitter.prepare_array_call(ARRAY_COPY, RESUME_LOCAL_RELOAD);
            emitter.input_field(INDEX);
        });
        dispatch.dispatch_case(RESUME_LOCAL_RELOAD, |emitter| {
            emitter.output_field(VALUE);
            emitter.clear_protocol();
            emitter.migrate_local_array_to_frame(frame_chunks);
            emitter.set_field(RUN, 1);
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, CALL_GLOBAL_STORE);
        });
        dispatch.dispatch_case(CALL_GLOBAL_STORE, |emitter| {
            emitter.migrate_frame_to_global(frame_chunks, anchor_from_global);
            emitter.prepare_array_call(ARRAY_STORE, RESUME_GLOBAL_STORE);
            emitter.input_field(INDEX);
            emitter.set_field(VALUE, 88);
        });
        dispatch.dispatch_case(RESUME_GLOBAL_STORE, |emitter| {
            emitter.clear_protocol();
            emitter.migrate_global_to_frame(anchor_from_global, frame_chunks);
            emitter.set_field(RUN, 1);
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, CALL_GLOBAL_RELOAD);
        });
        dispatch.dispatch_case(CALL_GLOBAL_RELOAD, |emitter| {
            emitter.migrate_frame_to_global(frame_chunks, anchor_from_global);
            emitter.prepare_array_call(ARRAY_COPY, RESUME_GLOBAL_RELOAD);
            emitter.input_field(INDEX);
        });
        dispatch.dispatch_case(RESUME_GLOBAL_RELOAD, |emitter| {
            emitter.output_field(VALUE);
            emitter.clear_protocol();
            emitter.migrate_global_to_frame(anchor_from_global, frame_chunks);
            emitter.set_field(RUN, 1);
            emitter.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, HALT);
        });
    }
    dispatch.dispatch_case(HALT, |emitter| {
        emitter.clear_field(RUN);
    });

    dispatch.move_field(NEXT_PC_LOW, PC_LOW);
    dispatch.move_field(NEXT_PC_HIGH, PC_HIGH);
    dispatch.move_to(field_offset::<CELLS>(RUN) as isize);
    dispatch.source.push(']');

    // HALT always returns the moving dispatcher to the root frame portal.
    dispatch.position = field_offset::<CELLS>(RUN) as isize;
    for chunk in 0..array_chunks {
        dispatch.move_to(global_base as isize + (chunk * stride) as isize - frame_base as isize);
        dispatch.source.push('.');
    }
    dispatch.move_to(0);

    init.source.push_str(&dispatch.source);
    debug_assert_eq!(frontier, frame_base + portal_chunks * stride);
    PortalProbe {
        source: init.source,
        array_length,
        portal_chunks,
        array_chunks,
    }
}

fn field_offset<const CELLS: usize>(field: usize) -> usize {
    ChunkLayout::<CELLS>::cell_offset_from_head(field)
}

#[derive(Default)]
struct AbsoluteEmitter {
    source: String,
    position: usize,
}

impl AbsoluteEmitter {
    fn set(&mut self, position: usize, value: u8) {
        self.move_to(position);
        self.source.push_str("[-]");
        adjust(&mut self.source, value);
    }

    fn move_to(&mut self, destination: usize) {
        if destination >= self.position {
            self.source
                .extend(std::iter::repeat_n('>', destination - self.position));
        } else {
            self.source
                .extend(std::iter::repeat_n('<', self.position - destination));
        }
        self.position = destination;
    }
}

struct RelativeEmitter<const CELLS: usize> {
    source: String,
    /// Position relative to whichever dispatch context is currently active.
    position: isize,
}

impl<const CELLS: usize> RelativeEmitter<CELLS> {
    fn new(position: isize) -> Self {
        Self {
            source: String::new(),
            position,
        }
    }

    fn dispatch_case(&mut self, id: u16, body: impl FnOnce(&mut Self)) {
        self.copy_field(PC_LOW, CONDITION);
        self.add_field(CONDITION, 0_u8.wrapping_sub(id as u8));
        self.set_field(BRANCH, 1);

        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push('[');
        self.source.push_str("[-]");
        self.clear_field(BRANCH);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push(']');
        self.move_to(0);

        // The low-byte equality left BRANCH at one only on a match. A
        // non-matching high byte clears it, producing a 16-bit equality.
        self.copy_field(PC_HIGH, CONDITION);
        self.add_field(CONDITION, 0_u8.wrapping_sub((id >> 8) as u8));
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push('[');
        self.source.push_str("[-]");
        self.clear_field(BRANCH);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push(']');
        self.move_to(0);

        self.move_to(field_offset::<CELLS>(BRANCH) as isize);
        self.source.push('[');
        self.source.push('-');
        self.move_to(0);
        self.clear_field(PC_LOW);
        self.clear_field(PC_HIGH);
        body(self);
        assert_eq!(self.position, 0, "continuation must finish at context base");
        self.move_to(field_offset::<CELLS>(BRANCH) as isize);
        self.source.push(']');
        self.move_to(0);
    }

    fn prepare_array_call(&mut self, accessor_pc: u16, return_pc: u16) {
        for field in [
            VALUE,
            RUN,
            PC_LOW,
            PC_HIGH,
            NEXT_PC_LOW,
            NEXT_PC_HIGH,
            CONDITION,
            RESTORE,
            BRANCH,
            INDEX,
            RETURN_PC_LOW,
            RETURN_PC_HIGH,
            QUOTIENT,
            REMAINDER,
            PHASE,
            ZERO_FLAG,
        ] {
            self.clear_field(field);
        }
        self.set_field(RUN, 1);
        self.set_pc(NEXT_PC_LOW, NEXT_PC_HIGH, accessor_pc);
        self.set_pc(RETURN_PC_LOW, RETURN_PC_HIGH, return_pc);
    }

    fn array_load_chunked(&mut self, length: usize) {
        self.clear_field(VALUE);
        self.compute_index_divmod();
        let chunks = length.div_ceil(CELLS);
        for chunk in 0..chunks {
            self.if_field_equals(QUOTIENT, chunk as u8, |emitter| {
                let chunk_start = chunk * CELLS;
                let chunk_length = (length - chunk_start).min(CELLS);
                for within in 0..chunk_length {
                    emitter.if_field_equals(REMAINDER, within as u8, |emitter| {
                        let source = ChunkLayout::<CELLS>::cell_offset_from_head(
                            PORTAL_CELLS + chunk_start + within,
                        );
                        emitter.copy(source as isize, field_offset::<CELLS>(VALUE) as isize);
                    });
                }
            });
        }
    }

    fn array_store_chunked(&mut self, length: usize) {
        self.compute_index_divmod();
        let chunks = length.div_ceil(CELLS);
        for chunk in 0..chunks {
            self.if_field_equals(QUOTIENT, chunk as u8, |emitter| {
                let chunk_start = chunk * CELLS;
                let chunk_length = (length - chunk_start).min(CELLS);
                for within in 0..chunk_length {
                    emitter.if_field_equals(REMAINDER, within as u8, |emitter| {
                        let destination = ChunkLayout::<CELLS>::cell_offset_from_head(
                            PORTAL_CELLS + chunk_start + within,
                        );
                        emitter.move_between(
                            field_offset::<CELLS>(VALUE) as isize,
                            destination as isize,
                        );
                    });
                }
            });
        }
    }

    fn compute_index_divmod(&mut self) {
        for field in [QUOTIENT, REMAINDER, PHASE, ZERO_FLAG] {
            self.clear_field(field);
        }
        self.set_field(PHASE, CELLS as u8);

        self.move_to(field_offset::<CELLS>(INDEX) as isize);
        self.source.push('[');
        self.source.push('-');
        self.move_to(0);
        self.add_field(REMAINDER, 1);
        self.add_field(PHASE, u8::MAX);

        self.copy_field(PHASE, CONDITION);
        self.set_field(ZERO_FLAG, 1);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push('[');
        self.source.push_str("[-]");
        self.clear_field(ZERO_FLAG);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push(']');
        self.move_to(0);

        self.move_to(field_offset::<CELLS>(ZERO_FLAG) as isize);
        self.source.push('[');
        self.source.push('-');
        self.move_to(0);
        self.add_field(QUOTIENT, 1);
        self.clear_field(REMAINDER);
        self.set_field(PHASE, CELLS as u8);
        self.move_to(field_offset::<CELLS>(ZERO_FLAG) as isize);
        self.source.push(']');
        self.move_to(field_offset::<CELLS>(INDEX) as isize);
        self.source.push(']');
        self.move_to(0);
    }

    fn if_field_equals(&mut self, field: usize, value: u8, body: impl FnOnce(&mut Self)) {
        self.copy_field(field, CONDITION);
        self.add_field(CONDITION, 0_u8.wrapping_sub(value));
        self.set_field(BRANCH, 1);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push('[');
        self.source.push_str("[-]");
        self.clear_field(BRANCH);
        self.move_to(field_offset::<CELLS>(CONDITION) as isize);
        self.source.push(']');
        self.move_to(0);

        self.move_to(field_offset::<CELLS>(BRANCH) as isize);
        self.source.push('[');
        self.source.push('-');
        self.move_to(0);
        body(self);
        assert_eq!(self.position, 0);
        self.move_to(field_offset::<CELLS>(BRANCH) as isize);
        self.source.push(']');
        self.move_to(0);
    }

    fn clear_protocol(&mut self) {
        for field in [
            VALUE,
            RUN,
            PC_LOW,
            PC_HIGH,
            NEXT_PC_LOW,
            NEXT_PC_HIGH,
            CONDITION,
            RESTORE,
            BRANCH,
            INDEX,
            RETURN_PC_LOW,
            RETURN_PC_HIGH,
            QUOTIENT,
            REMAINDER,
            PHASE,
            ZERO_FLAG,
        ] {
            self.clear_field(field);
        }
    }

    fn call_frame(
        &mut self,
        frame_chunks: usize,
        parameter_field: usize,
        decrement_parameter: bool,
        entry_pc: u16,
        return_pc: u16,
    ) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        let context_delta = (frame_chunks * stride) as isize;
        let portal_chunks = PORTAL_CELLS / CELLS;
        let caller_frontier = (portal_chunks * stride) as isize;

        // The previous return cleared every data cell. Allocation therefore
        // only needs to mark the known number of chunk heads as live.
        for chunk in 0..frame_chunks {
            self.set(caller_frontier + (chunk * stride) as isize, 1);
        }
        for field in 0..PORTAL_CELLS {
            self.clear(context_delta + field_offset::<CELLS>(field) as isize);
        }
        self.set(context_delta + field_offset::<CELLS>(RUN) as isize, 1);
        self.set_pc_at(context_delta, NEXT_PC_LOW, NEXT_PC_HIGH, entry_pc);
        self.set_pc_at(context_delta, RETURN_PC_LOW, RETURN_PC_HIGH, return_pc);
        self.copy(
            field_offset::<CELLS>(parameter_field) as isize,
            context_delta + field_offset::<CELLS>(PARAMETER) as isize,
        );
        if decrement_parameter {
            self.move_to(context_delta + field_offset::<CELLS>(PARAMETER) as isize);
            self.source.push('-');
            self.move_to(0);
        }
        self.migrate_by(context_delta);
    }

    fn return_scalar(&mut self, frame_chunks: usize) {
        self.return_frame(frame_chunks, true);
    }

    fn return_aggregate(&mut self, frame_chunks: usize) {
        self.return_frame(frame_chunks, false);
    }

    fn return_frame(&mut self, frame_chunks: usize, transfer_scalar: bool) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        let caller_delta = -((frame_chunks * stride) as isize);

        if transfer_scalar {
            self.move_between(
                field_offset::<CELLS>(VALUE) as isize,
                caller_delta + field_offset::<CELLS>(VALUE) as isize,
            );
        }
        self.move_between(
            field_offset::<CELLS>(RETURN_PC_LOW) as isize,
            caller_delta + field_offset::<CELLS>(NEXT_PC_LOW) as isize,
        );
        self.move_between(
            field_offset::<CELLS>(RETURN_PC_HIGH) as isize,
            caller_delta + field_offset::<CELLS>(NEXT_PC_HIGH) as isize,
        );
        self.set(caller_delta + field_offset::<CELLS>(RUN) as isize, 1);

        let portal_chunks = PORTAL_CELLS / CELLS;
        let frame_bottom = -(((frame_chunks - portal_chunks) * stride) as isize);
        for chunk in 0..frame_chunks {
            let head = frame_bottom + (chunk * stride) as isize;
            self.clear(head);
            for data in 0..CELLS {
                self.clear(head + 1 + data as isize);
            }
        }
        self.migrate_by(caller_delta);
    }

    fn build_parent_outbox(&mut self, frame_chunks: usize, outbox_chunks: usize, cells: usize) {
        let caller_delta = -((frame_chunks * ChunkLayout::<CELLS>::STRIDE) as isize);
        for element in 0..cells {
            let destination = caller_delta + self.outbox_offset(outbox_chunks, element);
            self.copy(field_offset::<CELLS>(PARAMETER) as isize, destination);
            self.move_to(destination);
            adjust(&mut self.source, (element + 1) as u8);
            self.move_to(0);
        }
    }

    fn increment_outbox(&mut self, outbox_chunks: usize, cells: usize) {
        for element in 0..cells {
            let offset = self.outbox_offset(outbox_chunks, element);
            self.move_to(offset);
            self.source.push('+');
            self.move_to(0);
        }
    }

    fn move_outbox_to_parent(&mut self, frame_chunks: usize, outbox_chunks: usize, cells: usize) {
        let caller_delta = -((frame_chunks * ChunkLayout::<CELLS>::STRIDE) as isize);
        for element in 0..cells {
            let source = self.outbox_offset(outbox_chunks, element);
            self.move_between(source, caller_delta + source);
        }
    }

    fn output_outbox(&mut self, outbox_chunks: usize, cells: usize) {
        for element in 0..cells {
            self.move_to(self.outbox_offset(outbox_chunks, element));
            self.source.push('.');
            self.move_to(0);
        }
    }

    fn outbox_offset(&self, outbox_chunks: usize, element: usize) -> isize {
        assert!(element < outbox_chunks * CELLS);
        let chunk = element / CELLS;
        let within = element % CELLS;
        -(((chunk + 1) * ChunkLayout::<CELLS>::STRIDE) as isize) + 1 + within as isize
    }

    fn migrate_by(&mut self, distance: isize) {
        assert_eq!(self.position, 0);
        self.move_by(distance);
        self.position = 0;
    }

    fn migrate_local_array_to_frame(&mut self, frame_chunks: usize) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        self.source.extend(std::iter::repeat_n('>', stride));
        self.source.push('[');
        self.source.extend(std::iter::repeat_n('>', stride));
        self.source.push(']');
        self.source
            .extend(std::iter::repeat_n('<', frame_chunks * stride));
        self.position = 0;
    }

    fn migrate_frame_to_global(&mut self, frame_chunks: usize, anchor_from_global: usize) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        self.source
            .extend(std::iter::repeat_n('>', frame_chunks * stride));
        self.source.extend(std::iter::repeat_n('<', stride));
        self.source.push('[');
        self.source.extend(std::iter::repeat_n('<', stride));
        self.source.push(']');
        self.source
            .extend(std::iter::repeat_n('<', anchor_from_global));
        self.position = 0;
    }

    fn migrate_global_to_frame(&mut self, anchor_from_global: usize, frame_chunks: usize) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        self.source
            .extend(std::iter::repeat_n('>', anchor_from_global));
        self.source.extend(std::iter::repeat_n('>', stride));
        self.source.push('[');
        self.source.extend(std::iter::repeat_n('>', stride));
        self.source.push(']');
        self.source
            .extend(std::iter::repeat_n('<', frame_chunks * stride));
        self.position = 0;
    }

    fn input_field(&mut self, field: usize) {
        self.move_to(field_offset::<CELLS>(field) as isize);
        self.source.push(',');
        self.move_to(0);
    }

    fn output_field(&mut self, field: usize) {
        self.move_to(field_offset::<CELLS>(field) as isize);
        self.source.push('.');
        self.move_to(0);
    }

    fn set_field(&mut self, field: usize, value: u8) {
        self.set(field_offset::<CELLS>(field) as isize, value);
    }

    fn set_pc(&mut self, low_field: usize, high_field: usize, value: u16) {
        self.set_field(low_field, value as u8);
        self.set_field(high_field, (value >> 8) as u8);
    }

    fn set_pc_at(&mut self, base: isize, low_field: usize, high_field: usize, value: u16) {
        self.set(
            base + field_offset::<CELLS>(low_field) as isize,
            value as u8,
        );
        self.set(
            base + field_offset::<CELLS>(high_field) as isize,
            (value >> 8) as u8,
        );
    }

    fn add_field(&mut self, field: usize, value: u8) {
        self.move_to(field_offset::<CELLS>(field) as isize);
        adjust(&mut self.source, value);
        self.move_to(0);
    }

    fn clear_field(&mut self, field: usize) {
        self.clear(field_offset::<CELLS>(field) as isize);
    }

    fn copy_field(&mut self, source: usize, destination: usize) {
        self.copy(
            field_offset::<CELLS>(source) as isize,
            field_offset::<CELLS>(destination) as isize,
        );
    }

    fn move_field(&mut self, source: usize, destination: usize) {
        let source = field_offset::<CELLS>(source) as isize;
        let destination = field_offset::<CELLS>(destination) as isize;
        self.clear(destination);
        self.move_to(source);
        self.source.push('[');
        self.source.push('-');
        self.move_to(destination);
        self.source.push('+');
        self.move_to(source);
        self.source.push(']');
        self.move_to(0);
    }

    fn move_between(&mut self, source: isize, destination: isize) {
        self.clear(destination);
        self.move_field_at(source, destination);
        self.move_to(0);
    }

    fn copy(&mut self, source: isize, destination: isize) {
        assert_ne!(source, destination);
        let restore = field_offset::<CELLS>(RESTORE) as isize;
        assert_ne!(source, restore);
        assert_ne!(destination, restore);
        self.clear(destination);
        self.clear(restore);
        self.move_to(source);
        self.source.push('[');
        self.source.push('-');
        self.move_to(destination);
        self.source.push('+');
        self.move_to(restore);
        self.source.push('+');
        self.move_to(source);
        self.source.push(']');
        self.move_field_at(restore, source);
        self.move_to(0);
    }

    fn move_field_at(&mut self, source: isize, destination: isize) {
        self.move_to(source);
        self.source.push('[');
        self.source.push('-');
        self.move_to(destination);
        self.source.push('+');
        self.move_to(source);
        self.source.push(']');
    }

    fn set(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.source.push_str("[-]");
        adjust(&mut self.source, value);
        self.move_to(0);
    }

    fn clear(&mut self, offset: isize) {
        self.move_to(offset);
        self.source.push_str("[-]");
        self.move_to(0);
    }

    fn move_to(&mut self, destination: isize) {
        self.move_by(destination - self.position);
    }

    fn move_by(&mut self, distance: isize) {
        let instruction = if distance >= 0 { '>' } else { '<' };
        self.source
            .extend(std::iter::repeat_n(instruction, distance.unsigned_abs()));
        self.position += distance;
    }
}

fn adjust(source: &mut String, value: u8) {
    if value <= 128 {
        source.extend(std::iter::repeat_n('+', usize::from(value)));
    } else {
        source.extend(std::iter::repeat_n(
            '-',
            usize::from(256_u16 - u16::from(value)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run_with_stats;

    use super::*;

    fn verify<const CELLS: usize>() {
        let probe = build_portal_probe::<CELLS>();
        for index in 0..probe.array_length {
            let result =
                run_with_stats(probe.source.as_bytes(), &[index as u8, index as u8]).unwrap();
            let mut expected = vec![10 + index as u8, 100 + index as u8];
            expected.extend(201..201 + probe.array_chunks as u8);
            assert_eq!(
                result.output, expected,
                "portal probe output for chunk size {CELLS}, index {index}; stats={:?}",
                result.stats,
            );
        }
    }

    #[test]
    fn portal_dispatch_works_with_eight_cell_chunks() {
        verify::<8>();
    }

    #[test]
    fn portal_dispatch_works_with_sixteen_cell_chunks() {
        verify::<16>();
    }

    fn verify_store<const CELLS: usize>() {
        let length = CELLS * 2;
        let probe = build_portal_store_probe_for_length::<CELLS>(length);
        for index in 0..length {
            let result = run_with_stats(
                probe.source.as_bytes(),
                &[
                    index as u8,
                    index as u8,
                    index as u8,
                    index as u8,
                    index as u8,
                    index as u8,
                ],
            )
            .unwrap();
            let mut expected = vec![
                10_u8.wrapping_add(index as u8),
                100_u8.wrapping_add(index as u8),
                77,
                88,
            ];
            expected.extend(201..201 + probe.array_chunks as u8);
            assert_eq!(
                result.output, expected,
                "portal store for chunk size {CELLS}, index {index}; stats={:?}",
                result.stats,
            );
        }
    }

    #[test]
    fn portal_store_works_with_eight_cell_chunks() {
        verify_store::<8>();
    }

    #[test]
    fn portal_store_works_with_sixteen_cell_chunks() {
        verify_store::<16>();
    }

    fn verify_calls<const CELLS: usize>() {
        let probe = build_call_probe::<CELLS>();
        for depth in 0..=20_u8 {
            let result = run_with_stats(probe.source.as_bytes(), &[depth]).unwrap();
            assert_eq!(
                result.output,
                vec![depth + 1],
                "recursive call output for chunk size {CELLS}, depth {depth}; stats={:?}",
                result.stats,
            );
        }
    }

    #[test]
    fn recursive_calls_work_with_eight_cell_chunks() {
        verify_calls::<8>();
    }

    #[test]
    fn recursive_calls_work_with_sixteen_cell_chunks() {
        verify_calls::<16>();
    }

    fn verify_aggregates<const CELLS: usize>() {
        let probe = build_aggregate_probe::<CELLS>();
        for depth in 0..=10_u8 {
            let result = run_with_stats(probe.source.as_bytes(), &[depth]).unwrap();
            let expected: Vec<_> = (0..probe.result_cells)
                .map(|element| depth + 1 + element as u8)
                .collect();
            assert_eq!(
                result.output, expected,
                "aggregate return for chunk size {CELLS}, depth {depth}; stats={:?}",
                result.stats,
            );
        }
    }

    #[test]
    fn recursive_aggregate_returns_work_with_eight_cell_chunks() {
        verify_aggregates::<8>();
    }

    #[test]
    fn recursive_aggregate_returns_work_with_sixteen_cell_chunks() {
        verify_aggregates::<16>();
    }

    #[test]
    fn outbox_offsets_do_not_depend_on_caller_capacity() {
        let emitter = RelativeEmitter::<16>::new(0);
        assert_eq!(emitter.outbox_offset(1, 0), emitter.outbox_offset(3, 0));
        assert_eq!(emitter.outbox_offset(1, 15), emitter.outbox_offset(3, 15));
        assert_eq!(emitter.outbox_offset(2, 16), emitter.outbox_offset(3, 16));
        assert_eq!(emitter.outbox_offset(2, 31), emitter.outbox_offset(3, 31));
    }

    fn verify_mutual_calls<const CELLS: usize>() {
        let probe = build_mutual_call_probe::<CELLS>();
        assert_ne!(probe.a_frame_chunks, probe.b_frame_chunks);
        for depth in 0..=20_u8 {
            let result = run_with_stats(probe.source.as_bytes(), &[depth]).unwrap();
            assert_eq!(
                result.output,
                vec![depth + 1],
                "mutual recursion for chunk size {CELLS}, depth {depth}; stats={:?}",
                result.stats,
            );
        }
    }

    #[test]
    fn different_frame_sizes_support_mutual_recursion_with_eight_cell_chunks() {
        verify_mutual_calls::<8>();
    }

    #[test]
    fn different_frame_sizes_support_mutual_recursion_with_sixteen_cell_chunks() {
        verify_mutual_calls::<16>();
    }
}
