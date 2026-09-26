//! BFCDBG2: small, BF-comment-safe records emitted directly by the selfhost backend.
//!
//! Header: @BFCDBG2; (@Fhhhh:hex_utf8_name;)* @ENDDBG;
//! Body: @Chhhh; selects a nonzero 16-bit continuation ID. @P0; restores its
//! body, @P1; selects shared dispatch (and clears the continuation), and
//! @P2;/@P3;/@P4; select compare/call/return children of the current continuation.
//! Function entries partition continuation IDs, not physical emission order.

use super::*;

pub(super) const HEADER: &[u8] = b"@BFCDBG2;";

fn hex_id(bytes: &[u8]) -> Result<u16, EmbeddedProfileError> {
    if bytes.len() != 4 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(EmbeddedProfileError::MalformedMarker);
    }
    let id = u16::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16)
        .map_err(|_| EmbeddedProfileError::MalformedMarker)?;
    if id == 0 {
        return Err(EmbeddedProfileError::MalformedMarker);
    }
    Ok(id)
}

fn hex_name(bytes: &[u8]) -> Result<String, EmbeddedProfileError> {
    if bytes.is_empty()
        || !bytes.len().is_multiple_of(2)
        || !bytes.iter().all(u8::is_ascii_hexdigit)
    {
        return Err(EmbeddedProfileError::MalformedMarker);
    }
    let decoded = bytes
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    String::from_utf8(decoded).map_err(|_| EmbeddedProfileError::MalformedMarker)
}

fn site(id: u32, parent: Option<u32>, kind: &str, key: &str, label: &str) -> ProfileSite {
    ProfileSite {
        id: ProfileSiteId(id),
        parent: parent.map(ProfileSiteId),
        kind: kind.into(),
        stable_key: key.into(),
        label: label.into(),
        source: None,
        attributes: BTreeMap::new(),
    }
}

pub(super) fn decode(source: &[u8], header: usize) -> Result<ProfileMap, EmbeddedProfileError> {
    let prefix = &source[..header];
    let prefix = prefix
        .strip_prefix(rle::HEX_HEADER.as_bytes())
        .or_else(|| prefix.strip_prefix(rle::HEADER.as_bytes()))
        .unwrap_or(prefix);
    if !prefix.iter().all(u8::is_ascii_whitespace) {
        return Err(EmbeddedProfileError::MalformedMarker);
    }
    // Validate RLE counts and cumulative overflow without visiting expanded commands.
    let bf = try_encoded_identity(source).map_err(|_| EmbeddedProfileError::MalformedMarker)?;
    let mut cursor = header + HEADER.len();
    let mut names = BTreeMap::new();
    while !source[cursor..].starts_with(b"@ENDDBG;") {
        if !source[cursor..].starts_with(b"@F") {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        let end = source[cursor..]
            .iter()
            .position(|b| *b == b';')
            .map(|n| cursor + n)
            .ok_or(EmbeddedProfileError::MalformedMarker)?;
        let record = &source[cursor + 2..end];
        if record.get(4) != Some(&b':') {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        let entry = hex_id(&record[..4])?;
        let name = hex_name(&record[5..])?;
        if names.insert(entry, name).is_some() {
            return Err(EmbeddedProfileError::MalformedMarker);
        }
        cursor = end + 1;
    }
    cursor += b"@ENDDBG;".len();
    let mut sites = vec![
        site(0, None, "artifact", "artifact.root", "selfhost BF"),
        site(1, Some(0), "abi", "abi.dispatch.countdown", "dispatcher"),
    ];
    let mut functions = BTreeMap::new();
    for (index, (entry, name)) in names.iter().enumerate() {
        let id = sites.len() as u32;
        let key = format!("function.{index}");
        let mut function = site(id, Some(0), "function", &key, name);
        function
            .attributes
            .insert("entry_continuation".into(), entry.to_string());
        function.attributes.insert("name".into(), name.clone());
        sites.push(function);
        functions.insert(*entry, (id, key, name));
    }
    let mut continuations = HashMap::new();
    let mut categories = HashMap::new();
    let mut context = None;
    let mut active = ProfileSiteId(0);
    let mut ordinal = 0_u64;
    let mut ranges: Vec<ProfileRange> = Vec::new();
    while cursor < source.len() {
        if source[cursor] == b'@' {
            let end = source[cursor..]
                .iter()
                .position(|b| *b == b';')
                .map(|n| cursor + n)
                .ok_or(EmbeddedProfileError::MalformedMarker)?;
            let record = &source[cursor + 1..end];
            if let Some(id) = record.strip_prefix(b"C") {
                let continuation = hex_id(id)?;
                let id = if let Some(id) = continuations.get(&continuation) {
                    *id
                } else {
                    let function = functions.range(..=continuation).next_back().map(|(_, f)| f);
                    if !functions.is_empty() && function.is_none() {
                        return Err(EmbeddedProfileError::MalformedMarker);
                    }
                    let (parent, key, label) = if let Some((id, key, name)) = function {
                        (
                            *id,
                            format!("{key}.continuation.{continuation}"),
                            format!("{name}: continuation {continuation}"),
                        )
                    } else {
                        (
                            0,
                            format!("continuation.{continuation}"),
                            format!("continuation {continuation}"),
                        )
                    };
                    let id = sites.len() as u32;
                    let mut metadata = site(id, Some(parent), "continuation", &key, &label);
                    metadata
                        .attributes
                        .insert("continuation_id".into(), continuation.to_string());
                    sites.push(metadata);
                    continuations.insert(continuation, id);
                    id
                };
                context = Some(id);
                active = ProfileSiteId(id);
            } else if record == b"P0" {
                active = ProfileSiteId(context.unwrap_or(0));
            } else if record == b"P1" {
                context = None;
                active = ProfileSiteId(1);
            } else {
                let (category, key) = match record {
                    b"P2" => (2, "abi.frame.compare"),
                    b"P3" => (3, "abi.call"),
                    b"P4" => (4, "abi.return"),
                    _ => return Err(EmbeddedProfileError::MalformedMarker),
                };
                let parent = context.ok_or(EmbeddedProfileError::MalformedMarker)?;
                active =
                    ProfileSiteId(*categories.entry((parent, category)).or_insert_with(|| {
                        let id = sites.len() as u32;
                        sites.push(site(id, Some(parent), "abi", key, key));
                        id
                    }));
            }
            cursor = end + 1;
        } else if is_bf_instruction(source[cursor]) {
            let run = rle::run_at(source, cursor, rle::compressed(source))
                .map_err(|_| EmbeddedProfileError::MalformedMarker)?;
            let next = ordinal + run.count as u64;
            if let Some(last) = ranges.last_mut()
                && last.site == active
            {
                last.end = next;
            } else {
                ranges.push(ProfileRange {
                    start: ordinal,
                    end: next,
                    site: active,
                });
            }
            ordinal = next;
            cursor = run.end;
        } else {
            cursor += 1;
        }
    }
    let map = ProfileMap {
        format: PROFILE_MAP_FORMAT.into(),
        version: ENCODED_PROFILE_MAP_VERSION,
        bf,
        files: Vec::new(),
        sites,
        ranges,
    };
    map.validate_structure()
        .map_err(EmbeddedProfileError::Map)?;
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_emission_restores_context_and_resolves_function_entries() {
        let source = b"@BFCRLE2;@BFCDBG2;@F0001:666f6f;@F0003:6d61696e;@ENDDBG;\
            +@P1;>@C0003;+2@P2;[->+<]@P0;.@P1;<@C0002;-@P4;[-]@P1;>";
        let map = embedded_profile_map(source).unwrap().unwrap();
        assert_eq!(map.version, 2);
        assert_eq!(map.bf.hash_kind, BfHashKind::EncodedSource);
        map.validate_for_source(source).unwrap();
        let main = map.sites.iter().find(|s| s.label == "main").unwrap();
        let cont = map
            .sites
            .iter()
            .find(|s| s.stable_key == "function.1.continuation.3")
            .unwrap();
        assert_eq!(cont.parent, Some(main.id));
        let compare = map
            .sites
            .iter()
            .find(|s| s.stable_key == "abi.frame.compare")
            .unwrap();
        assert_eq!(compare.parent, Some(cont.id));
        assert_eq!(map.ranges.iter().filter(|r| r.site == cont.id).count(), 2);
        assert!(
            map.sites
                .iter()
                .any(|s| s.stable_key == "function.0.continuation.2")
        );
        let json = map.to_json_pretty().unwrap();
        assert_eq!(ProfileMap::from_json(&json).unwrap(), map);
        let mut changed = source.to_vec();
        changed.push(b' ');
        assert!(map.validate_for_source(&changed).is_err());
        assert!(embed_profile_markers(std::str::from_utf8(source).unwrap(), &map).is_err());
    }

    #[test]
    fn huge_runs_do_not_require_expanded_hashing() {
        let source = b"@BFCRLE2;@BFCDBG2;@ENDDBG;@Cffff;+10000000000";
        let map = embedded_profile_map(source).unwrap().unwrap();
        assert_eq!(map.bf.instruction_count, 1 << 40);
        map.validate_for_source(source).unwrap();
    }

    #[test]
    fn accepts_plain_and_decimal_rle_and_optional_functions() {
        for source in [
            b"@BFCDBG2;@ENDDBG;@C0001;++.".as_slice(),
            b"@BFCRLE1;@BFCDBG2;@ENDDBG;@C0001;+2.",
        ] {
            let map = embedded_profile_map(source).unwrap().unwrap();
            assert_eq!(map.bf.instruction_count, 3);
            assert_eq!(map.sites[2].stable_key, "continuation.1");
        }
        assert!(
            embedded_profile_map(b"@BFCDBG2;@ENDDBG;")
                .unwrap()
                .unwrap()
                .ranges
                .is_empty()
        );
        assert!(embedded_profile_map(b"@C0001;+").unwrap().is_none());
    }

    #[test]
    fn rejects_malformed_compact_records_and_counts() {
        for source in [
            "@BFCDBG2;",
            "@BFCDBG2;@F0001:ff;@ENDDBG;",
            "@BFCDBG2;@F0001:6;@ENDDBG;",
            "@BFCDBG2;@F0000:61;@ENDDBG;",
            "@BFCDBG2;@F0001:61;@F0001:62;@ENDDBG;",
            "@BFCDBG2;@F0002:61;@ENDDBG;@C0001;+",
            "@BFCDBG2;@ENDDBG;@C0000;+",
            "@BFCDBG2;@ENDDBG;@C001;+",
            "@BFCDBG2;@ENDDBG;@Czzzz;+",
            "@BFCDBG2;@ENDDBG;@C0001",
            "@BFCDBG2;@ENDDBG;@P2;+",
            "@BFCDBG2;@ENDDBG;@C0001;@P1;@P2;+",
            "@BFCDBG2;@ENDDBG;@P9;+",
            "@BFCDBG2;@ENDDBG;@BFCDBG1;",
            "+@BFCDBG2;@ENDDBG;",
            "@BFCDBG1;@BFCDBG2;@ENDDBG;",
            "@BFCRLE2;@BFCDBG2;@ENDDBG;+0",
            "@BFCRLE2;@BFCDBG2;@ENDDBG;+ffffffffffffffff",
        ] {
            assert!(embedded_profile_map(source.as_bytes()).is_err(), "{source}");
        }
    }
}
