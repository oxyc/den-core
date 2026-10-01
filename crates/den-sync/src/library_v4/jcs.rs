//! RFC 8785 (JCS) canonical JSON. Library v4 compresses, compares and breaks ties on these bytes (§4, §6).

use serde_json::Value;
use std::cmp::Ordering;

/// The JCS bytes of `value`.
pub fn jcs(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

/// Byte order of two values' JCS, the tie-break every v4 merge rule ends with.
pub fn cmp(a: &Value, b: &Value) -> Ordering {
    jcs(a).cmp(&jcs(b))
}

/// Equality as v4 defines it: equal JCS bytes (so `1.0` and `1` are the same number).
pub fn same(a: &Value, b: &Value) -> bool {
    jcs(a) == jcs(b)
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => out.extend_from_slice(number_text(number).as_bytes()),
        Value::String(text) => string(text, out),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // §3.2.3: members sorted by their names' UTF-16 code units, not by UTF-8 bytes.
            let mut members: Vec<_> = map.iter().collect();
            members.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push(b'{');
            for (index, (key, item)) in members.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                string(key, out);
                out.push(b':');
                write(item, out);
            }
            out.push(b'}');
        }
    }
}

fn string(text: &str, out: &mut Vec<u8>) {
    // serde_json escapes exactly what JCS escapes: `"`, `\`, and U+0000–U+001F (as \b \t \n \f \r or a
    // lowercase \u00xx), and nothing else.
    out.extend_from_slice(
        serde_json::to_string(text)
            .expect("a string always serializes")
            .as_bytes(),
    );
}

/// A JSON number as ECMAScript's Number.prototype.toString prints the IEEE double it denotes (JCS §3.2.2.3).
fn number_text(number: &serde_json::Number) -> String {
    if let Some(n) = number.as_i64() {
        if n.unsigned_abs() <= 1 << 53 {
            return n.to_string();
        }
    }
    if let Some(n) = number.as_u64() {
        if n <= 1 << 53 {
            return n.to_string();
        }
    }
    es_number(number.as_f64().unwrap_or(f64::NAN))
}

pub fn es_number(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if !value.is_finite() {
        // JSON cannot carry these; serde_json never produces them from text.
        return "null".into();
    }
    // Rust's LowerExp prints the shortest digits that round-trip, as ECMAScript requires.
    let text = format!("{:e}", value.abs());
    let (mantissa, exponent) = text.split_once('e').expect("LowerExp has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent.parse::<i32>().expect("integer exponent") + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let rest = if k > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{}{rest}e{sign}{}", &digits[..1], (n - 1).abs())
    };
    if value < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_follow_ecmascript() {
        for (value, text) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (-0.0, "0"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (123.456, "123.456"),
            (1.5e300, "1.5e+300"),
            (-2.5e-10, "-2.5e-10"),
            (9007199254740993.0, "9007199254740992"),
        ] {
            assert_eq!(es_number(value), text, "{value}");
        }
    }

    #[test]
    fn keys_sort_by_utf16_and_strings_escape_controls_only() {
        let value = json!({"\u{e000}": 1, "\u{1f600}": 2, "b": [1.0, "\u{7f}\n\u{1}"], "a": null});
        assert_eq!(
            String::from_utf8(jcs(&value)).unwrap(),
            "{\"a\":null,\"b\":[1,\"\u{7f}\\n\\u0001\"],\"\u{1f600}\":2,\"\u{e000}\":1}"
        );
    }
}
