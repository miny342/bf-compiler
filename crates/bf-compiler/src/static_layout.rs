//! Checked physical layout of file-scope objects and the stack anchor.
//!
//! Global aggregate regions occupy the low-address end. Regions with at most
//! one payload chunk follow larger regions, keeping small structs near the
//! stack anchor instead of behind large arenas. Each group retains reverse
//! declaration order, so low-numbered arena regions remain nearest the anchor.
//! Every nonempty aggregate receives an aligned portal prefix; zero-sized
//! aggregates retain identity without consuming tape. Scalar globals and the
//! D=16 remote-copy scratch sit next to the stack anchor, keeping their emitted
//! navigation templates bounded even when aggregate storage is very large.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use crate::continuation_ir::{GlobalDescriptor, GlobalId, ValueType};
use crate::frame_layout::{
    AbiConfig, AbiField, TAPE_CELLS, aggregate_element_physical_offset, aggregate_region_chunks,
};

/// Absolute tape positions assigned to all file-scope objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticLayout {
    config: AbiConfig,
    globals: Vec<GlobalLayout>,
    scalar_cells: usize,
    remote_copy_scratch_start: Option<usize>,
    anchor_head: usize,
}

pub(crate) const REMOTE_COPY_SCRATCH_CELLS: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalLayout {
    Cell {
        id: GlobalId,
        position: usize,
    },
    Aggregate {
        id: GlobalId,
        cells: usize,
        base_head: usize,
        chunks: usize,
    },
}

impl GlobalLayout {
    const fn id(self) -> GlobalId {
        match self {
            Self::Cell { id, .. } | Self::Aggregate { id, .. } => id,
        }
    }
}

impl StaticLayout {
    /// Lay out globals and verify that the static prefix and anchor chunk fit
    /// on the ABI's guaranteed tape.
    pub fn new(
        config: AbiConfig,
        descriptors: &[GlobalDescriptor],
    ) -> Result<Self, StaticLayoutError> {
        Self::new_with_capacity_check(config, descriptors, true)
    }

    pub(crate) fn new_unbounded(
        config: AbiConfig,
        descriptors: &[GlobalDescriptor],
    ) -> Result<Self, StaticLayoutError> {
        Self::new_with_capacity_check(config, descriptors, false)
    }

    fn new_with_capacity_check(
        config: AbiConfig,
        descriptors: &[GlobalDescriptor],
        check_capacity: bool,
    ) -> Result<Self, StaticLayoutError> {
        validate_descriptors(descriptors)?;

        let scalar_cells = descriptors
            .iter()
            .filter(|descriptor| descriptor.value_type() == ValueType::Cell)
            .count();
        let mut globals = Vec::with_capacity(descriptors.len());
        let mut next_head = 0usize;
        let mut aggregates = descriptors
            .iter()
            .rev()
            .filter_map(|descriptor| match descriptor.value_type() {
                ValueType::Array(cells) | ValueType::Aggregate { cells } => {
                    Some((descriptor, cells))
                }
                ValueType::Cell | ValueType::Void => None,
            })
            .collect::<Vec<_>>();
        // A stable partition preserves reverse declaration order within both
        // groups. Portal prefixes and total storage are unchanged; only region
        // bases move. With D=16, the near-anchor group holds up to 16 cells.
        aggregates.sort_by_key(|(_, cells)| *cells <= config.chunk_cells());
        for (descriptor, cells) in aggregates {
            let chunks = aggregate_region_chunks(cells, config)
                .map_err(|_| StaticLayoutError::SizeOverflow)?;
            let physical_cells = chunks
                .checked_mul(config.stride())
                .ok_or(StaticLayoutError::SizeOverflow)?;
            globals.push(GlobalLayout::Aggregate {
                id: descriptor.id(),
                cells,
                base_head: next_head,
                chunks,
            });
            next_head = next_head
                .checked_add(physical_cells)
                .ok_or(StaticLayoutError::SizeOverflow)?;
        }

        let mut scalar_position = next_head;
        for descriptor in descriptors {
            if descriptor.value_type() == ValueType::Cell {
                globals.push(GlobalLayout::Cell {
                    id: descriptor.id(),
                    position: scalar_position,
                });
                scalar_position = scalar_position
                    .checked_add(1)
                    .ok_or(StaticLayoutError::SizeOverflow)?;
            }
        }
        let remote_copy_scratch_start = Some(scalar_position);
        next_head = scalar_position
            .checked_add(REMOTE_COPY_SCRATCH_CELLS)
            .ok_or(StaticLayoutError::SizeOverflow)?;

        let layout = Self {
            config,
            globals,
            scalar_cells,
            remote_copy_scratch_start,
            anchor_head: next_head,
        };
        if check_capacity {
            layout.validate_capacity()?;
        }
        Ok(layout)
    }

    pub const fn config(&self) -> AbiConfig {
        self.config
    }

    pub const fn scalar_cells(&self) -> usize {
        self.scalar_cells
    }

    pub(crate) fn remote_copy_scratch_position(&self, index: usize) -> Option<usize> {
        self.remote_copy_scratch_start
            .filter(|_| index < REMOTE_COPY_SCRATCH_CELLS)
            .and_then(|start| start.checked_add(index))
    }

    /// Absolute position of the stack anchor head.
    pub const fn anchor_head(&self) -> usize {
        self.anchor_head
    }

    /// Reserve compiler-owned static storage before the dynamic stack anchor.
    /// User global addresses stay unchanged.
    pub(crate) fn reserve_internal_cells(
        &mut self,
        cells: usize,
    ) -> Result<usize, StaticLayoutError> {
        let start = self.anchor_head;
        self.anchor_head = start
            .checked_add(cells)
            .ok_or(StaticLayoutError::SizeOverflow)?;
        Ok(start)
    }

    /// Minimum tape length containing the static regions and the complete
    /// anchor chunk, including its ABI scratch cells.
    pub fn minimum_tape_cells(&self) -> Result<usize, StaticLayoutError> {
        self.anchor_head
            .checked_add(self.config.stride())
            .ok_or(StaticLayoutError::SizeOverflow)
    }

    pub fn validate_capacity(&self) -> Result<(), StaticLayoutError> {
        self.validate_capacity_in(TAPE_CELLS)
    }

    /// Capacity check against an explicit tape size, primarily useful when
    /// composing this prefix with a frame layout and in tests.
    pub fn validate_capacity_in(&self, tape_cells: usize) -> Result<(), StaticLayoutError> {
        let required_cells = self.minimum_tape_cells()?;
        if required_cells > tape_cells {
            return Err(StaticLayoutError::StaticAreaTooLarge {
                anchor_head: self.anchor_head,
                required_cells,
                tape_cells,
            });
        }
        Ok(())
    }

    /// Absolute position of an ordinary scalar global.
    pub fn scalar_position(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        match self.global_layout(global)? {
            GlobalLayout::Cell { position, .. } => Ok(position),
            GlobalLayout::Aggregate { .. } => Err(StaticLayoutError::ExpectedScalar { global }),
        }
    }

    /// Absolute position of an aligned global aggregate's aux/base head.
    pub fn aggregate_base_head(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        match self.global_layout(global)? {
            GlobalLayout::Aggregate { cells: 0, .. } => {
                Err(StaticLayoutError::ZeroSizedAggregate { global })
            }
            GlobalLayout::Aggregate { base_head, .. } => Ok(base_head),
            GlobalLayout::Cell { .. } => Err(StaticLayoutError::ExpectedAggregate { global }),
        }
    }

    pub fn aggregate_chunk_count(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        match self.global_layout(global)? {
            GlobalLayout::Aggregate { chunks, .. } => Ok(chunks),
            GlobalLayout::Cell { .. } => Err(StaticLayoutError::ExpectedAggregate { global }),
        }
    }

    /// Absolute position of a statically indexed aggregate payload cell.
    pub fn aggregate_element_position(
        &self,
        global: GlobalId,
        index: usize,
    ) -> Result<usize, StaticLayoutError> {
        let GlobalLayout::Aggregate {
            cells, base_head, ..
        } = self.global_layout(global)?
        else {
            return Err(StaticLayoutError::ExpectedAggregate { global });
        };
        if index >= cells {
            return Err(StaticLayoutError::AggregateElementOutOfBounds {
                global,
                index,
                cells,
            });
        }
        let offset = aggregate_element_physical_offset(index, self.config)
            .map_err(|_| StaticLayoutError::SizeOverflow)?;
        base_head
            .checked_add(offset)
            .ok_or(StaticLayoutError::SizeOverflow)
    }

    /// Absolute position of one field in a global aggregate's portal prefix.
    pub fn aggregate_portal_field_position(
        &self,
        global: GlobalId,
        field: AbiField,
    ) -> Result<usize, StaticLayoutError> {
        let base_head = self.aggregate_base_head(global)?;
        checked_position(base_head, self.config, field.index())
    }

    /// Version-0 compatibility alias for [`Self::aggregate_base_head`].
    pub fn array_base_head(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        self.aggregate_base_head(global).map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_chunk_count`].
    pub fn array_chunk_count(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        self.aggregate_chunk_count(global)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_element_position`].
    pub fn array_element_position(
        &self,
        global: GlobalId,
        index: usize,
    ) -> Result<usize, StaticLayoutError> {
        self.aggregate_element_position(global, index)
            .map_err(array_compat_error)
    }

    /// Version-0 compatibility alias for [`Self::aggregate_portal_field_position`].
    pub fn array_portal_field_position(
        &self,
        global: GlobalId,
        field: AbiField,
    ) -> Result<usize, StaticLayoutError> {
        self.aggregate_portal_field_position(global, field)
            .map_err(array_compat_error)
    }

    fn global_layout(&self, global: GlobalId) -> Result<GlobalLayout, StaticLayoutError> {
        self.globals
            .iter()
            .copied()
            .find(|layout| layout.id() == global)
            .ok_or(StaticLayoutError::UnknownGlobal { global })
    }
}

fn validate_descriptors(descriptors: &[GlobalDescriptor]) -> Result<(), StaticLayoutError> {
    let mut ids = HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        if !ids.insert(descriptor.id()) {
            return Err(StaticLayoutError::DuplicateGlobalId {
                global: descriptor.id(),
            });
        }
        match descriptor.value_type() {
            ValueType::Cell => {}
            ValueType::Array(cells) if (1..=256).contains(&cells) => {}
            ValueType::Array(cells) => {
                return Err(StaticLayoutError::InvalidArrayLength {
                    global: descriptor.id(),
                    cells,
                });
            }
            ValueType::Aggregate { .. } => {}
            ValueType::Void => {
                return Err(StaticLayoutError::InvalidGlobalType {
                    global: descriptor.id(),
                    actual: ValueType::Void,
                });
            }
        }
    }
    Ok(())
}

fn array_compat_error(error: StaticLayoutError) -> StaticLayoutError {
    match error {
        StaticLayoutError::ExpectedAggregate { global } => {
            StaticLayoutError::ExpectedArray { global }
        }
        StaticLayoutError::AggregateElementOutOfBounds {
            global,
            index,
            cells,
        } => StaticLayoutError::ArrayElementOutOfBounds {
            global,
            index,
            cells,
        },
        other => other,
    }
}

#[cfg(test)]
fn checked_chunks(cells: usize, config: AbiConfig) -> Result<usize, StaticLayoutError> {
    cells
        .checked_add(config.chunk_cells() - 1)
        .map(|rounded| rounded / config.chunk_cells())
        .ok_or(StaticLayoutError::SizeOverflow)
}

fn checked_position(
    base_head: usize,
    config: AbiConfig,
    logical_cell: usize,
) -> Result<usize, StaticLayoutError> {
    let chunk = logical_cell / config.chunk_cells();
    let within = logical_cell % config.chunk_cells();
    chunk
        .checked_mul(config.stride())
        .and_then(|offset| offset.checked_add(1))
        .and_then(|offset| offset.checked_add(within))
        .and_then(|offset| base_head.checked_add(offset))
        .ok_or(StaticLayoutError::SizeOverflow)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticLayoutError {
    DuplicateGlobalId {
        global: GlobalId,
    },
    InvalidGlobalType {
        global: GlobalId,
        actual: ValueType,
    },
    InvalidArrayLength {
        global: GlobalId,
        cells: usize,
    },
    UnknownGlobal {
        global: GlobalId,
    },
    ExpectedScalar {
        global: GlobalId,
    },
    ExpectedArray {
        global: GlobalId,
    },
    ExpectedAggregate {
        global: GlobalId,
    },
    ZeroSizedAggregate {
        global: GlobalId,
    },
    AggregateElementOutOfBounds {
        global: GlobalId,
        index: usize,
        cells: usize,
    },
    ArrayElementOutOfBounds {
        global: GlobalId,
        index: usize,
        cells: usize,
    },
    SizeOverflow,
    StaticAreaTooLarge {
        anchor_head: usize,
        required_cells: usize,
        tape_cells: usize,
    },
}

impl fmt::Display for StaticLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateGlobalId { global } => {
                write!(f, "global ID {} occurs more than once", global.index())
            }
            Self::InvalidGlobalType { global, actual } => {
                write!(f, "global {} has invalid type {actual:?}", global.index())
            }
            Self::InvalidArrayLength { global, cells } => write!(
                f,
                "global array {} has length {cells}; expected 1..=256",
                global.index()
            ),
            Self::UnknownGlobal { global } => {
                write!(f, "global {} has no static layout", global.index())
            }
            Self::ExpectedScalar { global } => {
                write!(f, "global {} is not a scalar", global.index())
            }
            Self::ExpectedArray { global } => {
                write!(f, "global {} is not an array", global.index())
            }
            Self::ExpectedAggregate { global } => {
                write!(f, "global {} is not an aggregate", global.index())
            }
            Self::ZeroSizedAggregate { global } => write!(
                f,
                "zero-sized global aggregate {} has no physical base or portal",
                global.index()
            ),
            Self::AggregateElementOutOfBounds {
                global,
                index,
                cells,
            } => write!(
                f,
                "aggregate cell {index} is outside global {}'s {cells}-cell payload",
                global.index()
            ),
            Self::ArrayElementOutOfBounds {
                global,
                index,
                cells,
            } => write!(
                f,
                "array element {index} is outside global {}'s {cells}-cell payload",
                global.index()
            ),
            Self::SizeOverflow => write!(f, "static layout size overflowed usize"),
            Self::StaticAreaTooLarge {
                anchor_head,
                required_cells,
                tape_cells,
            } => write!(
                f,
                "static area with anchor at {anchor_head} needs {required_cells} tape cells, but only {tape_cells} are available"
            ),
        }
    }
}

impl Error for StaticLayoutError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_layout::PROTOCOL_CELLS;

    fn mixed_descriptors() -> Vec<GlobalDescriptor> {
        vec![
            GlobalDescriptor::array(GlobalId::new(0), 3),
            GlobalDescriptor::cell(GlobalId::new(1)),
            GlobalDescriptor::array(GlobalId::new(2), 17),
            GlobalDescriptor::cell(GlobalId::new(3)),
        ]
    }

    #[test]
    fn mixed_globals_keep_scalars_near_anchor_and_low_arrays_near_anchor() {
        let descriptors = mixed_descriptors();
        let d16 = StaticLayout::new(AbiConfig::new(16).unwrap(), &descriptors).unwrap();

        assert_eq!(d16.scalar_cells(), 2);
        assert_eq!(d16.scalar_position(GlobalId::new(1)), Ok(85));
        assert_eq!(d16.scalar_position(GlobalId::new(3)), Ok(86));
        assert_eq!(d16.remote_copy_scratch_position(0), Some(87));
        assert_eq!(d16.remote_copy_scratch_position(8), Some(95));
        assert_eq!(d16.remote_copy_scratch_position(9), None);
        assert_eq!(d16.array_base_head(GlobalId::new(0)), Ok(51));
        assert_eq!(d16.array_chunk_count(GlobalId::new(0)), Ok(2));
        assert_eq!(d16.array_base_head(GlobalId::new(2)), Ok(0));
        assert_eq!(d16.array_chunk_count(GlobalId::new(2)), Ok(3));
        assert_eq!(d16.anchor_head(), 96);
    }

    #[test]
    fn small_aggregates_follow_large_regions_without_reordering_either_group() {
        let large = GlobalId::new(7);
        let small = GlobalId::new(3);
        let paged = GlobalId::new(19);
        let one_chunk = GlobalId::new(40);
        let scalar = GlobalId::new(0);
        let empty = GlobalId::new(21);
        let layout = StaticLayout::new(
            AbiConfig::default(),
            &[
                GlobalDescriptor::array(large, 17),
                GlobalDescriptor::aggregate(small, 3),
                GlobalDescriptor::aggregate(paged, 257),
                GlobalDescriptor::array(one_chunk, 16),
                GlobalDescriptor::cell(scalar),
                GlobalDescriptor::aggregate(empty, 0),
            ],
        )
        .unwrap();

        // Declaration order, not numeric GlobalId order, is reversed in each
        // group. The 16/17 boundary applies to both legacy arrays and structs.
        assert_eq!(layout.aggregate_base_head(paged), Ok(0));
        assert_eq!(layout.aggregate_base_head(large), Ok(323));
        assert_eq!(layout.aggregate_base_head(one_chunk), Ok(374));
        assert_eq!(layout.aggregate_base_head(small), Ok(408));
        assert_eq!(layout.aggregate_chunk_count(empty), Ok(0));
        assert_eq!(layout.scalar_position(scalar), Ok(442));
        assert_eq!(layout.anchor_head(), 452);
    }

    #[test]
    fn protocol_and_payload_positions_skip_chunk_heads_for_d16() {
        let global = GlobalId::new(0);
        let descriptor = [GlobalDescriptor::array(global, 17)];
        {
            let chunk_cells = 16;
            let config = AbiConfig::new(chunk_cells).unwrap();
            let layout = StaticLayout::new(config, &descriptor).unwrap();
            let base = layout.array_base_head(global).unwrap();

            assert_eq!(
                layout
                    .array_portal_field_position(global, AbiField::Value)
                    .unwrap(),
                base + 1
            );
            assert_eq!(
                layout
                    .array_portal_field_position(global, AbiField::Branch)
                    .unwrap(),
                base + config.logical_offset_from_head(AbiField::Branch.index())
            );
            assert_eq!(
                layout.array_element_position(global, 0).unwrap(),
                base + config.logical_offset_from_head(PROTOCOL_CELLS)
            );
            assert_eq!(
                layout.array_element_position(global, 16).unwrap(),
                base + config.logical_offset_from_head(PROTOCOL_CELLS + 16)
            );
        }
    }

    #[test]
    fn maximum_length_array_has_checked_boundaries_in_sixteen_cell_chunks() {
        let global = GlobalId::new(4);
        let descriptor = [GlobalDescriptor::array(global, 256)];
        {
            let chunk_cells = 16;
            let config = AbiConfig::new(chunk_cells).unwrap();
            let layout = StaticLayout::new(config, &descriptor).unwrap();
            let expected_chunks = (PROTOCOL_CELLS + 256).div_ceil(chunk_cells);
            let scratch_cells = if chunk_cells >= REMOTE_COPY_SCRATCH_CELLS {
                REMOTE_COPY_SCRATCH_CELLS
            } else {
                0
            };
            assert_eq!(layout.array_chunk_count(global), Ok(expected_chunks));
            assert_eq!(
                layout.array_element_position(global, 255).unwrap(),
                config.logical_offset_from_head(PROTOCOL_CELLS + 255)
            );
            assert_eq!(
                layout.array_element_position(global, 256),
                Err(StaticLayoutError::ArrayElementOutOfBounds {
                    global,
                    index: 256,
                    cells: 256,
                })
            );
            assert_eq!(
                layout.anchor_head(),
                scratch_cells + expected_chunks * config.stride()
            );
        }
    }

    #[test]
    fn rejects_invalid_descriptors_and_wrong_lookup_kinds() {
        let duplicate = GlobalId::new(7);
        assert_eq!(
            StaticLayout::new(
                AbiConfig::default(),
                &[
                    GlobalDescriptor::cell(duplicate),
                    GlobalDescriptor::array(duplicate, 1),
                ],
            ),
            Err(StaticLayoutError::DuplicateGlobalId { global: duplicate })
        );

        for cells in [0, 257] {
            let global = GlobalId::new(cells);
            assert_eq!(
                StaticLayout::new(
                    AbiConfig::default(),
                    &[GlobalDescriptor::array(global, cells)]
                ),
                Err(StaticLayoutError::InvalidArrayLength { global, cells })
            );
        }

        let void = GlobalId::new(9);
        assert_eq!(
            StaticLayout::new(
                AbiConfig::default(),
                &[GlobalDescriptor::new(void, ValueType::Void)]
            ),
            Err(StaticLayoutError::InvalidGlobalType {
                global: void,
                actual: ValueType::Void,
            })
        );

        let layout = StaticLayout::new(
            AbiConfig::default(),
            &[
                GlobalDescriptor::cell(GlobalId::new(0)),
                GlobalDescriptor::array(GlobalId::new(1), 1),
            ],
        )
        .unwrap();
        assert_eq!(
            layout.array_base_head(GlobalId::new(0)),
            Err(StaticLayoutError::ExpectedArray {
                global: GlobalId::new(0)
            })
        );
        assert_eq!(
            layout.scalar_position(GlobalId::new(1)),
            Err(StaticLayoutError::ExpectedScalar {
                global: GlobalId::new(1)
            })
        );
        assert_eq!(
            layout.scalar_position(GlobalId::new(2)),
            Err(StaticLayoutError::UnknownGlobal {
                global: GlobalId::new(2)
            })
        );
    }

    #[test]
    fn capacity_includes_the_complete_anchor_chunk() {
        let layout = StaticLayout::new(
            AbiConfig::default(),
            &[GlobalDescriptor::cell(GlobalId::new(0))],
        )
        .unwrap();
        assert_eq!(layout.anchor_head(), 10);
        assert_eq!(layout.minimum_tape_cells(), Ok(27));
        assert_eq!(layout.validate_capacity_in(27), Ok(()));
        assert_eq!(
            layout.validate_capacity_in(26),
            Err(StaticLayoutError::StaticAreaTooLarge {
                anchor_head: 10,
                required_cells: 27,
                tape_cells: 26,
            })
        );
    }

    #[test]
    fn checked_arithmetic_reports_size_overflow() {
        let config = AbiConfig::new(16).unwrap();
        assert_eq!(
            checked_chunks(usize::MAX, config),
            Err(StaticLayoutError::SizeOverflow)
        );
        assert_eq!(
            checked_position(usize::MAX, config, 0),
            Err(StaticLayoutError::SizeOverflow)
        );
    }

    #[test]
    fn version_one_global_aggregate_crosses_legacy_and_chunk_boundaries() {
        let global = GlobalId::new(4);
        let descriptor = [GlobalDescriptor::aggregate(global, 299)];
        {
            let chunk_cells = 16;
            let config = AbiConfig::new(chunk_cells).unwrap();
            let layout = StaticLayout::new(config, &descriptor).unwrap();
            let base = layout.aggregate_base_head(global).unwrap();

            assert_eq!(
                layout.aggregate_chunk_count(global),
                Ok(aggregate_region_chunks(299, config).unwrap())
            );
            for index in [0, 255, 256, 298] {
                assert_eq!(
                    layout.aggregate_element_position(global, index),
                    base.checked_add(aggregate_element_physical_offset(index, config).unwrap())
                        .ok_or(StaticLayoutError::SizeOverflow)
                );
            }
            assert_eq!(
                layout.aggregate_element_position(global, 299),
                Err(StaticLayoutError::AggregateElementOutOfBounds {
                    global,
                    index: 299,
                    cells: 299,
                })
            );
        }
    }

    #[test]
    fn zero_sized_global_aggregate_has_identity_without_storage() {
        let empty = GlobalId::new(0);
        let scalar = GlobalId::new(1);
        let full = GlobalId::new(2);
        {
            let chunk_cells = 16;
            let config = AbiConfig::new(chunk_cells).unwrap();
            let mixed = StaticLayout::new(
                config,
                &[
                    GlobalDescriptor::aggregate(empty, 0),
                    GlobalDescriptor::cell(scalar),
                    GlobalDescriptor::aggregate(full, 1),
                ],
            )
            .unwrap();
            let without_empty = StaticLayout::new(
                config,
                &[
                    GlobalDescriptor::cell(scalar),
                    GlobalDescriptor::aggregate(full, 1),
                ],
            )
            .unwrap();

            assert_eq!(mixed.aggregate_chunk_count(empty), Ok(0));
            assert_eq!(mixed.anchor_head(), without_empty.anchor_head());
            assert_eq!(
                mixed.aggregate_base_head(full),
                without_empty.aggregate_base_head(full)
            );
            assert_eq!(
                mixed.aggregate_base_head(empty),
                Err(StaticLayoutError::ZeroSizedAggregate { global: empty })
            );
            assert_eq!(
                mixed.aggregate_portal_field_position(empty, AbiField::Value),
                Err(StaticLayoutError::ZeroSizedAggregate { global: empty })
            );
            assert_eq!(
                mixed.aggregate_element_position(empty, 0),
                Err(StaticLayoutError::AggregateElementOutOfBounds {
                    global: empty,
                    index: 0,
                    cells: 0,
                })
            );
        }
    }

    #[test]
    fn aggregate_static_layout_checks_overflow_and_capacity() {
        let global = GlobalId::new(0);
        assert_eq!(
            StaticLayout::new(
                AbiConfig::default(),
                &[GlobalDescriptor::aggregate(global, usize::MAX)]
            ),
            Err(StaticLayoutError::SizeOverflow)
        );
        assert!(matches!(
            StaticLayout::new(
                AbiConfig::default(),
                &[GlobalDescriptor::aggregate(global, 30_000)]
            ),
            Err(StaticLayoutError::StaticAreaTooLarge { .. })
        ));
    }
}
