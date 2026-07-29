use agentos_client::SessionStreamEntry;
use serde_json::json;

#[test]
fn session_events_use_the_flat_public_json_shape() {
    let cases = [
        json!({
            "durability": "durable",
            "sessionId": "main",
            "sequence": 1,
            "timestamp": "2026-07-18T00:00:00.000Z",
            "type": "agent_message_chunk",
            "content": { "type": "text", "text": "done" }
        }),
        json!({
            "durability": "ephemeral",
            "sessionId": "main",
            "afterSequence": 1,
            "type": "agent_thought_chunk",
            "content": { "type": "text", "text": "thinking" }
        }),
        json!({
            "durability": "durable",
            "sessionId": "main",
            "sequence": 2,
            "timestamp": "2026-07-18T00:00:01.000Z",
            "type": "permission_request",
            "requestId": "permission-1",
            "options": [{
                "optionId": "allow",
                "name": "Allow",
                "kind": "allow_once"
            }],
            "toolCall": { "toolCallId": "tool-1" }
        }),
        json!({
            "durability": "durable",
            "sessionId": "main",
            "sequence": 3,
            "timestamp": "2026-07-18T00:00:02.000Z",
            "type": "permission_response",
            "requestId": "permission-1",
            "outcome": { "outcome": "selected", "optionId": "allow" },
            "status": "accepted"
        }),
        json!({
            "durability": "durable",
            "sessionId": "main",
            "sequence": 4,
            "timestamp": "2026-07-18T00:00:03.000Z",
            "type": "tool_call",
            "toolCallId": "tool-media-1",
            "title": "Read media",
            "status": "completed",
            "content": [
                {
                    "type": "content",
                    "content": {
                        "type": "image",
                        "data": "iVBORw0KGgo=",
                        "mimeType": "image/png"
                    }
                },
                {
                    "type": "content",
                    "content": {
                        "type": "audio",
                        "data": "UklGRg==",
                        "mimeType": "audio/wav"
                    }
                },
                {
                    "type": "content",
                    "content": {
                        "type": "resource",
                        "resource": {
                            "uri": "file:///workspace/context.txt",
                            "mimeType": "text/plain",
                            "text": "context"
                        }
                    }
                }
            ]
        }),
    ];

    for expected in cases {
        let event: SessionStreamEntry =
            serde_json::from_value(expected.clone()).expect("deserialize flat session event");
        assert_eq!(
            serde_json::to_value(event).expect("serialize flat session event"),
            expected
        );
    }
}

#[test]
fn session_event_envelopes_reject_invalid_durability_combinations() {
    let ephemeral_permission = json!({
        "durability": "ephemeral",
        "sessionId": "main",
        "afterSequence": 0,
        "type": "permission_request",
        "requestId": "permission-1",
        "options": [],
        "toolCall": { "toolCallId": "tool-1" }
    });
    assert!(serde_json::from_value::<SessionStreamEntry>(ephemeral_permission).is_err());

    let ephemeral_with_timestamp = json!({
        "durability": "ephemeral",
        "sessionId": "main",
        "afterSequence": 0,
        "timestamp": "2026-07-18T00:00:00.000Z",
        "type": "agent_message_chunk",
        "content": { "type": "text", "text": "partial" }
    });
    let normalized = serde_json::to_value(
        serde_json::from_value::<SessionStreamEntry>(ephemeral_with_timestamp)
            .expect("deserialize ephemeral event"),
    )
    .expect("serialize ephemeral event");
    assert!(normalized.get("timestamp").is_none());
}
