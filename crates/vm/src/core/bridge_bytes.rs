use base64::Engine;
use serde_json::{json, Value};

pub fn encoded_bytes_value(bytes: &[u8]) -> Value {
    json!({
        "__agentOSType": "bytes",
        "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub fn decode_encoded_bytes_value(value: &Value) -> Result<Vec<u8>, String> {
    let base64_value = if value.get("__agentOSType").and_then(Value::as_str) == Some("bytes") {
        value.get("base64").and_then(Value::as_str)
    } else if value.get("__type").and_then(Value::as_str) == Some("Buffer") {
        // The V8 bridge serializes Uint8Array/Buffer arguments as a CBOR byte
        // string. Its JSON compatibility projection is this exact tagged
        // shape; accepting it here keeps every syscall adapter on one strict
        // byte decoder without admitting arbitrary numeric arrays or objects.
        value.get("data").and_then(Value::as_str)
    } else {
        None
    }
    .ok_or_else(|| String::from("must be a string or encoded bytes payload"))?;

    decode_base64(base64_value)
}

pub fn bridge_buffer_value(bytes: &[u8]) -> Value {
    json!({
        "__type": "buffer",
        "value": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub fn decode_bridge_buffer_value(value: &Value) -> Result<Vec<u8>, String> {
    let base64_value = value
        .as_object()
        .filter(|object| object.get("__type").and_then(Value::as_str) == Some("buffer"))
        .and_then(|object| object.get("value"))
        .and_then(Value::as_str)
        .ok_or_else(|| String::from("must be a serialized bridge buffer"))?;

    decode_base64(base64_value)
}

pub fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| format!("contains invalid base64: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_encoded_bytes_payload() {
        let value = encoded_bytes_value(b"hello");

        assert_eq!(decode_encoded_bytes_value(&value), Ok(b"hello".to_vec()));
    }

    #[test]
    fn decodes_the_canonical_v8_cbor_byte_projection() {
        let value = json!({ "__type": "Buffer", "data": "aGVsbG8=" });

        assert_eq!(decode_encoded_bytes_value(&value), Ok(b"hello".to_vec()));
    }

    #[test]
    fn round_trips_bridge_buffer_payload() {
        let value = bridge_buffer_value(b"secret");

        assert_eq!(decode_bridge_buffer_value(&value), Ok(b"secret".to_vec()));
    }

    #[test]
    fn rejects_wrong_payload_shapes() {
        assert_eq!(
            decode_encoded_bytes_value(&json!({ "__agentOSType": "text", "base64": "aGk=" })),
            Err(String::from("must be a string or encoded bytes payload"))
        );
        assert_eq!(
            decode_encoded_bytes_value(&json!({ "__type": "Buffer", "value": "aGk=" })),
            Err(String::from("must be a string or encoded bytes payload"))
        );
        assert_eq!(
            decode_encoded_bytes_value(&json!([104, 105])),
            Err(String::from("must be a string or encoded bytes payload"))
        );
        assert_eq!(
            decode_bridge_buffer_value(&json!({ "__type": "bytes", "value": "aGk=" })),
            Err(String::from("must be a serialized bridge buffer"))
        );
    }

    #[test]
    fn labels_invalid_base64() {
        assert!(decode_base64("not base64!")
            .expect_err("invalid base64 should fail")
            .starts_with("contains invalid base64:"));
    }
}
