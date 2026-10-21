//! Recovery code §3's memory-hard KDF in its own module, loaded only while making or redeeming a code.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_recovery_kdf(request: &str) -> String {
    den_sync::evaluate_recovery_kdf(request)
}

#[cfg(test)]
mod tests {
    #[test]
    fn accepts_only_the_memory_hard_recovery_operation() {
        let answer = super::evaluate_recovery_kdf(r#"{"op":"recovery_read","text":"x"}"#);
        assert_eq!(answer, r#"{"error":"invalid_request","version":1}"#);
    }
}
