//! Checked physical layout of file-scope objects and the stack anchor.
//!
//! Scalar globals occupy ordinary tape cells at the low-address end. Global
//! arrays follow in declaration order, with each array receiving its own
//! aligned 16-cell portal prefix. The first cell after the static regions is
//! the zero-valued stack anchor head.

use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use crate::continuation_ir::{GlobalDescriptor, GlobalId, ValueType};
use crate::frame_layout::{AbiConfig, AbiField, PROTOCOL_CELLS, TAPE_CELLS};

/// Absolute tape positions assigned to all file-scope objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticLayout {
    config: AbiConfig,
    globals: Vec<GlobalLayout>,
    scalar_cells: usize,
    anchor_head: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalLayout {
    Cell {
        id: GlobalId,
        position: usize,
    },
    Array {
        id: GlobalId,
        cells: usize,
        base_head: usize,
        chunks: usize,
    },
}

impl GlobalLayout {
    const fn id(self) -> GlobalId {
        match self {
            Self::Cell { id, .. } | Self::Array { id, .. } => id,
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
        validate_descriptors(descriptors)?;

        let scalar_cells = descriptors
            .iter()
            .filter(|descriptor| descriptor.value_type() == ValueType::Cell)
            .count();
        let mut globals = Vec::with_capacity(descriptors.len());

        let mut scalar_position = 0usize;
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

        let mut next_head = scalar_cells;
        for descriptor in descriptors {
            let ValueType::Array(cells) = descriptor.value_type() else {
                continue;
            };
            let logical_cells = PROTOCOL_CELLS
                .checked_add(cells)
                .ok_or(StaticLayoutError::SizeOverflow)?;
            let chunks = checked_chunks(logical_cells, config)?;
            let physical_cells = chunks
                .checked_mul(config.stride())
                .ok_or(StaticLayoutError::SizeOverflow)?;
            globals.push(GlobalLayout::Array {
                id: descriptor.id(),
                cells,
                base_head: next_head,
                chunks,
            });
            next_head = next_head
                .checked_add(physical_cells)
                .ok_or(StaticLayoutError::SizeOverflow)?;
        }

        let layout = Self {
            config,
            globals,
            scalar_cells,
            anchor_head: next_head,
        };
        layout.validate_capacity()?;
        Ok(layout)
    }

    pub const fn config(&self) -> AbiConfig {
        self.config
    }

    pub const fn scalar_cells(&self) -> usize {
        self.scalar_cells
    }

    /// Absolute position of the stack anchor head.
    pub const fn anchor_head(&self) -> usize {
        self.anchor_head
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
            GlobalLayout::Array { .. } => Err(StaticLayoutError::ExpectedScalar { global }),
        }
    }

    /// Absolute position of an aligned global array's aux/base head.
    pub fn array_base_head(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        match self.global_layout(global)? {
            GlobalLayout::Array { base_head, .. } => Ok(base_head),
            GlobalLayout::Cell { .. } => Err(StaticLayoutError::ExpectedArray { global }),
        }
    }

    pub fn array_chunk_count(&self, global: GlobalId) -> Result<usize, StaticLayoutError> {
        match self.global_layout(global)? {
            GlobalLayout::Array { chunks, .. } => Ok(chunks),
            GlobalLayout::Cell { .. } => Err(StaticLayoutError::ExpectedArray { global }),
        }
    }

    /// Absolute position of a statically indexed array payload element.
    pub fn array_element_position(
        &self,
        global: GlobalId,
        index: usize,
    ) -> Result<usize, StaticLayoutError> {
        let GlobalLayout::Array {
            cells, base_head, ..
        } = self.global_layout(global)?
        else {
            return Err(StaticLayoutError::ExpectedArray { global });
        };
        if index >= cells {
            return Err(StaticLayoutError::ArrayElementOutOfBounds {
                global,
                index,
                cells,
            });
        }
        checked_position(
            base_head,
            self.config,
            PROTOCOL_CELLS
                .checked_add(index)
                .ok_or(StaticLayoutError::SizeOverflow)?,
        )
    }

    /// Absolute position of one field in a global array's portal prefix.
    pub fn array_portal_field_position(
        &self,
        global: GlobalId,
        field: AbiField,
    ) -> Result<usize, StaticLayoutError> {
        let base_head = self.array_base_head(global)?;
        checked_position(base_head, self.config, field.index())
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

    fn mixed_descriptors() -> Vec<GlobalDescriptor> {
        vec![
            GlobalDescriptor::array(GlobalId::new(0), 3),
            GlobalDescriptor::cell(GlobalId::new(1)),
            GlobalDescriptor::array(GlobalId::new(2), 17),
            GlobalDescriptor::cell(GlobalId::new(3)),
        ]
    }

    #[test]
    fn mixed_globals_use_scalar_prefix_and_source_order_for_arrays() {
        let descriptors = mixed_descriptors();
        let d16 = StaticLayout::new(AbiConfig::new(16).unwrap(), &descriptors).unwrap();

        assert_eq!(d16.scalar_cells(), 2);
        assert_eq!(d16.scalar_position(GlobalId::new(1)), Ok(0));
        assert_eq!(d16.scalar_position(GlobalId::new(3)), Ok(1));
        assert_eq!(d16.array_base_head(GlobalId::new(0)), Ok(2));
        assert_eq!(d16.array_chunk_count(GlobalId::new(0)), Ok(2));
        assert_eq!(d16.array_base_head(GlobalId::new(2)), Ok(36));
        assert_eq!(d16.array_chunk_count(GlobalId::new(2)), Ok(3));
        assert_eq!(d16.anchor_head(), 87);

        let d8 = StaticLayout::new(AbiConfig::new(8).unwrap(), &descriptors).unwrap();
        assert_eq!(d8.array_base_head(GlobalId::new(0)), Ok(2));
        assert_eq!(d8.array_chunk_count(GlobalId::new(0)), Ok(3));
        assert_eq!(d8.array_base_head(GlobalId::new(2)), Ok(29));
        assert_eq!(d8.array_chunk_count(GlobalId::new(2)), Ok(5));
        assert_eq!(d8.anchor_head(), 74);
    }

    #[test]
    fn protocol_and_payload_positions_skip_chunk_heads_for_d8_and_d16() {
        let global = GlobalId::new(0);
        let descriptor = [GlobalDescriptor::array(global, 17)];
        for chunk_cells in [8, 16] {
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
    fn maximum_length_array_has_checked_boundaries_in_both_geometries() {
        let global = GlobalId::new(4);
        let descriptor = [GlobalDescriptor::array(global, 256)];
        for chunk_cells in [8, 16] {
            let config = AbiConfig::new(chunk_cells).unwrap();
            let layout = StaticLayout::new(config, &descriptor).unwrap();
            let expected_chunks = (PROTOCOL_CELLS + 256).div_ceil(chunk_cells);
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
            assert_eq!(layout.anchor_head(), expected_chunks * config.stride());
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
            AbiConfig::new(8).unwrap(),
            &[GlobalDescriptor::cell(GlobalId::new(0))],
        )
        .unwrap();
        assert_eq!(layout.anchor_head(), 1);
        assert_eq!(layout.minimum_tape_cells(), Ok(10));
        assert_eq!(layout.validate_capacity_in(10), Ok(()));
        assert_eq!(
            layout.validate_capacity_in(9),
            Err(StaticLayoutError::StaticAreaTooLarge {
                anchor_head: 1,
                required_cells: 10,
                tape_cells: 9,
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
}
