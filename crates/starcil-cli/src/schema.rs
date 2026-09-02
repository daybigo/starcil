use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn method_groups() -> BTreeMap<String, Vec<&'static str>> {
    let mut groups = BTreeMap::<String, Vec<&'static str>>::new();
    for method in starcil_protocol::methods::ALL {
        let group = method.split_once('.').map(|(group, _)| group).unwrap_or("core");
        groups.entry(group.to_owned()).or_default().push(method);
    }
    groups
}

pub fn api_schema() -> Value {
    let error_codes = [
        "not_found",
        "ambiguous_target",
        "invalid_params",
        "invalid_state",
        "lease_conflict",
        "stream_conflict",
        "timeout",
        "agent_prompt_stalled",
        "protocol_mismatch",
        "frame_too_large",
        "unknown_method",
        "feature_disabled",
        "platform_unsupported",
        "plugin_disabled",
        "popup_not_open",
        "rate_limited",
        "internal",
    ];

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://starcil.dev/schema/socket-api-v1.json",
        "title": "Starcil socket API frame",
        "description": "One newline-delimited JSON frame sent to or received from the Starcil socket API.",
        "oneOf": [
            {"$ref": "#/$defs/request"},
            {"$ref": "#/$defs/successResponse"},
            {"$ref": "#/$defs/errorResponse"},
            {"$ref": "#/$defs/eventFrame"}
        ],
        "x-starcil-protocol": {
            "major": starcil_protocol::PROTOCOL_MAJOR,
            "minor": starcil_protocol::PROTOCOL_MINOR,
            "maxFrameBytes": starcil_protocol::MAX_FRAME_BYTES
        },
        "x-starcil-catalogs": {
            "methods": starcil_protocol::methods::ALL,
            "events": starcil_protocol::events::ALL
        },
        "$defs": {
            "method": {
                "type": "string",
                "enum": starcil_protocol::methods::ALL
            },
            "eventName": {
                "type": "string",
                "enum": starcil_protocol::events::ALL
            },
            "request": {
                "type": "object",
                "required": ["id", "method"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "method": {"$ref": "#/$defs/method"},
                    "params": {}
                },
                "additionalProperties": false
            },
            "successResponse": {
                "type": "object",
                "required": ["id", "result"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "result": {}
                },
                "additionalProperties": false
            },
            "apiError": {
                "type": "object",
                "required": ["code", "message"],
                "properties": {
                    "code": {"type": "string", "enum": error_codes},
                    "message": {"type": "string"},
                    "details": {}
                },
                "additionalProperties": false
            },
            "errorResponse": {
                "type": "object",
                "required": ["id", "error"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "error": {"$ref": "#/$defs/apiError"}
                },
                "additionalProperties": false
            },
            "eventFrame": {
                "type": "object",
                "required": ["event", "data"],
                "properties": {
                    "event": {"$ref": "#/$defs/eventName"},
                    "data": {},
                    "revision": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_draft_2020_12_and_freezes_both_catalogs() {
        let schema = api_schema();
        assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert_eq!(schema["$defs"]["method"]["enum"], json!(starcil_protocol::methods::ALL));
        assert_eq!(schema["$defs"]["eventName"]["enum"], json!(starcil_protocol::events::ALL));
        assert!(schema["oneOf"].as_array().is_some_and(|frames| frames.len() == 4));
    }
}
