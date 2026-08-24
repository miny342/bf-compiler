//! Physical layout of scalar activation frames for the continuation ABI.
//!
//! A frame is made from chunks containing one allocation flag followed by
//! `chunk_cells` data cells. Value slots occupy the low-address end of the
//! frame, an optional aggregate-return outbox is immediately below the ABI
//! context, and the 16 protocol cells occupy the highest-address chunks.

use crate::continuation_ir::FrameSlot;
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
    /// Creates a supported ABI configuration.
    pub const fn new(chunk_cells: usize) -> Result<Self, FrameLayoutError> {
        match chunk_cells {
            8 | 16 => Ok(Self { chunk_cells }),
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
    value_chunks: usize,
    outbox_chunks: usize,
    context_chunks: usize,
    frame_chunks: usize,
}

impl FrameLayout {
    /// Lays out parameter, local, and temporary slots followed by outbox and
    /// context chunks. Passing zero outbox cells selects the scalar-only ABI.
    pub fn new(
        config: AbiConfig,
        value_cells: usize,
        outbox_cells: usize,
    ) -> Result<Self, FrameLayoutError> {
        let value_chunks = checked_chunks(value_cells, config)?;
        let outbox_chunks = checked_chunks(outbox_cells, config)?;
        let context_chunks = config.portal_chunks();
        let frame_chunks = value_chunks
            .checked_add(outbox_chunks)
            .and_then(|chunks| chunks.checked_add(context_chunks))
            .ok_or(FrameLayoutError::SizeOverflow)?;

        let layout = Self {
            config,
            value_cells,
            outbox_cells,
            value_chunks,
            outbox_chunks,
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

        let chunks_below_context = self.value_chunks + self.outbox_chunks;
        Ok(
            -(chunks_below_context as isize * self.config.stride() as isize)
                + self.config.try_logical_offset_from_head(index)? as isize,
        )
    }

    /// Offset of an outbox cell from the dispatch-context base head.
    /// Logical chunk zero is immediately below the context.
    pub fn outbox_offset(&self, index: usize) -> Result<isize, FrameLayoutError> {
        if index >= self.outbox_cells {
            return Err(FrameLayoutError::OutboxCellOutOfBounds {
                cell: index,
                outbox_cells: self.outbox_cells,
            });
        }

        let chunk = index / self.config.chunk_cells;
        let within = index % self.config.chunk_cells;
        Ok(-((chunk + 1) as isize * self.config.stride() as isize) + 1 + within as isize)
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
        let context_start = (self.value_chunks + self.outbox_chunks) * self.config.stride();
        let frame_end = self.frame_chunks * self.config.stride();
        context_start < frame_end
            && self.value_chunks * self.config.stride() <= context_start
            && self.outbox_chunks * self.config.stride()
                <= context_start - self.value_chunks * self.config.stride()
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
                "ABI chunk size must be 8 or 16 data cells, got {chunk_cells}"
            ),
            Self::SizeOverflow => write!(f, "frame layout size exceeds the host address space"),
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
        assert_eq!(
            AbiConfig::new(12),
            Err(FrameLayoutError::UnsupportedChunkCells { chunk_cells: 12 })
        );
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
    fn exact_layout_for_eight_cell_chunks() {
        let config = AbiConfig::new(8).unwrap();
        let layout = FrameLayout::new(config, 9, 0).unwrap();

        assert_eq!(config.stride(), 9);
        assert_eq!(config.portal_chunks(), 2);
        assert_eq!(layout.value_chunks(), 2);
        assert_eq!(layout.value_padding_cells(), 7);
        assert_eq!(layout.context_chunks(), 2);
        assert_eq!(layout.frame_chunks(), 4);
        assert_eq!(layout.physical_cells(), 36);

        assert_eq!(layout.frame_offset(FrameSlot::new(0)), -17);
        assert_eq!(layout.frame_offset(FrameSlot::new(7)), -10);
        assert_eq!(layout.frame_offset(FrameSlot::new(8)), -8);
        assert_eq!(layout.context_offset_from_frontier(), -18);
        assert_eq!(layout.frame_offset_from_frontier(FrameSlot::new(0)), -35);
        assert_eq!(layout.abi_offset(AbiField::Value), 1);
        assert_eq!(layout.abi_offset(AbiField::Restore), 8);
        assert_eq!(layout.abi_offset(AbiField::Branch), 10);
        assert_eq!(layout.abi_offset(AbiField::Scratch3), 17);
        assert_eq!(layout.abi_offset_from_frontier(AbiField::Value), -17);
        assert_eq!(layout.abi_offset_from_frontier(AbiField::Scratch3), -1);
    }

    #[test]
    fn outbox_is_reversed_below_context_and_disjoint_from_values() {
        let layout = FrameLayout::new(AbiConfig::new(8).unwrap(), 9, 10).unwrap();

        assert_eq!(layout.value_chunks(), 2);
        assert_eq!(layout.outbox_chunks(), 2);
        assert_eq!(layout.outbox_padding_cells(), 6);
        assert_eq!(layout.frame_chunks(), 6);
        assert_eq!(layout.frame_offset(FrameSlot::new(8)), -26);
        assert_eq!(layout.outbox_offset(0), Ok(-8));
        assert_eq!(layout.outbox_offset(7), Ok(-1));
        assert_eq!(layout.outbox_offset(8), Ok(-17));
        assert_eq!(layout.outbox_offset(9), Ok(-16));
        assert!(layout.frame_offset(FrameSlot::new(8)) < layout.outbox_offset(9).unwrap());
        assert!(layout.outbox_offset(0).unwrap() < layout.abi_offset(AbiField::Value));
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
        let d8 = FrameLayout::new(AbiConfig::new(8).unwrap(), 0, 0).unwrap();

        // D=16: anchor chunk 0..16, frame 17..33, frontier head 34.
        assert_eq!(d16.minimum_main_tape_cells(0), Ok(35));
        // D=8: anchor chunk 0..8, two context chunks 9..26, frontier 27.
        assert_eq!(d8.minimum_main_tape_cells(0), Ok(28));

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
}
