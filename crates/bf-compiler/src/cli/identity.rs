//! Stable input and lowering-option identities for IR artifacts.

use serde_json::{Value, json};

pub(super) struct IrArtifactMetadata<'a> {
    pub(super) source_kind: &'a str,
    pub(super) artifact_identity: &'a str,
    pub(super) optimization_options: bf_compiler::ContinuationOptimizationOptions,
}

pub(super) const IR_ARTIFACT_ID_VERSION: &str = "bfc-ir-artifact-v6";

pub(super) fn lowering_options_json(
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> Value {
    json!({
        "inline_functions": optimization_options.inline_functions,
        "inline_branch_successors": optimization_options.inline_branch_successors,
        "structure_local_control_flow": optimization_options.structure_local_control_flow,
    })
}

pub(super) fn validate_cli_artifact_id(
    configured: &std::ffi::OsStr,
    actual: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let configured = configured
        .to_str()
        .ok_or("--ir-artifact-id must be UTF-8")?;
    if configured != actual {
        return Err(format!(
            "--ir-artifact-id {configured:?} does not match the actual artifact identity {actual:?}"
        )
        .into());
    }
    Ok(())
}

pub(super) fn source_artifact_identity(
    sources: &[(String, String, Vec<u8>)],
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(IR_ARTIFACT_ID_VERSION.as_bytes());
    append_identity_frame(&mut data, b"source");
    append_lowering_options(&mut data, optimization_options);
    data.extend_from_slice(&(sources.len() as u64).to_be_bytes());
    for (_, _, bytes) in sources {
        append_identity_frame(&mut data, bytes);
    }
    sha256_hex(&data)
}

pub(super) fn cir_artifact_identity(
    bytes: &[u8],
    optimization_options: bf_compiler::ContinuationOptimizationOptions,
) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(IR_ARTIFACT_ID_VERSION.as_bytes());
    append_identity_frame(&mut data, b"cir");
    append_lowering_options(&mut data, optimization_options);
    append_identity_frame(&mut data, bytes);
    sha256_hex(&data)
}

pub(super) fn append_lowering_options(
    data: &mut Vec<u8>,
    options: bf_compiler::ContinuationOptimizationOptions,
) {
    append_identity_frame(
        data,
        if options.inline_functions {
            b"inline_functions=true"
        } else {
            b"inline_functions=false"
        },
    );
    append_identity_frame(
        data,
        if options.inline_branch_successors {
            b"inline_branch_successors=true"
        } else {
            b"inline_branch_successors=false"
        },
    );
    append_identity_frame(
        data,
        if options.structure_local_control_flow {
            b"structure_local_control_flow=true"
        } else {
            b"structure_local_control_flow=false"
        },
    );
}

pub(super) fn append_identity_frame(data: &mut Vec<u8>, value: &[u8]) {
    data.extend_from_slice(&(value.len() as u64).to_be_bytes());
    data.extend_from_slice(value);
}

pub(super) fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut padded = input.to_vec();
    let bit_length = (padded.len() as u64) * 8;
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.as_chunks::<64>().0 {
        let mut words = [0u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut working = state;
        for index in 0..64 {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (state_word, working_word) in state.iter_mut().zip(working) {
            *state_word = state_word.wrapping_add(working_word);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{sha256_hex, source_artifact_identity};

    #[test]
    fn artifact_identity_hash_uses_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn source_identity_preserves_order_boundaries_and_options() {
        let first = vec![
            ("a.bfc".to_owned(), "ab".to_owned(), b"ab".to_vec()),
            ("b.bfc".to_owned(), "c".to_owned(), b"c".to_vec()),
        ];
        let second = vec![
            ("a.bfc".to_owned(), "a".to_owned(), b"a".to_vec()),
            ("b.bfc".to_owned(), "bc".to_owned(), b"bc".to_vec()),
        ];
        assert_ne!(
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions::default()
            ),
            source_artifact_identity(
                &second,
                bf_compiler::ContinuationOptimizationOptions::default()
            )
        );
        assert_ne!(
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions::default()
            ),
            source_artifact_identity(
                &first,
                bf_compiler::ContinuationOptimizationOptions {
                    inline_branch_successors: false,
                    ..Default::default()
                }
            )
        );
    }
}
