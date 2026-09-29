//! Brainfuck-shaped intermediate representation.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};

use bf_profiling::ProfileSiteId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSiteRecord {
    pub id: ProfileSiteId,
    pub parent: Option<ProfileSiteId>,
    pub kind: String,
    pub stable_key: String,
    pub label: String,
    pub source: Option<bf_profiling::ProfileSourceSpan>,
    pub attributes: BTreeMap<String, String>,
}

/// A profile site table owned by one generated artifact.
///
/// The wire-format representation lives in `bf-profiling`; this compact
/// compiler-side form is deliberately independent from the public BF IR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileSiteTable {
    sites: Vec<ProfileSiteRecord>,
    source_files: Vec<bf_profiling::ProfileFile>,
    interned: HashMap<u64, ProfileSiteId>,
    interned_collisions: HashMap<u64, Vec<ProfileSiteId>>,
}

fn profile_site_key_hash(parent: Option<ProfileSiteId>, kind: &str, stable_key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    parent.hash(&mut hasher);
    kind.hash(&mut hasher);
    stable_key.hash(&mut hasher);
    hasher.finish()
}

fn profile_site_matches(
    sites: &[ProfileSiteRecord],
    site_id: ProfileSiteId,
    parent: Option<ProfileSiteId>,
    kind: &str,
    stable_key: &str,
) -> bool {
    sites.get(site_id.0 as usize).is_some_and(|site| {
        site.parent == parent && site.kind == kind && site.stable_key == stable_key
    })
}

impl ProfileSiteTable {
    pub fn with_root(label: impl Into<String>) -> Self {
        let mut interned = HashMap::new();
        interned.insert(
            profile_site_key_hash(None, "artifact", "artifact.root"),
            ProfileSiteId(0),
        );
        Self {
            sites: vec![ProfileSiteRecord {
                id: ProfileSiteId(0),
                parent: None,
                kind: "artifact".into(),
                stable_key: "artifact.root".into(),
                label: label.into(),
                source: None,
                attributes: BTreeMap::new(),
            }],
            source_files: Vec::new(),
            interned,
            interned_collisions: HashMap::new(),
        }
    }

    pub fn new(parents: Vec<Option<ProfileSiteId>>) -> Self {
        let sites: Vec<ProfileSiteRecord> = parents
            .into_iter()
            .enumerate()
            .map(|(id, parent)| ProfileSiteRecord {
                id: ProfileSiteId(id as u32),
                parent,
                kind: String::new(),
                stable_key: String::new(),
                label: String::new(),
                source: None,
                attributes: BTreeMap::new(),
            })
            .collect();
        let mut interned = HashMap::new();
        let mut interned_collisions = HashMap::new();
        for site in &sites {
            let hash = profile_site_key_hash(site.parent, &site.kind, &site.stable_key);
            if let Some(&existing) = interned.get(&hash) {
                interned_collisions
                    .entry(hash)
                    .or_insert_with(|| vec![existing])
                    .push(site.id);
            } else {
                interned.insert(hash, site.id);
            }
        }
        Self {
            sites,
            source_files: Vec::new(),
            interned,
            interned_collisions,
        }
    }

    pub fn parent(&self, site: ProfileSiteId) -> Option<ProfileSiteId> {
        self.sites.get(site.0 as usize).and_then(|site| site.parent)
    }

    pub fn records(&self) -> &[ProfileSiteRecord] {
        &self.sites
    }

    pub fn source_files(&self) -> &[bf_profiling::ProfileFile] {
        &self.source_files
    }

    pub fn set_source_files(&mut self, source_files: Vec<bf_profiling::ProfileFile>) {
        self.source_files = source_files;
    }

    pub fn set_source(
        &mut self,
        site: ProfileSiteId,
        source: Option<bf_profiling::ProfileSourceSpan>,
    ) {
        if let Some(record) = self.sites.get_mut(site.0 as usize)
            && record.source.is_none()
        {
            record.source = source;
        }
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
        let hash = profile_site_key_hash(parent, &kind, &stable_key);
        if let Some(&site) = self.interned.get(&hash) {
            if profile_site_matches(&self.sites, site, parent, &kind, &stable_key) {
                return site;
            }
            if let Some(collisions) = self.interned_collisions.get(&hash)
                && let Some(&site) = collisions.iter().find(|&&site| {
                    profile_site_matches(&self.sites, site, parent, &kind, &stable_key)
                })
            {
                return site;
            }
        }
        let id = ProfileSiteId(self.sites.len() as u32);
        self.sites.push(ProfileSiteRecord {
            id,
            parent,
            kind,
            stable_key,
            label: label.into(),
            source: None,
            attributes,
        });
        if let Some(first) = self.interned.get(&hash).copied() {
            // Keep the first entry in the compact index and retain later
            // entries only for the (extremely rare) hash-collision case.
            self.interned_collisions
                .entry(hash)
                .or_insert_with(|| vec![first])
                .push(id);
        } else {
            self.interned.insert(hash, id);
        }
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

    /// Build the versioned sidecar profile map for this program.
    pub fn profile_map(&self) -> bf_profiling::ProfileMap {
        self.profile_artifact(false).map
    }

    /// Serialize directly in the selected encoding; ranges always count expanded BF commands.
    pub fn profile_artifact(&self, compressed: bool) -> crate::CompiledProfileArtifact {
        let (source, ranges) = if compressed {
            let mut source = bf_profiling::rle::HEADER.to_owned();
            let mut ranges = Vec::new();
            write_annotated_compact(&self.instructions, &mut source, &mut 0, &mut ranges);
            (source, ranges)
        } else {
            self.to_source_and_ranges()
        };
        let map = bf_profiling::ProfileMap {
            format: bf_profiling::PROFILE_MAP_FORMAT.to_owned(),
            version: bf_profiling::PROFILE_MAP_VERSION,
            bf: bf_profiling::bf_identity(source.as_bytes()),
            files: self.sites.source_files().to_vec(),
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
                    source: site.source.clone(),
                    attributes: site.attributes.clone(),
                })
                .collect(),
            ranges: ranges
                .into_iter()
                .map(|(start, end, site)| bf_profiling::ProfileRange { start, end, site })
                .collect(),
        };
        crate::CompiledProfileArtifact { source, map }
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

    pub fn write_compressed_source(&self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(bf_profiling::rle::HEADER.as_bytes())?;
        write_compact(&self.instructions, output)
    }

    pub fn compressed_source_len(&self) -> u64 {
        struct Count(u64);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0 += bytes.len() as u64;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut count = Count(0);
        self.write_compressed_source(&mut count)
            .expect("counting cannot fail");
        count.0
    }
}

fn write_compact(instructions: &[BfInstruction], output: &mut impl Write) -> io::Result<()> {
    for instruction in instructions {
        match instruction {
            BfInstruction::Move(n) => bf_profiling::rle::write_run(
                output,
                if *n < 0 { b'<' } else { b'>' },
                n.unsigned_abs(),
            )?,
            BfInstruction::Add(n) => bf_profiling::rle::write_run(
                output,
                if *n <= 128 { b'+' } else { b'-' },
                if *n <= 128 {
                    *n as usize
                } else {
                    256 - *n as usize
                },
            )?,
            BfInstruction::Input => output.write_all(b",")?,
            BfInstruction::Output => output.write_all(b".")?,
            BfInstruction::Loop(body) => {
                output.write_all(b"[")?;
                write_compact(body, output)?;
                output.write_all(b"]")?;
            }
        }
    }
    Ok(())
}

fn write_annotated_compact(
    instructions: &[AnnotatedBfInstruction],
    output: &mut String,
    ordinal: &mut u64,
    ranges: &mut Vec<(u64, u64, ProfileSiteId)>,
) {
    fn emit(
        byte: char,
        count: usize,
        site: ProfileSiteId,
        output: &mut String,
        ordinal: &mut u64,
        ranges: &mut Vec<(u64, u64, ProfileSiteId)>,
    ) {
        if count == 0 {
            return;
        }
        output.push(byte);
        if count > 1 {
            output.push_str(&count.to_string());
        }
        if let Some((_, end, previous)) = ranges.last_mut()
            && *previous == site
        {
            *end += count as u64;
        } else {
            ranges.push((*ordinal, *ordinal + count as u64, site));
        }
        *ordinal += count as u64;
    }
    for instruction in instructions {
        let (byte, count) = match &instruction.operation {
            AnnotatedBfOperation::Move(n) => (if *n < 0 { '<' } else { '>' }, n.unsigned_abs()),
            AnnotatedBfOperation::Add(n) => (
                if *n <= 128 { '+' } else { '-' },
                if *n <= 128 {
                    *n as usize
                } else {
                    256 - *n as usize
                },
            ),
            AnnotatedBfOperation::Input => (',', 1),
            AnnotatedBfOperation::Output => ('.', 1),
            AnnotatedBfOperation::Loop(body) => {
                emit('[', 1, instruction.site, output, ordinal, ranges);
                write_annotated_compact(body, output, ordinal, ranges);
                (']', 1)
            }
        };
        emit(byte, count, instruction.site, output, ordinal, ranges);
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
    fn compact_serialization_preserves_every_add_value_and_long_moves() {
        for value in 0..=255 {
            let program = BfProgram::new(vec![
                BfInstruction::Move(12345),
                BfInstruction::Add(value),
                BfInstruction::Move(-12345),
            ]);
            let mut compact = Vec::new();
            program.write_compressed_source(&mut compact).unwrap();
            assert_eq!(compact.len() as u64, program.compressed_source_len());
            assert_eq!(
                bf_profiling::bf_identity(&compact),
                bf_profiling::bf_identity(program.to_source().as_bytes())
            );
            assert!(compact.len() < 40);
        }
        let program = BfProgram::new(vec![BfInstruction::Loop(vec![BfInstruction::Move(
            1_000_000_000,
        )])]);
        let mut compact = Vec::new();
        program.write_compressed_source(&mut compact).unwrap();
        assert_eq!(compact, b"@BFCRLE1;[>1000000000]");
    }

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
