//! The recovery code (den-spec `wire/recovery-code.md`): making a code from random bytes (§2), reading a typed one
//! (§2), and deriving its locator and wrap key (§3). Randomness arrives as input; sealing stays in the clients.

use argon2::{Algorithm, Argon2, Block, Params, Version};
use hkdf::Hkdf;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const DATA_LEN: usize = 22;
const CODE_LEN: usize = DATA_LEN + 2;
const DOMAIN: &[u8] = b"den/recovery/v1";
const CHECK_DOMAIN: &[u8] = b"den/recovery/v1/check";

/// §2: the two check characters, the first 10 bits of `SHA-256("den/recovery/v1/check" ‖ data)`.
fn check(data: &str) -> [u8; 2] {
    let h = Sha256::new()
        .chain_update(CHECK_DOMAIN)
        .chain_update(data.as_bytes())
        .finalize();
    [
        ALPHABET[(h[0] >> 3) as usize],
        ALPHABET[(((h[0] & 7) << 2) | (h[1] >> 6)) as usize],
    ]
}

/// §2 *Making one*: 22 random bytes (hex) → the code in six groups of four, and its data characters.
pub fn code(random: &str) -> Result<Value, String> {
    let bytes = unhex(random).ok_or("invalid_random")?;
    if bytes.len() != DATA_LEN {
        return Err("invalid_random".into());
    }
    let data: String = bytes
        .iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect();
    let mut full = data.clone().into_bytes();
    full.extend_from_slice(&check(&data));
    let groups: Vec<&str> = full
        .chunks(4)
        .map(|g| std::str::from_utf8(g).expect("alphabet is ASCII"))
        .collect();
    Ok(json!({ "code": groups.join("-"), "data": data }))
}

/// §2 *Reading a typed code*: whitespace (Unicode `White_Space`: a pasted code may carry a newline) and dashes
/// dropped, then ASCII uppercase, then 24 alphabet characters whose check characters match. Anything non-ASCII left is
/// `mistyped`, before any case mapping: `ſ` must not read as `S`. `checksum` for a check that does not match.
pub fn read(text: &str) -> Result<Value, String> {
    let kept: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if !kept.is_ascii() {
        return Err("mistyped".into());
    }
    let normalized = kept.to_ascii_uppercase();
    if normalized.len() != CODE_LEN || !normalized.bytes().all(|b| ALPHABET.contains(&b)) {
        return Err("mistyped".into());
    }
    let (data, given) = normalized.split_at(DATA_LEN);
    if given.as_bytes() != check(data) {
        return Err("checksum".into());
    }
    Ok(json!({ "data": data }))
}

/// §3: Argon2id over the data characters, then the HKDF-SHA256 locator (16 bytes) and wrap key (32 bytes).
///
/// `A`, Argon2's 64 MiB of blocks and the raw wrap key are wiped here before returning (§8 step 6). The wrap key still
/// leaves as hex in the JSON answer, an immutable string in the caller's runtime that nothing can wipe: a client that
/// zeroes its own copy has not zeroed every copy.
pub fn derive(data: &str) -> Result<Value, String> {
    let (locator, wrap_key) = derive_bytes(data)?;
    Ok(json!({ "locator": hex(&locator), "wrapKey": hex(&*wrap_key) }))
}

fn argon2id(data: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    if data.len() != DATA_LEN || !data.bytes().all(|b| ALPHABET.contains(&b)) {
        return Err("mistyped".into());
    }
    let params = Params::new(65536, 3, 1, Some(32)).map_err(|_| "kdf_failed")?;
    let mut a = Zeroizing::new([0u8; 32]);
    // The blocks are this crate's, so they are wiped: `hash_password_into` frees its own without wiping them.
    let mut blocks = Zeroizing::new(vec![Block::default(); params.block_count()]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(data.as_bytes(), DOMAIN, &mut *a, &mut **blocks)
        .map_err(|_| "kdf_failed")?;
    Ok(a)
}

fn derive_bytes(data: &str) -> Result<([u8; 16], Zeroizing<[u8; 32]>), String> {
    let a = argon2id(data)?;
    let hkdf = Hkdf::<Sha256>::new(Some(DOMAIN), &*a);
    drop(a);
    let mut locator = [0u8; 16];
    let mut wrap_key = Zeroizing::new([0u8; 32]);
    hkdf.expand(b"locator", &mut locator)
        .map_err(|_| "kdf_failed")?;
    hkdf.expand(b"wrap", &mut *wrap_key)
        .map_err(|_| "kdf_failed")?;
    Ok((locator, wrap_key))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Argon2id this crate links is RFC 9106's, checked against §5.3 as den-spec's vector tool checks Node's.
    #[test]
    fn argon2id_is_rfc_9106() {
        let params = argon2::ParamsBuilder::new()
            .m_cost(32)
            .t_cost(3)
            .p_cost(4)
            .output_len(32)
            .data(argon2::AssociatedData::new(&[4; 12]).unwrap())
            .build()
            .unwrap();
        let mut tag = [0u8; 32];
        let mut blocks = vec![Block::default(); params.block_count()];
        Argon2::new_with_secret(&[3; 8], Algorithm::Argon2id, Version::V0x13, params)
            .unwrap()
            .hash_password_into_with_memory(&[1; 32], &[2; 16], &mut tag, &mut blocks)
            .unwrap();
        assert_eq!(
            hex(&tag),
            "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659"
        );
    }

    /// `A` for den-spec's two vector codes: `derive` publishes only what HKDF makes of it.
    #[test]
    fn argon2id_is_the_vectors_a() {
        let cases = [
            (
                "GEB2LP9UC63WQ95UNSLTXM",
                "5ef8e8c93dbc6f726ff84f8cf44be105491f96fa0e927064bb94198040d1de91",
            ),
            (
                "8N8XL3WCQ7LXWY6YJMUXZJ",
                "18ceffc21f9a25c82926ea0672719862eaaf2748d68c17c9511dcc24bb89cc90",
            ),
        ];
        for (data, a) in cases {
            assert_eq!(hex(&*argon2id(data).unwrap()), a, "{data}");
        }
    }

    #[test]
    fn derive_refuses_anything_but_data_characters() {
        for data in [
            "",
            "GEB2LP9UC63WQ95UNSLTX",
            "GEB2LP9UC63WQ95UNSLTXMFL",
            "geb2lp9uc63wq95unsltxm",
        ] {
            assert_eq!(derive(data).unwrap_err(), "mistyped", "{data}");
        }
    }

    #[test]
    fn code_takes_exactly_22_bytes_of_hex() {
        for random in [
            "",
            "00",
            &"00".repeat(21),
            &"00".repeat(23),
            &format!("+f{}", "00".repeat(21)),
        ] {
            assert_eq!(code(random).unwrap_err(), "invalid_random", "{random}");
        }
    }

    /// §2: whitespace of any kind is dropped, but no other non-ASCII character is mapped into the alphabet.
    #[test]
    fn reading_uppercases_ascii_only() {
        let code = "GEB2-LP9U-C63W-Q95U-NSLT-XMFL";
        let nbsp = code.replace('-', "\u{a0}");
        assert_eq!(read(&nbsp).unwrap()["data"], "GEB2LP9UC63WQ95UNSLTXM");
        let long_s = code.replace("NSLT", "N\u{17f}LT");
        assert_eq!(read(&long_s).unwrap_err(), "mistyped");
        // `ﬆ` uppercases to two letters under Unicode rules, which would change the length.
        let ligature = code.replace("XMFL", "XMF\u{fb06}");
        assert_eq!(read(&ligature).unwrap_err(), "mistyped");
    }

    #[test]
    fn every_code_reads_back_as_its_data() {
        for seed in 0u8..64 {
            let random: Vec<u8> = (0..22u8)
                .map(|i| i.wrapping_mul(37).wrapping_add(seed.wrapping_mul(101)))
                .collect();
            let made = code(&hex(&random)).unwrap();
            assert_eq!(
                read(made["code"].as_str().unwrap()).unwrap()["data"],
                made["data"]
            );
        }
    }
}
