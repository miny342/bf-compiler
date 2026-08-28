//! Shared version 1 schema and validation for BFC Brainfuck profile maps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error as StdError;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The required marker in every version 1 profile-map artifact.
pub const PROFILE_MAP_FORMAT: &str = "bfc-bf-profile-map";

/// The profile-map schema version implemented by this crate.
pub const PROFILE_MAP_VERSION: u32 = 1;

/// An artifact-local identifier for a profile site. Site zero is the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileSiteId(pub u32);

/// A byte span in one source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSourceSpan {
    pub file_id: u32,
    pub start_byte: u64,
    pub end_byte: u64,
}

/// A source file referenced by profile sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileFile {
    pub id: u32,
    pub path: String,
}

/// A node in the profile-site hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSite {
    pub id: ProfileSiteId,
    pub parent: Option<ProfileSiteId>,
    pub kind: String,
    pub stable_key: String,
    pub label: String,
    pub source: Option<ProfileSourceSpan>,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

/// A half-open BF instruction-ordinal range assigned to one profile site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRange {
    pub start: u64,
    pub end: u64,
    pub site: ProfileSiteId,
}

/// A FNV-1a hash rendered in JSON as exactly 16 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fnv1a64(pub u64);

impl Serialize for Fnv1a64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("{:016x}", self.0))
    }
}

impl<'de> Deserialize<'de> for Fnv1a64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        if text.len() != 16
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(serde::de::Error::custom(
                "fnv1a64 must be 16 lowercase hexadecimal digits",
            ));
        }
        u64::from_str_radix(&text, 16)
            .map(Self)
            .map_err(|_| serde::de::Error::custom("invalid fnv1a64"))
    }
}

/// Identity of the command-only Brainfuck instruction stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BfIdentity {
    pub instruction_count: u64,
    pub fnv1a64: Fnv1a64,
}

/// Versioned sidecar profile metadata for a Brainfuck artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMap {
    pub format: String,
    pub version: u32,
    pub bf: BfIdentity,
    #[serde(default)]
    pub files: Vec<ProfileFile>,
    pub sites: Vec<ProfileSite>,
    pub ranges: Vec<ProfileRange>,
}

/// Errors while decoding or validating a profile map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileMapError {
    Json(String),
    UnsupportedFormat {
        found: String,
    },
    UnsupportedVersion {
        found: u32,
    },
    IdentityMismatch {
        expected: BfIdentity,
        actual: BfIdentity,
    },
    DuplicateFileId {
        id: u32,
    },
    DuplicateSiteId {
        id: ProfileSiteId,
    },
    MissingRootSite,
    RootHasParent {
        parent: ProfileSiteId,
    },
    NonRootWithoutParent {
        id: ProfileSiteId,
    },
    UnknownParent {
        site: ProfileSiteId,
        parent: ProfileSiteId,
    },
    ParentCycle {
        site: ProfileSiteId,
    },
    UnknownSourceFile {
        site: ProfileSiteId,
        file_id: u32,
    },
    InvalidSourceSpan {
        site: ProfileSiteId,
    },
    EmptyRange {
        index: usize,
        start: u64,
        end: u64,
    },
    RangeOutOfBounds {
        index: usize,
        end: u64,
        instruction_count: u64,
    },
    RangeGap {
        index: usize,
        expected_start: u64,
        actual_start: u64,
    },
    RangeOverlap {
        index: usize,
        previous_end: u64,
        start: u64,
    },
    UnknownRangeSite {
        index: usize,
        site: ProfileSiteId,
    },
    MissingRanges {
        instruction_count: u64,
    },
}

/// Errors specific to the optional embedded marker stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddedProfileError {
    MalformedMarker,
    InvalidBase32,
    InvalidSiteJson(String),
    DuplicateSite(ProfileSiteId),
    UnknownSite(ProfileSiteId),
    Map(ProfileMapError),
}

impl fmt::Display for EmbeddedProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedMarker => f.write_str("malformed embedded profile marker"),
            Self::InvalidBase32 => f.write_str("invalid embedded profile base32 payload"),
            Self::InvalidSiteJson(error) => {
                write!(f, "invalid embedded profile site JSON: {error}")
            }
            Self::DuplicateSite(id) => write!(f, "duplicate embedded profile site {}", id.0),
            Self::UnknownSite(id) => write!(f, "unknown embedded profile site {}", id.0),
            Self::Map(error) => error.fmt(f),
        }
    }
}

impl StdError for EmbeddedProfileError {}

impl fmt::Display for ProfileMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(f, "invalid profile-map JSON: {message}"),
            Self::UnsupportedFormat { found } => {
                write!(f, "unsupported profile-map format {found:?}")
            }
            Self::UnsupportedVersion { found } => {
                write!(f, "unsupported profile-map version {found}")
            }
            Self::IdentityMismatch { expected, actual } => write!(
                f,
                "profile-map BF identity mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::DuplicateFileId { id } => write!(f, "duplicate profile file ID {id}"),
            Self::DuplicateSiteId { id } => write!(f, "duplicate profile site ID {}", id.0),
            Self::MissingRootSite => write!(f, "profile map is missing root site ID 0"),
            Self::RootHasParent { parent } => {
                write!(f, "root site ID 0 must not have parent {}", parent.0)
            }
            Self::NonRootWithoutParent { id } => {
                write!(f, "non-root site {} must have a parent", id.0)
            }
            Self::UnknownParent { site, parent } => {
                write!(f, "site {} references unknown parent {}", site.0, parent.0)
            }
            Self::ParentCycle { site } => {
                write!(f, "profile-site parent cycle contains site {}", site.0)
            }
            Self::UnknownSourceFile { site, file_id } => write!(
                f,
                "site {} references unknown source file {file_id}",
                site.0
            ),
            Self::InvalidSourceSpan { site } => {
                write!(f, "site {} has an invalid source span", site.0)
            }
            Self::EmptyRange { index, start, end } => {
                write!(f, "range {index} is empty or reversed: [{start}, {end})")
            }
            Self::RangeOutOfBounds {
                index,
                end,
                instruction_count,
            } => write!(
                f,
                "range {index} ends at {end}, beyond instruction count {instruction_count}"
            ),
            Self::RangeGap {
                index,
                expected_start,
                actual_start,
            } => write!(
                f,
                "range {index} starts at {actual_start}, leaving a gap after {expected_start}"
            ),
            Self::RangeOverlap {
                index,
                previous_end,
                start,
            } => write!(
                f,
                "range {index} starts at {start}, overlapping previous end {previous_end}"
            ),
            Self::UnknownRangeSite { index, site } => {
                write!(f, "range {index} references unknown site {}", site.0)
            }
            Self::MissingRanges { instruction_count } => {
                write!(f, "no ranges cover {instruction_count} BF instructions")
            }
        }
    }
}

impl StdError for ProfileMapError {}

impl ProfileMap {
    /// Decodes a JSON map. Call [`Self::validate_for_source`] before use.
    pub fn from_json(json: &str) -> Result<Self, ProfileMapError> {
        serde_json::from_str(json).map_err(|error| ProfileMapError::Json(error.to_string()))
    }

    /// Encodes this map as human-readable JSON.
    pub fn to_json_pretty(&self) -> Result<String, ProfileMapError> {
        serde_json::to_string_pretty(self).map_err(|error| ProfileMapError::Json(error.to_string()))
    }

    /// Verifies schema invariants and that this map belongs to `source`.
    pub fn validate_for_source(&self, source: &[u8]) -> Result<(), ProfileMapError> {
        if self.format != PROFILE_MAP_FORMAT {
            return Err(ProfileMapError::UnsupportedFormat {
                found: self.format.clone(),
            });
        }
        if self.version != PROFILE_MAP_VERSION {
            return Err(ProfileMapError::UnsupportedVersion {
                found: self.version,
            });
        }
        let actual = bf_identity(source);
        if self.bf != actual {
            return Err(ProfileMapError::IdentityMismatch {
                expected: self.bf,
                actual,
            });
        }

        let mut files = HashSet::new();
        for file in &self.files {
            if !files.insert(file.id) {
                return Err(ProfileMapError::DuplicateFileId { id: file.id });
            }
        }

        let mut sites = HashMap::new();
        for site in &self.sites {
            if sites.insert(site.id, site).is_some() {
                return Err(ProfileMapError::DuplicateSiteId { id: site.id });
            }
            if let Some(span) = &site.source {
                if span.start_byte > span.end_byte {
                    return Err(ProfileMapError::InvalidSourceSpan { site: site.id });
                }
                if !files.contains(&span.file_id) {
                    return Err(ProfileMapError::UnknownSourceFile {
                        site: site.id,
                        file_id: span.file_id,
                    });
                }
            }
        }
        let root = ProfileSiteId(0);
        let Some(root_site) = sites.get(&root) else {
            return Err(ProfileMapError::MissingRootSite);
        };
        if let Some(parent) = root_site.parent {
            return Err(ProfileMapError::RootHasParent { parent });
        }
        for site in &self.sites {
            if site.id == root {
                continue;
            }
            let Some(parent) = site.parent else {
                return Err(ProfileMapError::NonRootWithoutParent { id: site.id });
            };
            if !sites.contains_key(&parent) {
                return Err(ProfileMapError::UnknownParent {
                    site: site.id,
                    parent,
                });
            }
        }
        for site in &self.sites {
            let mut seen = HashSet::new();
            let mut current = site.id;
            while let Some(parent) = sites[&current].parent {
                if !seen.insert(current) {
                    return Err(ProfileMapError::ParentCycle { site: current });
                }
                current = parent;
            }
            if !seen.insert(current) {
                return Err(ProfileMapError::ParentCycle { site: current });
            }
        }

        if self.bf.instruction_count != 0 && self.ranges.is_empty() {
            return Err(ProfileMapError::MissingRanges {
                instruction_count: self.bf.instruction_count,
            });
        }
        let mut expected_start = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            if range.start >= range.end {
                return Err(ProfileMapError::EmptyRange {
                    index,
                    start: range.start,
                    end: range.end,
                });
            }
            if !sites.contains_key(&range.site) {
                return Err(ProfileMapError::UnknownRangeSite {
                    index,
                    site: range.site,
                });
            }
            if range.end > self.bf.instruction_count {
                return Err(ProfileMapError::RangeOutOfBounds {
                    index,
                    end: range.end,
                    instruction_count: self.bf.instruction_count,
                });
            }
            if range.start < expected_start {
                return Err(ProfileMapError::RangeOverlap {
                    index,
                    previous_end: expected_start,
                    start: range.start,
                });
            }
            if range.start > expected_start {
                return Err(ProfileMapError::RangeGap {
                    index,
                    expected_start,
                    actual_start: range.start,
                });
            }
            expected_start = range.end;
        }
        if expected_start != self.bf.instruction_count {
            return Err(ProfileMapError::RangeGap {
                index: self.ranges.len(),
                expected_start,
                actual_start: self.bf.instruction_count,
            });
        }
        Ok(())
    }
}

/// Calculates the profile identity while ignoring all non-Brainfuck bytes.
pub fn bf_identity(source: &[u8]) -> BfIdentity {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut instruction_count = 0;
    let mut hash = OFFSET_BASIS;
    for byte in source
        .iter()
        .copied()
        .filter(|byte| is_bf_instruction(*byte))
    {
        instruction_count += 1;
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    BfIdentity {
        instruction_count,
        fnv1a64: Fnv1a64(hash),
    }
}

/// RFC 4648 base32 with padding. Its alphabet cannot contain BF commands.
pub fn base32_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in input {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits != 0 {
        output.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    while !output.len().is_multiple_of(8) {
        output.push('=');
    }
    output
}

pub fn base32_decode(input: &str) -> Result<Vec<u8>, EmbeddedProfileError> {
    if !input.len().is_multiple_of(8) {
        return Err(EmbeddedProfileError::InvalidBase32);
    }
    let padding = input.bytes().rev().take_while(|byte| *byte == b'=').count();
    let data_characters = input.len() - padding;
    let expected_padding = match data_characters % 8 {
        0 => 0,
        2 => 6,
        4 => 4,
        5 => 3,
        7 => 1,
        _ => return Err(EmbeddedProfileError::InvalidBase32),
    };
    if padding != expected_padding {
        return Err(EmbeddedProfileError::InvalidBase32);
    }
    let mut output = Vec::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut padded = false;
    for byte in input.bytes() {
        if byte == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return Err(EmbeddedProfileError::InvalidBase32);
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return Err(EmbeddedProfileError::InvalidBase32),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    if bits != 0 && buffer & ((1_u32 << bits) - 1) != 0 {
        return Err(EmbeddedProfileError::InvalidBase32);
    }
    Ok(output)
}

/// Emit a marker-bearing BF artifact. The underlying BF command stream is
/// unchanged, so ordinary interpreters treat markers as comments.
pub fn embed_profile_markers(
    source: &str,
    map: &ProfileMap,
) -> Result<String, EmbeddedProfileError> {
    map.validate_for_source(source.as_bytes())
        .map_err(EmbeddedProfileError::Map)?;
    let mut output = String::from("@BFCDBG1;");
    for site in &map.sites {
        let mut embedded_site = site.clone();
        embedded_site.source = None;
        let json = serde_json::to_vec(&embedded_site)
            .map_err(|error| EmbeddedProfileError::InvalidSiteJson(error.to_string()))?;
        output.push_str(&format!("@S{}:{};", site.id.0, base32_encode(&json)));
    }
    output.push_str("@ENDDBG;");
    let mut ordinal = 0_u64;
    let mut range_index = 0_usize;
    for byte in source.bytes() {
        if matches!(byte, b'>' | b'<' | b'+' | b'-' | b'.' | b',' | b'[' | b']') {
            while map.ranges[range_index].end <= ordinal {
                range_index += 1;
            }
            if ordinal == map.ranges[range_index].start {
                output.push_str(&format!("@P{};", map.ranges[range_index].site.0));
            }
            ordinal += 1;
        }
        output.push(byte as char);
    }
    Ok(output)
}

/// Decode embedded v1 markers. `Ok(None)` means no header: `@P` remains an
/// ordinary comment in that case.
pub fn embedded_profile_map(source: &[u8]) -> Result<Option<ProfileMap>, EmbeddedProfileError> {
    let Some(header) = source
        .windows(b"@BFCDBG1;".len())
        .position(|window| window == b"@BFCDBG1;")
    else {
        return Ok(None);
    };
    if source[..header].iter().copied().any(is_bf_instruction) {
        return Err(EmbeddedProfileError::MalformedMarker);
    }
    let mut cursor = header + b"@BFCDBG1;".len();
    let mut sites = Vec::new();
    while !source[cursor..].starts_with(b"@ENDDBG;") {
        if !source[cursor..].starts_with(b"@S") {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        cursor += 2;
        let id_start = cursor;
        while cursor < source.len() && source[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if id_start == cursor || source.get(cursor) != Some(&b':') {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        let declared = std::str::from_utf8(&source[id_start..cursor])
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(EmbeddedProfileError::MalformedMarker)?;
        cursor += 1;
        let payload_start = cursor;
        while cursor < source.len() && source[cursor] != b';' {
            cursor += 1;
        }
        if cursor == source.len() {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        let payload = std::str::from_utf8(&source[payload_start..cursor])
            .map_err(|_| EmbeddedProfileError::InvalidBase32)?;
        let decoded = base32_decode(payload)?;
        let site: ProfileSite = serde_json::from_slice(&decoded)
            .map_err(|error| EmbeddedProfileError::InvalidSiteJson(error.to_string()))?;
        if site.id.0 != declared {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        if sites
            .iter()
            .any(|existing: &ProfileSite| existing.id == site.id)
        {
            return Err(EmbeddedProfileError::DuplicateSite(site.id));
        }
        sites.push(site);
        cursor += 1;
    }
    cursor += b"@ENDDBG;".len();
    let known: HashSet<_> = sites.iter().map(|site| site.id).collect();
    let mut ranges: Vec<ProfileRange> = Vec::new();
    let mut active = ProfileSiteId(0);
    let mut ordinal = 0_u64;
    let mut position = 0_usize;
    while position < source.len() {
        if position >= cursor && source[position..].starts_with(b"@P") {
            let start = position + 2;
            let mut end = start;
            while end < source.len() && source[end].is_ascii_digit() {
                end += 1;
            }
            if start == end || source.get(end) != Some(&b';') {
                return Err(EmbeddedProfileError::MalformedMarker);
            }
            active = ProfileSiteId(
                std::str::from_utf8(&source[start..end])
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .ok_or(EmbeddedProfileError::MalformedMarker)?,
            );
            if !known.contains(&active) {
                return Err(EmbeddedProfileError::UnknownSite(active));
            }
            position = end + 1;
            continue;
        }
        if matches!(
            source[position],
            b'>' | b'<' | b'+' | b'-' | b'.' | b',' | b'[' | b']'
        ) {
            if let Some(range) = ranges.last_mut()
                && range.site == active
                && range.end == ordinal
            {
                range.end += 1;
            } else {
                ranges.push(ProfileRange {
                    start: ordinal,
                    end: ordinal + 1,
                    site: active,
                });
            }
            ordinal += 1;
        }
        position += 1;
    }
    let map = ProfileMap {
        format: PROFILE_MAP_FORMAT.into(),
        version: PROFILE_MAP_VERSION,
        bf: bf_identity(source),
        files: Vec::new(),
        sites,
        ranges,
    };
    map.validate_for_source(source)
        .map_err(EmbeddedProfileError::Map)?;
    Ok(Some(map))
}

/// Returns whether `byte` is one of the eight Brainfuck instructions.
pub const fn is_bf_instruction(byte: u8) -> bool {
    matches!(byte, b'>' | b'<' | b'+' | b'-' | b'.' | b',' | b'[' | b']')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_map(source: &[u8]) -> ProfileMap {
        ProfileMap {
            format: PROFILE_MAP_FORMAT.into(),
            version: PROFILE_MAP_VERSION,
            bf: bf_identity(source),
            files: vec![ProfileFile {
                id: 1,
                path: "test.bfc".into(),
            }],
            sites: vec![
                ProfileSite {
                    id: ProfileSiteId(0),
                    parent: None,
                    kind: "artifact".into(),
                    stable_key: "artifact.root".into(),
                    label: "root".into(),
                    source: None,
                    attributes: BTreeMap::new(),
                },
                ProfileSite {
                    id: ProfileSiteId(1),
                    parent: Some(ProfileSiteId(0)),
                    kind: "abi".into(),
                    stable_key: "abi.test".into(),
                    label: "test".into(),
                    source: Some(ProfileSourceSpan {
                        file_id: 1,
                        start_byte: 0,
                        end_byte: 1,
                    }),
                    attributes: BTreeMap::new(),
                },
            ],
            ranges: vec![ProfileRange {
                start: 0,
                end: bf_identity(source).instruction_count,
                site: ProfileSiteId(1),
            }],
        }
    }

    #[test]
    fn identity_ignores_comments_and_uses_fnv1a() {
        let plain = bf_identity(b"++[>+<-].");
        assert_eq!(plain, bf_identity(b"ignore ++ text [>+<-]. comment"));
        assert_eq!(plain.instruction_count, 9);
        assert_eq!(plain.fnv1a64, Fnv1a64(0x4d84_7ec2_9cb4_1d83));
    }

    #[test]
    fn json_uses_lowercase_sixteen_digit_hash() {
        let map = valid_map(b"+");
        let json = map.to_json_pretty().unwrap();
        assert!(json.contains("\"fnv1a64\": \"af63"));
        assert_eq!(ProfileMap::from_json(&json).unwrap(), map);
        assert!(ProfileMap::from_json(r#"{"fnv1a64":"AF63BD4C8601B7DF"}"#).is_err());
    }

    #[test]
    fn validates_a_complete_map() {
        let source = b"+ comment -";
        valid_map(source).validate_for_source(source).unwrap();
    }

    #[test]
    fn rejects_identity_and_schema_errors() {
        let source = b"+";
        let mut map = valid_map(source);
        map.format = "other".into();
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::UnsupportedFormat { .. })
        ));
        let mut map = valid_map(source);
        map.version = 2;
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::UnsupportedVersion { .. })
        ));
        let map = valid_map(b"++");
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn rejects_site_hierarchy_errors() {
        let source = b"+";
        let mut map = valid_map(source);
        map.sites.push(map.sites[1].clone());
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::DuplicateSiteId { .. })
        ));
        let mut map = valid_map(source);
        map.sites[1].parent = Some(ProfileSiteId(99));
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::UnknownParent { .. })
        ));
        let mut map = valid_map(source);
        map.sites[0].parent = Some(ProfileSiteId(1));
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::RootHasParent { .. })
        ));
        let mut map = valid_map(source);
        map.sites[0].id = ProfileSiteId(2);
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::MissingRootSite)
        ));
        let mut map = valid_map(source);
        map.sites[1].parent = Some(ProfileSiteId(1));
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::ParentCycle { .. })
        ));
    }

    #[test]
    fn rejects_invalid_ranges() {
        let source = b"++";
        let mut map = valid_map(source);
        map.ranges = vec![ProfileRange {
            start: 0,
            end: 1,
            site: ProfileSiteId(1),
        }];
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::RangeGap { .. })
        ));
        let mut map = valid_map(source);
        map.ranges = vec![
            ProfileRange {
                start: 0,
                end: 1,
                site: ProfileSiteId(1),
            },
            ProfileRange {
                start: 0,
                end: 2,
                site: ProfileSiteId(1),
            },
        ];
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::RangeOverlap { .. })
        ));
        let mut map = valid_map(source);
        map.ranges[0].end = 3;
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::RangeOutOfBounds { .. })
        ));
        let mut map = valid_map(source);
        map.ranges[0].site = ProfileSiteId(9);
        assert!(matches!(
            map.validate_for_source(source),
            Err(ProfileMapError::UnknownRangeSite { .. })
        ));
    }

    #[test]
    fn embedded_markers_round_trip_without_changing_bf_identity() {
        let source = "+>";
        let map = valid_map(source.as_bytes());
        let embedded = embed_profile_markers(source, &map).unwrap();
        assert_eq!(
            bf_identity(embedded.as_bytes()),
            bf_identity(source.as_bytes())
        );
        let metadata = embedded.split("@ENDDBG;").next().unwrap();
        assert!(
            !metadata
                .bytes()
                .any(|byte| matches!(byte, b'<' | b'>' | b'+' | b'-' | b'.' | b',' | b'[' | b']'))
        );
        let mut expected = map;
        expected.files.clear();
        for site in &mut expected.sites {
            site.source = None;
        }
        assert_eq!(
            embedded_profile_map(embedded.as_bytes()).unwrap(),
            Some(expected)
        );
        assert_eq!(embedded_profile_map(b"@P1;+").unwrap(), None);
    }

    #[test]
    fn embedded_markers_reject_duplicate_and_unknown_sites() {
        let source = "+";
        let map = valid_map(source.as_bytes());
        let embedded = embed_profile_markers(source, &map).unwrap();
        let record_start = embedded.find("@S0:").unwrap();
        let record_end = record_start + embedded[record_start..].find(';').unwrap() + 1;
        let duplicate_record = format!("{}@ENDDBG;", &embedded[record_start..record_end]);
        let duplicate = embedded.replacen("@ENDDBG;", &duplicate_record, 1);
        assert!(matches!(
            embedded_profile_map(duplicate.as_bytes()),
            Err(EmbeddedProfileError::DuplicateSite(ProfileSiteId(0)))
        ));
        let unknown = embedded.replace("@P1;", "@P99;");
        assert!(matches!(
            embedded_profile_map(unknown.as_bytes()),
            Err(EmbeddedProfileError::UnknownSite(ProfileSiteId(99)))
        ));
        assert!(matches!(
            embedded_profile_map(b"+@BFCDBG1;@ENDDBG;"),
            Err(EmbeddedProfileError::MalformedMarker)
        ));
        assert_eq!(base32_decode("MY======").unwrap(), b"f");
        assert!(matches!(
            base32_decode("MY"),
            Err(EmbeddedProfileError::InvalidBase32)
        ));
    }
}
