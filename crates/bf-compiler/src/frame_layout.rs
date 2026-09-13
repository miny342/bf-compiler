//! Physical layout of scalar activation frames for the continuation ABI.
//!
//! A frame is made from chunks containing one allocation flag followed by
//! `chunk_cells` data cells. Aligned aggregate regions occupy the low-address
//! end; scalar value slots are kept next to the optional aggregate-return
//! outbox and ABI context so large aggregates do not make ordinary scalar
//! operations expensive.

use crate::continuation_ir::{
    FrameAggregateDescriptor, FrameAggregateId, FrameArrayDescriptor, FrameArrayId, FrameSlot,
};
use std::error::Error;
use std::fmt;

/// The Brainfuck tape size guaranteed by the interpreter and ABI version 0.
pub const TAPE_CELLS: usize = 30_000;

/// Number of logical cells shared by frame contexts and array portals.
pub const PROTOCOL_CELLS: usize = 16;

/// Default number of data cells in a physical chunk.
pub const DEFAULT_CHUNK_CELLS: usize = 16;

/// Version-0 chunk geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiConfig {
    chunk_cells: usize,
}

impl AbiConfig {
    /// Creates the supported 16-data-cell ABI configuration.
    pub const fn new(chunk_cells: usize) -> Result<Self, FrameLayoutError> {
        match chunk_cells {
            16 => Ok(Self { chunk_cells }),
            _ => Err(FrameLayoutError::UnsupportedChunkCells { chunk_cells }),
        }
    }

    pub const fn chunk_cells(self) -> usize {
        self.chunk_cells
    }

    /// Physical width of `flag | data[D]`.
    pub const fn stride(self) -> usize {
        self.chunk_cells + 1
    }

    /// Chunks occupied by a 16-cell array portal or frame context.
    pub const fn portal_chunks(self) -> usize {
        PROTOCOL_CELLS.div_ceil(self.chunk_cells)
    }

    /// Checked physical offset of a logical data cell from its region's first head.
    pub fn try_logical_offset_from_head(
        self,
        logical_cell: usize,
    ) -> Result<usize, FrameLayoutError> {
        (logical_cell / self.chunk_cells)
            .checked_mul(self.stride())
            .and_then(|offset| offset.checked_add(1))
            .and_then(|offset| offset.checked_add(logical_cell % self.chunk_cells))
            .ok_or(FrameLayoutError::SizeOverflow)
    }

    pub(crate) const fn logical_offset_from_head(self, logical_cell: usize) -> usize {
        (logical_cell / self.chunk_cells) * self.stride() + 1 + logical_cell % self.chunk_cells
    }
}

impl Default for AbiConfig {
    fn default() -> Self {
        Self {
            chunk_cells: DEFAULT_CHUNK_CELLS,
        }
    }
}

/// A logical field in the common 16-cell context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AbiField {
    Value = 0,
    Active = 1,
    PcLow = 2,
    PcHigh = 3,
    NextPcLow = 4,
    NextPcHigh = 5,
    Condition = 6,
    Restore = 7,
    Branch = 8,
    Index = 9,
    ReturnPcLow = 10,
    ReturnPcHigh = 11,
    Scratch0 = 12,
    Scratch1 = 13,
    Scratch2 = 14,
    Scratch3 = 15,
}

impl AbiField {
    pub const ALL: [Self; PROTOCOL_CELLS] = [
        Self::Value,
        Self::Active,
        Self::PcLow,
        Self::PcHigh,
        Self::NextPcLow,
        Self::NextPcHigh,
        Self::Condition,
        Self::Restore,
        Self::Branch,
        Self::Index,
        Self::ReturnPcLow,
        Self::ReturnPcHigh,
        Self::Scratch0,
        Self::Scratch1,
        Self::Scratch2,
        Self::Scratch3,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Complete layout of one function activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameLayout {
    config: AbiConfig,
    value_cells: usize,
    outbox_cells: usize,
    route_cells: usize,
    value_chunks: usize,
    frame_aggregates: Vec<FrameAggregateLayout>,
    aggregate_chunks: usize,
    outbox_chunks: usize,
    route_chunks: usize,
    context_chunks: usize,
    frame_chunks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameAggregateLayout {
    descriptor: FrameAggregateDescriptor,
    base_chunk: usize,
    chunks: usize,
}

impl FrameLayout {
    /// Lays out parameter, local, and temporary slots followed by outbox and
    /// context chunks. Passing zero outbox cells selects the scalar-only ABI.
    pub fn new(
        config: AbiConfig,
        value_cells: usize,
        outbox_cells: usize,
    ) -> Result<Self, FrameLayoutError> {
        Self::with_aggregates(config, value_cells, &[], outbox_cells)
    }

    /// Lay out scalar slots, aligned array regions, an aggregate outbox, and
    /// the common dispatch context in that order.
    pub fn with_arrays(
        config: AbiConfig,
        value_cells: usize,
        frame_arrays: &[FrameArrayDescriptor],
        outbox_cells: usize,
    ) -> Result<Self, FrameLayoutError> {
        for descriptor in frame_arrays {
            if !(1..=256).contains(&descriptor.cells()) {
                return Err(FrameLayoutError::InvalidArrayLength {
                    array: descriptor.id(),
                    cells: descriptor.cells(),
                });
            }
        }
        Self::with_aggregates(config, value_cells, frame_arrays, outbox_cells)
            .map_err(array_compat_error)
    }

    /// Lay out aligned Version-1 aggregate regions, scalar slots, an aggregate
    /// outbox, and the common dispatch context in that order.
    ///
    /// A zero-cell aggregate keeps its descriptor identity but consumes no
    /// portal or payload chunks.
    pub fn with_aggregates(
        config: AbiConfig,
        value_cells: usize,
        frame_aggregates: &[FrameAggregateDescriptor],
        outbox_cells: usize,
    ) -> Result<Self, FrameLayoutError> {
        Self::with_aggregates_and_route(config, value_cells, frame_aggregates, outbox_cells, 0)
    }

    pub(crate) fn with_aggregates_and_route(
        config: AbiConfig,
        value_cells: usize,
        frame_aggregates: &[FrameAggregateDescriptor],
        outbox_cells: usize,
        route_cells: usize,
    ) -> Result<Self, FrameLayoutError> {
        let value_chunks = checked_chunks(value_cells, config)?;
        let mut aggregate_chunks = 0usize;
        let mut aggregate_layouts = Vec::with_capacity(frame_aggregates.len());
        for descriptor in frame_aggregates {
            if aggregate_layouts
                .iter()
                .any(|layout: &FrameAggregateLayout| layout.descriptor.id() == descriptor.id())
            {
                return Err(FrameLayoutError::DuplicateFrameAggregate {
                    aggregate: descriptor.id(),
                });
            }
            let chunks = if descriptor.cells() == 0 {
                0
            } else {
                checked_chunks(
                    PROTOCOL_CELLS
                        .checked_add(descriptor.cells())
                        .ok_or(FrameLayoutError::SizeOverflow)?,
                    config,
                )?
            };
            let base_chunk = aggregate_chunks;
            aggregate_layouts.push(FrameAggregateLayout {
                descriptor: *descriptor,
                base_chunk,
                chunks,
            });
            aggregate_chunks = aggregate_chunks
                .checked_add(chunks)
                .ok_or(FrameLayoutError::SizeOverflow)?;
        }
        let outbox_chunks = checked_chunks(outbox_cells, config)?;
        let route_chunks = checked_chunks(route_cells, config)?;
        let context_chunks = config.portal_chunks();
        let frame_chunks = value_chunks
            .checked_add(aggregate_chunks)
            .and_then(|chunks| chunks.checked_add(outbox_chunks))
            .and_then(|chunks| chunks.checked_add(route_chunks))
            .and_then(|chunks| chunks.checked_add(context_chunks))
            .ok_or(FrameLayoutError::SizeOverflow)?;

        let layout = Self {
            config,
            value_cells,
            outbox_cells,
            route_cells,
            value_chunks,
            frame_aggregates: aggregate_layouts,
            aggregate_chunks,
            outbox_chunks,
            route_chunks,
            context_chunks,
            frame_chunks,
        };

        // Even with an anchor at cell zero, the anchor chunk, root frame,
        // and first free frontier head must all be addressable.
        let minimum_tape_cells = layout.minimum_main_tape_cells(0)?;
        if minimum_tape_cells > TAPE_CELLS {
            return Err(FrameLayoutError::FrameTooLarge {
                frame_chunks,
                minimum_tape_cells,
                tape_cells: TAPE_CELLS,
            });
        }

        debug_assert!(layout.regions_do_not_overlap());
        Ok(layout)
    }

    pub const fn config(&self) -> AbiConfig {
        self.config
    }

    pub const fn value_cells(&self) -> usize {
        self.value_cells
    }

    pub const fn outbox_cells(&self) -> usize {
        self.outbox_cells
    }

    pub const fn value_chunks(&self) -> usize {
        self.value_chunks
    }

    pub const fn outbox_chunks(&self) -> usize {
        self.outbox_chunks
    }

    pub(crate) const fn route_chunks(&self) -> usize {
        self.route_chunks
    }

    pub(crate) fn route_offset(&self, index: usize) -> Result<isize, FrameLayoutError> {
        if index >= self.route_cells {
            return Err(FrameLayoutError::SizeOverflow);
        }
        Ok(
            -(self.route_chunks as isize * self.config.stride() as isize)
                + self.config.try_logical_offset_from_head(index)? as isize,
        )
    }

    pub const fn array_chunks(&self) -> usize {
        self.aggregate_chunks
    }

    /// Total chunks occupied by frame-owned aggregate regions.
    pub const fn aggregate_chunks(&self) -> usize {
        self.aggregate_chunks
    }

    pub const fn context_chunks(&self) -> usize {
        self.context_chunks
    }

    pub const fn frame_chunks(&self) -> usize {
        self.frame_chunks
    }

    /// Number of unused value cells before the outbox/context region.
    pub const fn value_padding_cells(&self) -> usize {
        self.value_chunks * self.config.chunk_cells - self.value_cells
    }

    /// Number of unused cells at the high end of the outbox allocation.
    pub const fn outbox_padding_cells(&self) -> usize {
        self.outbox_chunks * self.config.chunk_cells - self.outbox_cells
    }

    /// Physical size from the frame-bottom head up to, but not including, the
    /// frontier head.
    pub const fn physical_cells(&self) -> usize {
        self.frame_chunks * self.config.stride()
    }

    /// Offset of a value slot from the dispatch-context base head.
    ///
    /// This convenience accessor is intended for already validated IR.
    pub fn frame_offset(&self, slot: FrameSlot) -> isize {
        self.try_frame_offset(slot)
            .expect("frame slot must belong to this frame layout")
    }

    /// Checked form of [`Self::frame_offset`].
    pub fn try_frame_offset(&self, slot: FrameSlot) -> Result<isize, FrameLayoutError> {
        let index = slot.index();
        if index >= self.value_cells {
            return Err(FrameLayoutError::FrameSlotOutOfBounds {
                slot: index,
                value_cells: self.value_cells,
            });
        }

        let chunks_below_context = self.value_chunks + self.outbox_chunks + self.route_chunks;
        Ok(
            -(chunks_below_context as isize * self.config.stride() as isize)
                + self.config.try_logical_offset_from_head(index)? as isize,
        )
    }

    /// Offset of an aligned aggregate's base head from the dispatch-context base.
    pub fn aggregate_base_offset(
        &self,
        aggregate: FrameAggregateId,
    ) -> Result<isize, FrameLayoutError> {
        let layout = self.aggregate_layout(aggregate)?;
        if layout.descriptor.cells() == 0 {
            return Err(FrameLayoutError::ZeroSizedFrameAggregate { aggregate });
        }
        let chunks_below_context =
            self.value_chunks + self.aggregate_chunks + self.outbox_chunks + self.route_chunks;
        Ok(-((chunks_below_context - layout.base_chunk) as isize * self.config.stride() as isize))
    }

    pub fn aggregate_chunk_count(
        &self,
        aggregate: FrameAggregateId,
    ) -> Result<usize, FrameLayoutError> {
        Ok(self.aggregate_layout(aggregate)?.chunks)
    }

    /// Offset of one aggregate payload cell from the dispatch-context base.
    pub fn aggregate_element_offset(
        &self,
        aggregate: FrameAggregateId,
        index: usize,
    ) -> Result<isize, FrameLayoutError> {
        let layout = self.aggregate_layout(aggregate)?;
        if index >= layout.descriptor.cells() {
            return Err(FrameLayoutError::AggregateElementOutOfBounds {
                aggregate,
                index,
                cells: layout.descriptor.cells(),
            });
        }
        Ok(self.aggregate_base_offset(aggregate)?
            + self.config.try_logical_offset_from_head(
                PROTOCOL_CELLS
                    .checked_add(index)
                    .ok_or(FrameLayoutError::SizeOverflow)?,
            )? as isize)
    }

    /// Offset of a protocol field in an aligned aggregate portal.
    pub fn aggregate_portal_offset(
        &self,
        aggregate: FrameAggregateId,
        field: AbiField,
    ) -> Result<isize, FrameLayoutError> {
        Ok(self.aggregate_base_offset(aggregate)?
            + self.config.try_logical_offset_from_head(field.index())? as isize)
    }

    pub fn aggregate_base_offset_from_frontier(
        &self,
        aggregate: FrameAggregateId,
    ) -> Result<isize, FrameLayoutError> {
        Ok(self.context_offset_from_frontier() + self.aggregate_base_offset(aggregate)?)
    }

    pub fn aggregate_element_offset_from_frontier(
        &self,
        aggregate: FrameAggregateId,
        index: usize,
    ) -> Result<isize, FrameLayoutError> {
        Ok(
            self.context_offset_from_frontier()
                + self.aggregate_element_offset(aggregate, index)?,
        )
    }

    fn aggregate_layout(
        &self,
        aggregate: FrameAggregateId,
    ) -> Result<&FrameAggregateLayout, FrameLayoutError> {
        self.frame_aggregates
            .iter()
            .find(|layout| layout.descriptor.id() == aggregate)
            .ok_or(FrameLayoutError::UnknownFrameAggregate { aggregate })
    }

    /// Version-0 compatibility alias for [`Self::aggregate_base_offset`].
    pub fn array_base_offset(&self, array: FrameArrayId) -> Result<isize, FrameLayoutError> {
        self.aggregate_base_offset(array)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_chunk_count`].
    pub fn array_chunk_count(&self, array: FrameArrayId) -> Result<usize, FrameLayoutError> {
        self.aggregate_chunk_count(array)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_element_offset`].
    pub fn array_element_offset(
        &self,
        array: FrameArrayId,
        index: usize,
    ) -> Result<isize, FrameLayoutError> {
        self.aggregate_element_offset(array, index)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_portal_offset`].
    pub fn array_portal_offset(
        &self,
        array: FrameArrayId,
        field: AbiField,
    ) -> Result<isize, FrameLayoutError> {
        self.aggregate_portal_offset(array, field)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for
    /// [`Self::aggregate_base_offset_from_frontier`].
    pub fn array_base_offset_from_frontier(
        &self,
        array: FrameArrayId,
    ) -> Result<isize, FrameLayoutError> {
        self.aggregate_base_offset_from_frontier(array)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for
    /// [`Self::aggregate_element_offset_from_frontier`].
    pub fn array_element_offset_from_frontier(
        &self,
        array: FrameArrayId,
        index: usize,
    ) -> Result<isize, FrameLayoutError> {
        self.aggregate_element_offset_from_frontier(array, index)
            .map_err(array_compat_error)
    }

    /// Offset of an outbox cell from the dispatch-context base head.
    /// Logical chunk zero is immediately below the optional route staging
    /// chunk and therefore remains stable for callers using the same layout.
    pub fn outbox_offset(&self, index: usize) -> Result<isize, FrameLayoutError> {
        if index >= self.outbox_cells {
            return Err(FrameLayoutError::OutboxCellOutOfBounds {
                cell: index,
                outbox_cells: self.outbox_cells,
            });
        }

        let chunk = index / self.config.chunk_cells;
        let within = index % self.config.chunk_cells;
        Ok(
            -((self.route_chunks + chunk + 1) as isize * self.config.stride() as isize)
                + 1
                + within as isize,
        )
    }

    /// Offset of an ABI field from the dispatch-context base head.
    pub const fn abi_offset(&self, field: AbiField) -> isize {
        self.config.logical_offset_from_head(field.index()) as isize
    }

    /// Offset of the context base head from the current frontier head.
    pub const fn context_offset_from_frontier(&self) -> isize {
        -(self.context_chunks as isize * self.config.stride() as isize)
    }

    /// Offset of a value slot from the current frontier head.
    pub fn frame_offset_from_frontier(&self, slot: FrameSlot) -> isize {
        self.context_offset_from_frontier() + self.frame_offset(slot)
    }

    /// Offset of an ABI field from the current frontier head.
    pub const fn abi_offset_from_frontier(&self, field: AbiField) -> isize {
        self.context_offset_from_frontier() + self.abi_offset(field)
    }

    /// Offset of an outbox cell from the current frontier head.
    pub fn outbox_offset_from_frontier(&self, index: usize) -> Result<isize, FrameLayoutError> {
        Ok(self.context_offset_from_frontier() + self.outbox_offset(index)?)
    }

    /// Minimum tape length needed when the anchor head is at `anchor_head`.
    /// This includes the anchor data, root frame, and frontier head.
    pub fn minimum_main_tape_cells(&self, anchor_head: usize) -> Result<usize, FrameLayoutError> {
        let chunks_after_anchor = self
            .frame_chunks
            .checked_add(1)
            .ok_or(FrameLayoutError::SizeOverflow)?;
        anchor_head
            .checked_add(
                chunks_after_anchor
                    .checked_mul(self.config.stride())
                    .ok_or(FrameLayoutError::SizeOverflow)?,
            )
            .and_then(|frontier| frontier.checked_add(1))
            .ok_or(FrameLayoutError::SizeOverflow)
    }

    /// Verifies that the static prefix, anchor, minimum main frame, and
    /// frontier head fit on the ABI's 30,000-cell tape.
    pub fn validate_main_capacity(&self, anchor_head: usize) -> Result<(), FrameLayoutError> {
        self.validate_main_capacity_in(anchor_head, TAPE_CELLS)
    }

    /// Capacity check with an explicit tape size, useful to callers and tests.
    pub fn validate_main_capacity_in(
        &self,
        anchor_head: usize,
        tape_cells: usize,
    ) -> Result<(), FrameLayoutError> {
        let required_cells = self.minimum_main_tape_cells(anchor_head)?;
        if required_cells > tape_cells {
            return Err(FrameLayoutError::MainFrameDoesNotFit {
                anchor_head,
                frame_chunks: self.frame_chunks,
                required_cells,
                tape_cells,
            });
        }
        Ok(())
    }

    fn regions_do_not_overlap(&self) -> bool {
        let context_start =
            (self.value_chunks + self.aggregate_chunks + self.outbox_chunks + self.route_chunks)
                * self.config.stride();
        let frame_end = self.frame_chunks * self.config.stride();
        context_start < frame_end
            && (self.value_chunks + self.aggregate_chunks) * self.config.stride() <= context_start
            && (self.outbox_chunks + self.route_chunks) * self.config.stride()
                <= context_start
                    - (self.value_chunks + self.aggregate_chunks) * self.config.stride()
    }
}

fn array_compat_error(error: FrameLayoutError) -> FrameLayoutError {
    match error {
        FrameLayoutError::DuplicateFrameAggregate { aggregate } => {
            FrameLayoutError::DuplicateFrameArray { array: aggregate }
        }
        FrameLayoutError::UnknownFrameAggregate { aggregate } => {
            FrameLayoutError::UnknownFrameArray { array: aggregate }
        }
        FrameLayoutError::AggregateElementOutOfBounds {
            aggregate,
            index,
            cells,
        } => FrameLayoutError::ArrayElementOutOfBounds {
            array: aggregate,
            index,
            cells,
        },
        other => other,
    }
}

fn checked_chunks(cells: usize, config: AbiConfig) -> Result<usize, FrameLayoutError> {
    cells
        .checked_add(config.chunk_cells() - 1)
        .map(|rounded| rounded / config.chunk_cells())
        .ok_or(FrameLayoutError::SizeOverflow)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameLayoutError {
    UnsupportedChunkCells {
        chunk_cells: usize,
    },
    SizeOverflow,
    InvalidArrayLength {
        array: FrameArrayId,
        cells: usize,
    },
    DuplicateFrameAggregate {
        aggregate: FrameAggregateId,
    },
    UnknownFrameAggregate {
        aggregate: FrameAggregateId,
    },
    ZeroSizedFrameAggregate {
        aggregate: FrameAggregateId,
    },
    AggregateElementOutOfBounds {
        aggregate: FrameAggregateId,
        index: usize,
        cells: usize,
    },
    DuplicateFrameArray {
        array: FrameArrayId,
    },
    UnknownFrameArray {
        array: FrameArrayId,
    },
    ArrayElementOutOfBounds {
        array: FrameArrayId,
        index: usize,
        cells: usize,
    },
    FrameTooLarge {
        frame_chunks: usize,
        minimum_tape_cells: usize,
        tape_cells: usize,
    },
    MainFrameDoesNotFit {
        anchor_head: usize,
        frame_chunks: usize,
        required_cells: usize,
        tape_cells: usize,
    },
    FrameSlotOutOfBounds {
        slot: usize,
        value_cells: usize,
    },
    OutboxCellOutOfBounds {
        cell: usize,
        outbox_cells: usize,
    },
}

impl fmt::Display for FrameLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChunkCells { chunk_cells } => write!(
                f,
                "ABI chunk size must be 16 data cells, got {chunk_cells}"
            ),
            Self::SizeOverflow => write!(f, "frame layout size exceeds the host address space"),
            Self::InvalidArrayLength { array, cells } => write!(
                f,
                "frame array {} must contain between 1 and 256 cells, got {cells}",
                array.index()
            ),
            Self::DuplicateFrameAggregate { aggregate } => write!(
                f,
                "frame aggregate {} occurs more than once",
                aggregate.index()
            ),
            Self::UnknownFrameAggregate { aggregate } => {
                write!(f, "frame aggregate {} does not exist", aggregate.index())
            }
            Self::ZeroSizedFrameAggregate { aggregate } => write!(
                f,
                "zero-sized frame aggregate {} has no physical base or portal",
                aggregate.index()
            ),
            Self::AggregateElementOutOfBounds {
                aggregate,
                index,
                cells,
            } => write!(
                f,
                "element {index} is outside frame aggregate {} with {cells} cells",
                aggregate.index()
            ),
            Self::DuplicateFrameArray { array } => {
                write!(f, "frame array {} occurs more than once", array.index())
            }
            Self::UnknownFrameArray { array } => {
                write!(f, "frame array {} does not exist", array.index())
            }
            Self::ArrayElementOutOfBounds {
                array,
                index,
                cells,
            } => write!(
                f,
                "element {index} is outside frame array {} with {cells} cells",
                array.index()
            ),
            Self::FrameTooLarge {
                frame_chunks,
                minimum_tape_cells,
                tape_cells,
            } => write!(
                f,
                "a {frame_chunks}-chunk frame requires at least {minimum_tape_cells} cells, but the ABI tape has {tape_cells}"
            ),
            Self::MainFrameDoesNotFit {
                anchor_head,
                frame_chunks,
                required_cells,
                tape_cells,
            } => write!(
                f,
                "main frame ({frame_chunks} chunks) after anchor cell {anchor_head} requires {required_cells} tape cells, but only {tape_cells} are available"
            ),
            Self::FrameSlotOutOfBounds { slot, value_cells } => write!(
                f,
                "frame slot {slot} is outside a layout with {value_cells} value cells"
            ),
            Self::OutboxCellOutOfBounds { cell, outbox_cells } => write!(
                f,
                "outbox cell {cell} is outside a layout with {outbox_cells} cells"
            ),
        }
    }
}

impl Error for FrameLayoutError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_chunk_sizes() {
        for chunk_cells in [0, 1, 8, 12, 17, 32, usize::MAX] {
            assert_eq!(
                AbiConfig::new(chunk_cells),
                Err(FrameLayoutError::UnsupportedChunkCells { chunk_cells })
            );
        }
        assert_eq!(AbiConfig::new(16), Ok(AbiConfig::default()));
    }

    #[test]
    fn checked_logical_offset_rejects_host_size_overflow() {
        let config = AbiConfig::new(16).unwrap();

        assert_eq!(
            config.try_logical_offset_from_head(usize::MAX),
            Err(FrameLayoutError::SizeOverflow),
        );
    }

    #[test]
    fn exact_layout_for_sixteen_cell_chunks() {
        let config = AbiConfig::new(16).unwrap();
        let layout = FrameLayout::new(config, 17, 0).unwrap();

        assert_eq!(config.stride(), 17);
        assert_eq!(config.portal_chunks(), 1);
        assert_eq!(layout.value_chunks(), 2);
        assert_eq!(layout.value_padding_cells(), 15);
        assert_eq!(layout.outbox_chunks(), 0);
        assert_eq!(layout.context_chunks(), 1);
        assert_eq!(layout.frame_chunks(), 3);
        assert_eq!(layout.physical_cells(), 51);

        assert_eq!(layout.frame_offset(FrameSlot::new(0)), -33);
        assert_eq!(layout.frame_offset(FrameSlot::new(15)), -18);
        assert_eq!(layout.frame_offset(FrameSlot::new(16)), -16);
        assert_eq!(layout.context_offset_from_frontier(), -17);
        assert_eq!(layout.frame_offset_from_frontier(FrameSlot::new(0)), -50);
        assert_eq!(layout.abi_offset(AbiField::Value), 1);
        assert_eq!(layout.abi_offset(AbiField::Scratch3), 16);
        assert_eq!(layout.abi_offset_from_frontier(AbiField::Value), -16);
        assert_eq!(layout.abi_offset_from_frontier(AbiField::Scratch3), -1);
    }

    #[test]
    fn outbox_is_reversed_below_context_and_disjoint_from_values() {
        let layout = FrameLayout::new(AbiConfig::default(), 17, 18).unwrap();

        assert_eq!(layout.value_chunks(), 2);
        assert_eq!(layout.outbox_chunks(), 2);
        assert_eq!(layout.outbox_padding_cells(), 14);
        assert_eq!(layout.frame_chunks(), 5);
        assert_eq!(layout.frame_offset(FrameSlot::new(16)), -50);
        assert_eq!(layout.outbox_offset(0), Ok(-16));
        assert_eq!(layout.outbox_offset(15), Ok(-1));
        assert_eq!(layout.outbox_offset(16), Ok(-33));
        assert_eq!(layout.outbox_offset(17), Ok(-32));
        assert!(layout.frame_offset(FrameSlot::new(16)) < layout.outbox_offset(17).unwrap());
        assert!(layout.outbox_offset(0).unwrap() < layout.abi_offset(AbiField::Value));
    }

    #[test]
    fn aligned_arrays_fit_below_near_context_scalars_for_d16() {
        let first = FrameArrayId::new(0);
        let second = FrameArrayId::new(1);
        let layout = FrameLayout::with_arrays(
            AbiConfig::new(16).unwrap(),
            3,
            &[
                FrameArrayDescriptor::new(first, 17),
                FrameArrayDescriptor::new(second, 8),
            ],
            18,
        )
        .unwrap();

        assert_eq!(layout.value_chunks(), 1);
        assert_eq!(layout.array_chunk_count(first), Ok(3));
        assert_eq!(layout.array_chunk_count(second), Ok(2));
        assert_eq!(layout.array_chunks(), 5);
        assert_eq!(layout.outbox_chunks(), 2);
        assert_eq!(layout.context_chunks(), 1);
        assert_eq!(layout.frame_chunks(), 9);
        assert_eq!(layout.frame_offset(FrameSlot::new(0)), -50);
        assert_eq!(layout.array_base_offset(first), Ok(-136));
        assert_eq!(layout.array_portal_offset(first, AbiField::Value), Ok(-135));
        assert_eq!(layout.array_element_offset(first, 0), Ok(-118));
        assert_eq!(layout.array_element_offset(first, 16), Ok(-101));
        assert_eq!(layout.array_base_offset(second), Ok(-85));
        assert_eq!(layout.outbox_offset(0), Ok(-16));
    }

    #[test]
    fn checked_array_layout_rejects_invalid_descriptors_and_indices() {
        let config = AbiConfig::default();
        let array = FrameArrayId::new(0);
        assert_eq!(
            FrameLayout::with_arrays(config, 0, &[FrameArrayDescriptor::new(array, 0)], 0,),
            Err(FrameLayoutError::InvalidArrayLength { array, cells: 0 })
        );
        assert_eq!(
            FrameLayout::with_arrays(
                config,
                0,
                &[
                    FrameArrayDescriptor::new(array, 1),
                    FrameArrayDescriptor::new(array, 2),
                ],
                0,
            ),
            Err(FrameLayoutError::DuplicateFrameArray { array })
        );

        let layout =
            FrameLayout::with_arrays(config, 0, &[FrameArrayDescriptor::new(array, 1)], 0).unwrap();
        assert_eq!(
            layout.array_element_offset(array, 1),
            Err(FrameLayoutError::ArrayElementOutOfBounds {
                array,
                index: 1,
                cells: 1,
            })
        );
        assert_eq!(
            layout.array_base_offset(FrameArrayId::new(1)),
            Err(FrameLayoutError::UnknownFrameArray {
                array: FrameArrayId::new(1),
            })
        );
    }

    #[test]
    fn checked_access_rejects_cells_outside_their_regions() {
        let layout = FrameLayout::new(AbiConfig::default(), 1, 0).unwrap();

        assert_eq!(
            layout.try_frame_offset(FrameSlot::new(1)),
            Err(FrameLayoutError::FrameSlotOutOfBounds {
                slot: 1,
                value_cells: 1,
            })
        );
        assert_eq!(
            layout.outbox_offset(0),
            Err(FrameLayoutError::OutboxCellOutOfBounds {
                cell: 0,
                outbox_cells: 0,
            })
        );
    }

    #[test]
    fn minimum_main_capacity_includes_anchor_frame_and_frontier() {
        let d16 = FrameLayout::new(AbiConfig::new(16).unwrap(), 0, 0).unwrap();

        // D=16: anchor chunk 0..16, frame 17..33, frontier head 34.
        assert_eq!(d16.minimum_main_tape_cells(0), Ok(35));

        assert_eq!(d16.validate_main_capacity_in(29_965, 30_000), Ok(()));
        assert_eq!(
            d16.validate_main_capacity_in(29_966, 30_000),
            Err(FrameLayoutError::MainFrameDoesNotFit {
                anchor_head: 29_966,
                frame_chunks: 1,
                required_cells: 30_001,
                tape_cells: 30_000,
            })
        );
    }

    #[test]
    fn rejects_a_frame_that_cannot_fit_even_without_static_globals() {
        let error = FrameLayout::new(AbiConfig::new(16).unwrap(), 28_224, 0).unwrap_err();
        assert!(matches!(error, FrameLayoutError::FrameTooLarge { .. }));
    }

    #[test]
    fn version_one_aggregate_crosses_legacy_and_chunk_boundaries() {
        let aggregate = FrameAggregateId::new(3);
        for chunk_cells in [16] {
            let config = AbiConfig::new(chunk_cells).unwrap();
            let layout = FrameLayout::with_aggregates(
                config,
                0,
                &[FrameAggregateDescriptor::new(aggregate, 299)],
                0,
            )
            .unwrap();
            let base = layout.aggregate_base_offset(aggregate).unwrap();

            assert_eq!(
                layout.aggregate_chunk_count(aggregate),
                Ok((PROTOCOL_CELLS + 299).div_ceil(chunk_cells))
            );
            for index in [0, 255, 256, 298] {
                assert_eq!(
                    layout.aggregate_element_offset(aggregate, index),
                    Ok(base
                        + config
                            .try_logical_offset_from_head(PROTOCOL_CELLS + index)
                            .unwrap() as isize)
                );
            }
            assert_eq!(
                layout.aggregate_element_offset(aggregate, 299),
                Err(FrameLayoutError::AggregateElementOutOfBounds {
                    aggregate,
                    index: 299,
                    cells: 299,
                })
            );
        }
    }

    #[test]
    fn zero_sized_frame_aggregate_has_identity_without_storage() {
        let empty = FrameAggregateId::new(0);
        let full = FrameAggregateId::new(1);
        for chunk_cells in [16] {
            let config = AbiConfig::new(chunk_cells).unwrap();
            let mixed = FrameLayout::with_aggregates(
                config,
                2,
                &[
                    FrameAggregateDescriptor::new(empty, 0),
                    FrameAggregateDescriptor::new(full, 1),
                ],
                0,
            )
            .unwrap();
            let without_empty = FrameLayout::with_aggregates(
                config,
                2,
                &[FrameAggregateDescriptor::new(full, 1)],
                0,
            )
            .unwrap();

            assert_eq!(mixed.aggregate_chunk_count(empty), Ok(0));
            assert_eq!(mixed.aggregate_chunks(), without_empty.aggregate_chunks());
            assert_eq!(mixed.frame_chunks(), without_empty.frame_chunks());
            assert_eq!(
                mixed.aggregate_base_offset(full),
                without_empty.aggregate_base_offset(full)
            );
            assert_eq!(
                mixed.aggregate_base_offset(empty),
                Err(FrameLayoutError::ZeroSizedFrameAggregate { aggregate: empty })
            );
            assert_eq!(
                mixed.aggregate_portal_offset(empty, AbiField::Value),
                Err(FrameLayoutError::ZeroSizedFrameAggregate { aggregate: empty })
            );
            assert_eq!(
                mixed.aggregate_element_offset(empty, 0),
                Err(FrameLayoutError::AggregateElementOutOfBounds {
                    aggregate: empty,
                    index: 0,
                    cells: 0,
                })
            );
        }
    }

    #[test]
    fn aggregate_layout_checks_overflow_capacity_and_duplicate_ids() {
        let aggregate = FrameAggregateId::new(0);
        assert_eq!(
            FrameLayout::with_aggregates(
                AbiConfig::default(),
                0,
                &[FrameAggregateDescriptor::new(aggregate, usize::MAX)],
                0,
            ),
            Err(FrameLayoutError::SizeOverflow)
        );
        assert!(matches!(
            FrameLayout::with_aggregates(
                AbiConfig::default(),
                0,
                &[FrameAggregateDescriptor::new(aggregate, 30_000)],
                0,
            ),
            Err(FrameLayoutError::FrameTooLarge { .. })
        ));
        assert_eq!(
            FrameLayout::with_aggregates(
                AbiConfig::default(),
                0,
                &[
                    FrameAggregateDescriptor::new(aggregate, 0),
                    FrameAggregateDescriptor::new(aggregate, 1),
                ],
                0,
            ),
            Err(FrameLayoutError::DuplicateFrameAggregate { aggregate })
        );
    }
}
