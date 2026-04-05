use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn frozen_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis()
}

pub(crate) fn stable_hash64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;

    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
}

pub(crate) fn encode_json_string_array(values: &[String]) -> String {
    let mut json = String::from("[");

    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&encode_json_string(value));
    }

    json.push(']');
    json
}

pub(crate) fn encode_json_string_map(values: &BTreeMap<String, String>) -> String {
    let mut json = String::from("{");

    for (index, (key, value)) in values.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&encode_json_string(key));
        json.push(':');
        json.push_str(&encode_json_string(value));
    }

    json.push('}');
    json
}

pub(crate) fn encode_json_string(value: &str) -> String {
    let mut json = String::with_capacity(value.len() + 2);
    json.push('"');

    for ch in value.chars() {
        match ch {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            '\u{08}' => json.push_str("\\b"),
            '\u{0C}' => json.push_str("\\f"),
            ch if ch.is_control() || u32::from(ch) > 0xFFFF => {
                push_utf16_escape(&mut json, ch);
            }
            ch => json.push(ch),
        }
    }

    json.push('"');
    json
}

fn push_utf16_escape(json: &mut String, ch: char) {
    let mut units = [0_u16; 2];
    for unit in ch.encode_utf16(&mut units).iter() {
        let _ = write!(json, "\\u{:04x}", unit);
    }
}

#[cfg(test)]
mod tests {
    use super::encode_json_string;

    #[test]
    fn encode_json_string_escapes_non_bmp_as_surrogate_pairs() {
        assert_eq!(encode_json_string("emoji: 😀"), r#""emoji: \ud83d\ude00""#);
    }
}
