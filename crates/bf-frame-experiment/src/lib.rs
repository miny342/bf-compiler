//! Experiment for chunk-aligned, dynamically nested Brainfuck stack frames.
//!
//! This is deliberately separate from `bf-compiler`: it tests a possible ABI
//! without committing the main Cell IR to frame-relative addressing.

mod portal;

pub use portal::{
    AggregateProbe, CallProbe, MutualCallProbe, PortalMeasurements, PortalProbe,
    build_aggregate_probe, build_call_probe, build_mutual_call_probe, build_portal_probe,
    build_portal_probe_for_length, build_portal_store_probe_for_length, measure_portal_probe,
    measure_portal_probe_for_length,
};

const SCALAR_GLOBAL_CELLS: usize = 4;

/// Default number of data cells stored behind one allocation flag.
pub const DEFAULT_CHUNK_CELLS: usize = 16;

/// Tape layout calculations for `flag + CELLS` chunks.
#[derive(Debug, Clone, Copy)]
pub struct ChunkLayout<const CELLS: usize>;

impl<const CELLS: usize> ChunkLayout<CELLS> {
    pub const STRIDE: usize = CELLS + 1;

    /// Forward offset of a logical array cell from an aligned chunk flag.
    pub fn cell_offset_from_head(logical_cell: usize) -> usize {
        let chunk = logical_cell / CELLS;
        let within_chunk = logical_cell % CELLS;
        chunk * Self::STRIDE + 1 + within_chunk
    }

    /// Physical distance to a data cell in a frame, measured backwards from
    /// the first free chunk flag (the abstract stack frontier).
    pub fn frame_cell_distance(frame_chunks: usize, logical_cell: usize) -> usize {
        assert!(CELLS > 0, "a chunk must contain at least one data cell");
        assert!(frame_chunks > 0, "a frame must contain at least one chunk");
        assert!(
            logical_cell < frame_chunks * CELLS,
            "logical frame cell is out of bounds"
        );

        frame_chunks * Self::STRIDE - Self::cell_offset_from_head(logical_cell)
    }

    /// Distance to a common header cell counted from the top of the frame.
    pub fn header_cell_distance(index_from_top: usize) -> usize {
        assert!(index_from_top < CELLS, "frame header exceeds its top chunk");
        index_from_top + 1
    }
}

/// Build a Brainfuck program that probes the chunked frame ABI.
///
/// One input byte selects an element of a `CELLS * 2` local array. The output
/// verifies, in order:
///
/// 1. runtime-indexed access to the root frame's local array;
/// 2. access to an identically aligned global array from a child frame;
/// 3. access to a fixed global while a child frame is current;
/// 4. a child-frame local;
/// 5. preservation of a root-frame local across the child frame;
/// 6. preservation of the selected local-array value;
/// 7. the global value after all frames have been deallocated.
pub fn build_probe<const CELLS: usize>() -> String {
    assert!(
        CELLS >= 8,
        "the probe header requires at least eight data cells"
    );
    assert!(CELLS * 2 <= 246, "probe array values must fit in one cell");

    let array_length = CELLS * 2;
    let global_array_head = SCALAR_GLOBAL_CELLS;
    let anchor = global_array_head + 2 * ChunkLayout::<CELLS>::STRIDE;
    let mut emitter = FrameEmitter::<CELLS>::new(anchor);
    emitter.initialize_absolute(0, 41);
    emitter.initialize_absolute(global_array_head, 201);
    emitter.initialize_absolute(global_array_head + ChunkLayout::<CELLS>::STRIDE, 202);
    for index in 0..array_length {
        let position = global_array_head + ChunkLayout::<CELLS>::cell_offset_from_head(index);
        emitter.initialize_absolute(position, 100 + index as u8);
    }
    emitter.enter_stack();

    // Two data chunks for the local array and one top chunk for a common
    // frame header and scalar locals.
    let root_chunks = 3;
    emitter.allocate_frame(root_chunks);

    for index in 0..array_length {
        let offset = emitter.frame_cell_offset(index);
        emitter.set(offset, 10 + index as u8);
    }

    // Common header cells are addressed from the frontier so the same
    // accessor works regardless of the complete frame size.
    let index = emitter.header_offset(0);
    let destination = emitter.header_offset(1);
    let condition = emitter.header_offset(2);
    let copy_restore = emitter.header_offset(3);
    let branch_flag = emitter.header_offset(4);
    let root_local = emitter.header_offset(7);

    emitter.input(index);
    let local_array_offsets: Vec<_> = (0..array_length)
        .map(|element| emitter.frame_cell_offset(element))
        .collect();
    emitter.array_load_dispatch(
        &local_array_offsets,
        index,
        destination,
        condition,
        copy_restore,
        branch_flag,
    );
    emitter.output(destination);
    emitter.set(root_local, 77);

    // A differently-sized nested frame proves that all addressing really is
    // relative to the current frontier.
    let child_chunks = 2;
    emitter.allocate_frame(child_chunks);
    let child_index = emitter.header_offset(0);
    let child_destination = emitter.header_offset(1);
    let child_condition = emitter.header_offset(2);
    let child_copy_restore = emitter.header_offset(3);
    let child_branch_flag = emitter.header_offset(4);
    let child_local = emitter.header_offset(7);
    let root_index_from_child = index - (child_chunks * ChunkLayout::<CELLS>::STRIDE) as isize;
    emitter.copy(root_index_from_child, child_index, child_copy_restore);
    let global_array_offsets: Vec<_> = (0..array_length)
        .map(|element| {
            let position = global_array_head + ChunkLayout::<CELLS>::cell_offset_from_head(element);
            emitter.absolute_offset(position)
        })
        .collect();
    emitter.array_load_dispatch(
        &global_array_offsets,
        child_index,
        child_destination,
        child_condition,
        child_copy_restore,
        child_branch_flag,
    );
    emitter.output(child_destination);
    emitter.set(child_local, 99);
    emitter.add_global(0, 1);
    emitter.output_global(0);
    emitter.output(child_local);
    emitter.deallocate_frame(child_chunks);

    emitter.output(root_local);
    emitter.output(destination);
    emitter.deallocate_frame(root_chunks);

    emitter.output_global(0);
    emitter.output_absolute(global_array_head);
    emitter.output_absolute(global_array_head + ChunkLayout::<CELLS>::STRIDE);
    emitter.finish()
}

struct FrameEmitter<const CELLS: usize> {
    source: String,
    /// Current physical displacement from the active frontier.
    position: isize,
    anchor: usize,
    allocated_chunks: usize,
    frames: Vec<usize>,
}

impl<const CELLS: usize> FrameEmitter<CELLS> {
    fn new(anchor: usize) -> Self {
        Self {
            source: String::new(),
            position: 0,
            anchor,
            allocated_chunks: 0,
            frames: Vec::new(),
        }
    }

    fn initialize_absolute(&mut self, index: usize, value: u8) {
        assert!(self.frames.is_empty());
        assert!(index < self.anchor);
        self.move_to(index as isize);
        self.clear_current();
        self.adjust_current(value);
    }

    fn enter_stack(&mut self) {
        assert!(self.frames.is_empty());
        // The anchor is followed by one reserved data chunk. Keeping the
        // first stack flag one complete stride to its right puts every flag
        // on the same alignment.
        let first_stack_flag = self.anchor + ChunkLayout::<CELLS>::STRIDE;
        self.move_to(first_stack_flag as isize);
        self.position = 0;
    }

    fn allocate_frame(&mut self, chunks: usize) {
        assert!(chunks > 0);
        assert_eq!(self.position, 0);
        for _ in 0..chunks {
            self.adjust_current(1);
            self.move_by(ChunkLayout::<CELLS>::STRIDE as isize);
        }
        self.allocated_chunks += chunks;
        self.frames.push(chunks);
        self.position = 0;
    }

    fn deallocate_frame(&mut self, expected_chunks: usize) {
        assert_eq!(self.frames.pop(), Some(expected_chunks));
        assert_eq!(self.position, 0);
        for _ in 0..expected_chunks {
            self.move_by(-(ChunkLayout::<CELLS>::STRIDE as isize));
            self.clear_current();
            for _ in 0..CELLS {
                self.move_by(1);
                self.clear_current();
            }
            self.move_by(-(CELLS as isize));
        }
        self.allocated_chunks -= expected_chunks;
        self.position = 0;
    }

    fn frame_cell_offset(&self, logical_cell: usize) -> isize {
        let chunks = *self.frames.last().expect("no active frame");
        -(ChunkLayout::<CELLS>::frame_cell_distance(chunks, logical_cell) as isize)
    }

    fn absolute_offset(&self, position: usize) -> isize {
        let frontier = self.anchor + (self.allocated_chunks + 1) * ChunkLayout::<CELLS>::STRIDE;
        position as isize - frontier as isize
    }

    fn header_offset(&self, index_from_top: usize) -> isize {
        assert!(!self.frames.is_empty());
        -(ChunkLayout::<CELLS>::header_cell_distance(index_from_top) as isize)
    }

    fn set(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.clear_current();
        self.adjust_current(value);
        self.move_to(0);
    }

    fn add(&mut self, offset: isize, value: u8) {
        self.move_to(offset);
        self.adjust_current(value);
        self.move_to(0);
    }

    fn input(&mut self, offset: isize) {
        self.move_to(offset);
        self.source.push(',');
        self.move_to(0);
    }

    fn output(&mut self, offset: isize) {
        self.move_to(offset);
        self.source.push('.');
        self.move_to(0);
    }

    fn copy(&mut self, source: isize, destination: isize, restore: isize) {
        assert_ne!(source, destination);
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
        self.move_to(restore);
        self.source.push('[');
        self.source.push('-');
        self.move_to(source);
        self.source.push('+');
        self.move_to(restore);
        self.source.push(']');
        self.move_to(0);
    }

    fn clear(&mut self, offset: isize) {
        self.move_to(offset);
        self.clear_current();
        self.move_to(0);
    }

    fn branch(
        &mut self,
        condition: isize,
        flag: isize,
        then_body: impl FnOnce(&mut Self),
        else_body: impl FnOnce(&mut Self),
    ) {
        assert_ne!(condition, flag);
        self.set(flag, 1);

        self.move_to(condition);
        self.source.push('[');
        self.clear_current();
        self.move_to(0);
        then_body(self);
        assert_eq!(self.position, 0);
        self.clear(condition);
        self.clear(flag);
        self.move_to(condition);
        self.source.push(']');

        self.move_to(flag);
        self.source.push('[');
        self.clear_current();
        self.move_to(0);
        else_body(self);
        assert_eq!(self.position, 0);
        self.clear(condition);
        self.clear(flag);
        self.move_to(flag);
        self.source.push(']');
        self.move_to(0);
    }

    #[allow(clippy::too_many_arguments)]
    fn array_load_dispatch(
        &mut self,
        element_offsets: &[isize],
        index: isize,
        destination: isize,
        condition: isize,
        copy_restore: isize,
        branch_flag: isize,
    ) {
        self.clear(destination);
        for (element, element_offset) in element_offsets.iter().copied().enumerate() {
            self.copy(index, condition, copy_restore);
            self.add(condition, 0_u8.wrapping_sub(element as u8));
            self.branch(
                condition,
                branch_flag,
                |_| {},
                |emitter| emitter.copy(element_offset, destination, copy_restore),
            );
        }
    }

    fn add_global(&mut self, index: usize, value: u8) {
        assert!(index < SCALAR_GLOBAL_CELLS);
        self.seek_anchor();
        let distance = (self.anchor - index) as isize;
        self.move_by(-distance);
        self.adjust_current(value);
        self.move_by(distance);
        self.seek_frontier();
    }

    fn output_global(&mut self, index: usize) {
        assert!(index < SCALAR_GLOBAL_CELLS);
        self.output_absolute(index);
    }

    fn output_absolute(&mut self, position: usize) {
        assert!(position < self.anchor);
        self.seek_anchor();
        let distance = (self.anchor - position) as isize;
        self.move_by(-distance);
        self.source.push('.');
        self.move_by(distance);
        self.seek_frontier();
    }

    fn seek_anchor(&mut self) {
        assert_eq!(self.position, 0);
        let stride = ChunkLayout::<CELLS>::STRIDE;
        self.move_by(-(stride as isize));
        self.source.push('[');
        self.source.extend(std::iter::repeat_n('<', stride));
        self.source.push(']');
        self.position = -(((self.allocated_chunks + 1) * stride) as isize);
    }

    fn seek_frontier(&mut self) {
        let stride = ChunkLayout::<CELLS>::STRIDE;
        let anchor = -(((self.allocated_chunks + 1) * stride) as isize);
        assert_eq!(self.position, anchor);
        self.move_by(stride as isize);
        self.source.push('[');
        self.source.extend(std::iter::repeat_n('>', stride));
        self.source.push(']');
        self.position = 0;
    }

    fn move_to(&mut self, destination: isize) {
        self.move_by(destination - self.position);
    }

    fn move_by(&mut self, amount: isize) {
        let byte = if amount >= 0 { '>' } else { '<' };
        self.source
            .extend(std::iter::repeat_n(byte, amount.unsigned_abs()));
        self.position += amount;
    }

    fn clear_current(&mut self) {
        self.source.push_str("[-]");
    }

    fn adjust_current(&mut self, value: u8) {
        if value <= 128 {
            self.source
                .extend(std::iter::repeat_n('+', usize::from(value)));
        } else {
            self.source.extend(std::iter::repeat_n(
                '-',
                usize::from(256_u16 - u16::from(value)),
            ));
        }
    }

    fn finish(self) -> String {
        assert!(self.frames.is_empty());
        assert_eq!(self.allocated_chunks, 0);
        assert_eq!(self.position, 0);
        self.source
    }
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;

    fn verify_probe<const CELLS: usize>() {
        let source = build_probe::<CELLS>();
        for index in 0..CELLS * 2 {
            let selected = 10 + index as u8;
            assert_eq!(
                run(source.as_bytes(), &[index as u8]).unwrap(),
                vec![
                    selected,
                    100 + index as u8,
                    42,
                    99,
                    77,
                    selected,
                    42,
                    201,
                    202,
                ],
                "probe output for chunk size {CELLS}, index {index}",
            );
        }
    }

    #[test]
    fn layout_skips_interleaved_flags() {
        assert_eq!(ChunkLayout::<8>::STRIDE, 9);
        assert_eq!(ChunkLayout::<8>::cell_offset_from_head(0), 1);
        assert_eq!(ChunkLayout::<8>::cell_offset_from_head(7), 8);
        assert_eq!(ChunkLayout::<8>::cell_offset_from_head(8), 10);
        assert_eq!(ChunkLayout::<8>::frame_cell_distance(3, 0), 26);
        assert_eq!(ChunkLayout::<8>::frame_cell_distance(3, 7), 19);
        assert_eq!(ChunkLayout::<8>::frame_cell_distance(3, 8), 17);
        assert_eq!(ChunkLayout::<8>::frame_cell_distance(3, 23), 1);
    }

    #[test]
    fn probe_works_with_eight_cell_chunks() {
        verify_probe::<8>();
    }

    #[test]
    fn probe_works_with_sixteen_cell_chunks() {
        verify_probe::<16>();
    }
}
