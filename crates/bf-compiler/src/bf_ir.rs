//! Brainfuck-shaped intermediate representation.

use std::collections::BTreeMap;
use std::io::{self, Write};

use bf_profiling::ProfileSiteId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSiteRecord {
    pub id: ProfileSiteId,
    pub parent: Option<ProfileSiteId>,
    pub kind: String,
    pub stable_key: String,
    pub label: String,
    pub attributes: BTreeMap<String, String>,
}

/// A profile site table owned by one generated artifact.
///
/// The wire-format representation lives in `bf-profiling`; this compact
/// compiler-side form is deliberately independent from the public BF IR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSiteTable {
    sites: Vec<ProfileSiteRecord>,
}

impl ProfileSiteTable {
    pub fn with_root(label: impl Into<String>) -> Self {
        Self {
            sites: vec![ProfileSiteRecord {
                id: ProfileSiteId(0),
                parent: None,
                kind: "artifact".into(),
                stable_key: "artifact.root".into(),
                label: label.into(),
                attributes: BTreeMap::new(),
            }],
        }
    }

    pub fn new(parents: Vec<Option<ProfileSiteId>>) -> Self {
        let sites = parents
            .into_iter()
            .enumerate()
            .map(|(id, parent)| ProfileSiteRecord {
                id: ProfileSiteId(id as u32),
                parent,
                kind: String::new(),
                stable_key: String::new(),
                label: String::new(),
                attributes: BTreeMap::new(),
            })
            .collect();
        Self { sites }
    }

    pub fn parent(&self, site: ProfileSiteId) -> Option<ProfileSiteId> {
        self.sites.get(site.0 as usize).and_then(|site| site.parent)
    }

    pub fn records(&self) -> &[ProfileSiteRecord] {
        &self.sites
    }

    pub fn intern(
        &mut self,
        parent: Option<ProfileSiteId>,
        kind: impl Into<String>,
        stable_key: impl Into<String>,
        label: impl Into<String>,
        attributes: BTreeMap<String, String>,
    ) -> ProfileSiteId {
        let kind = kind.into();
        let stable_key = stable_key.into();
        if let Some(site) = self.sites.iter().find(|site| {
            site.parent == parent && site.kind == kind && site.stable_key == stable_key
        }) {
            return site.id;
        }
        let id = ProfileSiteId(self.sites.len() as u32);
        self.sites.push(ProfileSiteRecord {
            id,
            parent,
            kind,
            stable_key,
            label: label.into(),
            attributes,
        });
        id
    }

    /// The lowest common ancestor of two sites in this artifact.
    pub fn lowest_common_ancestor(
        &self,
        left: ProfileSiteId,
        right: ProfileSiteId,
    ) -> ProfileSiteId {
        if left == right {
            return left;
        }
        let mut ancestors = std::collections::BTreeSet::new();
        let mut current = Some(left);
        while let Some(site) = current {
            ancestors.insert(site.0);
            current = self.parent(site);
        }
        let mut current = Some(right);
        while let Some(site) = current {
            if ancestors.contains(&site.0) {
                return site;
            }
            current = self.parent(site);
        }
        ProfileSiteId(0)
    }
}

/// A BF operation with compiler provenance.  This is intentionally parallel
/// to the stable public [`BfInstruction`] API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnnotatedBfOperation {
    Move(isize),
    Add(u8),
    Input,
    Output,
    Loop(Vec<AnnotatedBfInstruction>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatedBfInstruction {
    pub site: ProfileSiteId,
    pub operation: AnnotatedBfOperation,
}

impl AnnotatedBfInstruction {
    pub const fn new(site: ProfileSiteId, operation: AnnotatedBfOperation) -> Self {
        Self { site, operation }
    }

    pub fn into_plain(self) -> BfInstruction {
        match self.operation {
            AnnotatedBfOperation::Move(amount) => BfInstruction::Move(amount),
            AnnotatedBfOperation::Add(value) => BfInstruction::Add(value),
            AnnotatedBfOperation::Input => BfInstruction::Input,
            AnnotatedBfOperation::Output => BfInstruction::Output,
            AnnotatedBfOperation::Loop(body) => {
                BfInstruction::Loop(body.into_iter().map(Self::into_plain).collect())
            }
        }
    }
}

/// A complete annotated program, including its provenance hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatedBfProgram {
    instructions: Vec<AnnotatedBfInstruction>,
    sites: ProfileSiteTable,
}

impl AnnotatedBfProgram {
    pub fn new(instructions: Vec<AnnotatedBfInstruction>, sites: ProfileSiteTable) -> Self {
        Self {
            instructions,
            sites,
        }
    }

    pub fn instructions(&self) -> &[AnnotatedBfInstruction] {
        &self.instructions
    }
    pub fn sites(&self) -> &ProfileSiteTable {
        &self.sites
    }
    pub fn into_plain(self) -> BfProgram {
        BfProgram::new(
            self.instructions
                .into_iter()
                .map(AnnotatedBfInstruction::into_plain)
                .collect(),
        )
    }

    /// Serialize and produce coalesced, half-open BF-instruction ranges.
    pub fn to_source_and_ranges(&self) -> (String, Vec<(u64, u64, ProfileSiteId)>) {
        let mut source = String::new();
        let mut ranges = Vec::new();
        let mut ordinal = 0;
        write_annotated(&self.instructions, &mut source, &mut ordinal, &mut ranges);
        (source, ranges)
    }

    pub fn to_source(&self) -> String {
        self.to_source_and_ranges().0
    }

    /// Build the versioned sidecar profile map for this program. Source spans
    /// are not yet carried by continuation IR, so compiler-generated sites
    /// currently have `source: null`.
    pub fn profile_map(&self) -> bf_profiling::ProfileMap {
        let (source, ranges) = self.to_source_and_ranges();
        bf_profiling::ProfileMap {
            format: bf_profiling::PROFILE_MAP_FORMAT.to_owned(),
            version: bf_profiling::PROFILE_MAP_VERSION,
            bf: bf_profiling::bf_identity(source.as_bytes()),
            files: Vec::new(),
            sites: self
                .sites
                .records()
                .iter()
                .map(|site| bf_profiling::ProfileSite {
                    id: site.id,
                    parent: site.parent,
                    kind: site.kind.clone(),
                    stable_key: site.stable_key.clone(),
                    label: site.label.clone(),
                    source: None,
                    attributes: site.attributes.clone(),
                })
                .collect(),
            ranges: ranges
                .into_iter()
                .map(|(start, end, site)| bf_profiling::ProfileRange { start, end, site })
                .collect(),
        }
    }
}

/// A Brainfuck operation before serialization to source text.
///
/// Movement and addition store their complete amount in one node. No
/// normalization or merging of adjacent nodes is performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BfInstruction {
    /// Move right when positive and left when negative.
    Move(isize),
    /// Add to the current cell modulo 256.
    Add(u8),
    Input,
    Output,
    Loop(Vec<BfInstruction>),
}

/// A complete BF IR program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BfProgram {
    instructions: Vec<BfInstruction>,
}

impl BfProgram {
    pub fn new(instructions: Vec<BfInstruction>) -> Self {
        Self { instructions }
    }

    pub fn instructions(&self) -> &[BfInstruction] {
        &self.instructions
    }

    pub fn into_instructions(self) -> Vec<BfInstruction> {
        self.instructions
    }

    /// Serialize the BF IR to Brainfuck source.
    ///
    /// An `Add` node uses the shorter equivalent run of `+` or `-`. This is
    /// the encoding of that node, not an optimization pass: adjacent nodes are
    /// not inspected or merged.
    pub fn to_source(&self) -> String {
        let mut source = String::new();
        write_instructions(&self.instructions, &mut source);
        source
    }

    /// Exact serialized Brainfuck byte count without materializing the source.
    pub fn source_len(&self) -> u64 {
        source_len(&self.instructions)
    }

    /// Serialize directly to a byte stream, bounding memory independently of
    /// long pointer movements in the generated program.
    pub fn write_source(&self, output: &mut impl Write) -> io::Result<()> {
        write_instructions_streaming(&self.instructions, output)
    }
}

fn source_len(instructions: &[BfInstruction]) -> u64 {
    instructions
        .iter()
        .map(|instruction| match instruction {
            BfInstruction::Move(amount) => amount.unsigned_abs() as u64,
            BfInstruction::Add(value) if *value <= 128 => u64::from(*value),
            BfInstruction::Add(value) => u64::from(256_u16 - u16::from(*value)),
            BfInstruction::Input | BfInstruction::Output => 1,
            BfInstruction::Loop(body) => 2 + source_len(body),
        })
        .sum()
}

fn write_instructions_streaming(
    instructions: &[BfInstruction],
    output: &mut impl Write,
) -> io::Result<()> {
    fn repeated(output: &mut impl Write, byte: u8, mut count: usize) -> io::Result<()> {
        let buffer = [byte; 8192];
        while count != 0 {
            let chunk = count.min(buffer.len());
            output.write_all(&buffer[..chunk])?;
            count -= chunk;
        }
        Ok(())
    }

    for instruction in instructions {
        match instruction {
            BfInstruction::Move(amount) if *amount >= 0 => {
                repeated(output, b'>', amount.unsigned_abs())?;
            }
            BfInstruction::Move(amount) => repeated(output, b'<', amount.unsigned_abs())?,
            BfInstruction::Add(value) if *value <= 128 => {
                repeated(output, b'+', usize::from(*value))?;
            }
            BfInstruction::Add(value) => {
                repeated(output, b'-', usize::from(256_u16 - u16::from(*value)))?;
            }
            BfInstruction::Input => output.write_all(b",")?,
            BfInstruction::Output => output.write_all(b".")?,
            BfInstruction::Loop(body) => {
                output.write_all(b"[")?;
                write_instructions_streaming(body, output)?;
                output.write_all(b"]")?;
            }
        }
    }
    Ok(())
}

fn write_instructions(instructions: &[BfInstruction], output: &mut String) {
    for instruction in instructions {
        match instruction {
            BfInstruction::Move(amount) if *amount >= 0 => {
                output.extend(std::iter::repeat_n('>', amount.unsigned_abs()));
            }
            BfInstruction::Move(amount) => {
                output.extend(std::iter::repeat_n('<', amount.unsigned_abs()));
            }
            BfInstruction::Add(value) if *value <= 128 => {
                output.extend(std::iter::repeat_n('+', usize::from(*value)));
            }
            BfInstruction::Add(value) => {
                let count = usize::from(256_u16 - u16::from(*value));
                output.extend(std::iter::repeat_n('-', count));
            }
            BfInstruction::Input => output.push(','),
            BfInstruction::Output => output.push('.'),
            BfInstruction::Loop(body) => {
                output.push('[');
                write_instructions(body, output);
                output.push(']');
            }
        }
    }
}

fn write_annotated(
    instructions: &[AnnotatedBfInstruction],
    output: &mut String,
    ordinal: &mut u64,
    ranges: &mut Vec<(u64, u64, ProfileSiteId)>,
) {
    fn emit(
        character: char,
        site: ProfileSiteId,
        output: &mut String,
        ordinal: &mut u64,
        ranges: &mut Vec<(u64, u64, ProfileSiteId)>,
    ) {
        output.push(character);
        if let Some((_, end, previous)) = ranges.last_mut()
            && *previous == site
            && *end == *ordinal
        {
            *end += 1;
        } else {
            ranges.push((*ordinal, *ordinal + 1, site));
        }
        *ordinal += 1;
    }

    for instruction in instructions {
        match &instruction.operation {
            AnnotatedBfOperation::Move(amount) if *amount >= 0 => {
                for _ in 0..amount.unsigned_abs() {
                    emit('>', instruction.site, output, ordinal, ranges);
                }
            }
            AnnotatedBfOperation::Move(amount) => {
                for _ in 0..amount.unsigned_abs() {
                    emit('<', instruction.site, output, ordinal, ranges);
                }
            }
            AnnotatedBfOperation::Add(value) if *value <= 128 => {
                for _ in 0..usize::from(*value) {
                    emit('+', instruction.site, output, ordinal, ranges);
                }
            }
            AnnotatedBfOperation::Add(value) => {
                for _ in 0..usize::from(256_u16 - u16::from(*value)) {
                    emit('-', instruction.site, output, ordinal, ranges);
                }
            }
            AnnotatedBfOperation::Input => emit(',', instruction.site, output, ordinal, ranges),
            AnnotatedBfOperation::Output => emit('.', instruction.site, output, ordinal, ranges),
            AnnotatedBfOperation::Loop(body) => {
                emit('[', instruction.site, output, ordinal, ranges);
                write_annotated(body, output, ordinal, ranges);
                emit(']', instruction.site, output, ordinal, ranges);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_nested_ir_without_merging_nodes() {
        let program = BfProgram::new(vec![
            BfInstruction::Move(2),
            BfInstruction::Move(-1),
            BfInstruction::Add(2),
            BfInstruction::Add(255),
            BfInstruction::Input,
            BfInstruction::Output,
            BfInstruction::Loop(vec![BfInstruction::Add(255)]),
        ]);

        assert_eq!(program.to_source(), ">><++-,.[-]");
        assert_eq!(program.source_len(), 11);
        let mut streamed = Vec::new();
        program.write_source(&mut streamed).unwrap();
        assert_eq!(streamed, program.to_source().into_bytes());
    }

    #[test]
    fn annotated_serialization_covers_each_ordinal_once() {
        let root = ProfileSiteId(0);
        let child = ProfileSiteId(1);
        let program = AnnotatedBfProgram::new(
            vec![AnnotatedBfInstruction::new(
                root,
                AnnotatedBfOperation::Loop(vec![AnnotatedBfInstruction::new(
                    child,
                    AnnotatedBfOperation::Add(2),
                )]),
            )],
            ProfileSiteTable::new(vec![None, Some(root)]),
        );
        let (source, ranges) = program.to_source_and_ranges();
        assert_eq!(source, "[++]");
        assert_eq!(ranges, vec![(0, 1, root), (1, 3, child), (3, 4, root)]);
        program
            .profile_map()
            .validate_for_source(source.as_bytes())
            .unwrap();
    }
}
