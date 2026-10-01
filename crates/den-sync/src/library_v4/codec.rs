//! §4 Encoding: framing, DEFLATE, the bounds that make a row unreadable, and the size caps.

use super::jcs::jcs;
use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

/// Compressed framing byte (§4 *Plaintext*).
pub const COMPRESSED: u8 = 0x00;
/// Inflation stops here, and no writer sends a JCS above it (§4 *Bounds*, *Size*).
pub const MAX_JCS: usize = 8 * 1024 * 1024;
/// den-edge's value cap at minimum 4: base64url text of `nonce ‖ ciphertext ‖ tag` (§4 *Size*, §13).
pub const ROW_CAP: usize = 256 * 1024;
/// A §8 write is refused above this, so a merge of it by another den-core version still fits under `ROW_CAP`.
pub const WRITE_CAP: usize = 224 * 1024;
/// §4 *Bounds*: nesting deeper than this is unreadable. The document object itself is level 1.
pub const MAX_DEPTH: usize = 32;
const LEVEL: u8 = 9;
const SEAL_OVERHEAD: usize = 12 + 16;

/// Length of `v` (unpadded base64url of `nonce ‖ ciphertext ‖ tag`, v2 §2) for a plaintext of `plaintext` bytes.
pub fn sealed_len(plaintext: usize) -> usize {
    (4 * (plaintext + SEAL_OVERHEAD)).div_ceil(3)
}

/// `0x00` + raw DEFLATE (level 9) of the document's JCS.
pub fn compress(document: &serde_json::Value) -> Result<Vec<u8>, String> {
    let text = jcs(document);
    if text.len() > MAX_JCS {
        return Err("jcs_too_large".into());
    }
    let mut out = vec![COMPRESSED];
    out.extend(miniz_oxide::deflate::compress_to_vec(&text, LEVEL));
    Ok(out)
}

/// Raw inflate with the 8 MiB limit, streamed against it, refusing a stream that does not end at its final block
/// or that has bytes after it. This is `decompress_to_vec_with_limit`'s loop, kept here because that function
/// does not report how much input it consumed, which the trailing-byte rule needs.
pub fn inflate(input: &[u8]) -> Result<Vec<u8>, &'static str> {
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let mut out = vec![0u8; input.len().saturating_mul(2).clamp(64, MAX_JCS)];
    let mut state = Box::<DecompressorOxide>::default();
    let (mut consumed, mut written) = (0usize, 0usize);
    loop {
        let (status, read, wrote) =
            decompress(&mut state, &input[consumed..], &mut out, written, flags);
        consumed += read;
        written += wrote;
        match status {
            TINFLStatus::Done => {
                if consumed != input.len() {
                    return Err("trailing_bytes");
                }
                out.truncate(written);
                return Ok(out);
            }
            TINFLStatus::HasMoreOutput => {
                if out.len() >= MAX_JCS {
                    return Err("inflate_limit");
                }
                let grown = out.len().saturating_mul(2).min(MAX_JCS);
                out.resize(grown, 0);
            }
            _ => return Err("invalid_deflate"),
        }
    }
}

/// Unpadded base64url, the alphabet v2 §2 uses for `v`; the decoder also takes the standard alphabet and padding.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub fn unbase64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in text.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("invalid_base64".into()),
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bits >= 6 {
        return Err("invalid_base64".into());
    }
    Ok(out)
}

pub fn depth(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        serde_json::Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_tail_length() {
        for len in 0..10u8 {
            let bytes: Vec<u8> = (0..len).map(|i| i.wrapping_mul(97)).collect();
            assert_eq!(unbase64(&base64(&bytes)).unwrap(), bytes);
        }
        assert_eq!(base64(b"\xfb\xff"), "-_8");
    }

    #[test]
    fn sealed_length_matches_the_spec_arithmetic() {
        // §4: 256 KiB of `v` is 196,580 bytes of compressed plaintext.
        assert_eq!(sealed_len(196_580), ROW_CAP);
        assert!(sealed_len(196_581) > ROW_CAP);
    }

    #[test]
    fn inflate_refuses_trailing_bytes_and_truncation() {
        let compressed = miniz_oxide::deflate::compress_to_vec(b"{\"a\":1}", 9);
        assert_eq!(inflate(&compressed).unwrap(), b"{\"a\":1}");
        let mut trailing = compressed.clone();
        trailing.push(0);
        assert_eq!(inflate(&trailing), Err("trailing_bytes"));
        assert!(inflate(&compressed[..compressed.len() - 1]).is_err());
    }
}
