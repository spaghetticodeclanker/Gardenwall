use gardenwall_core::CapabilityRequest;
use serde::{Deserialize, Serialize};

const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestCodecError {
    TooLarge,
    InvalidEncoding,
    NonCanonical,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEnvelope {
    version: u8,
    capability: String,
    arguments: RequestArguments,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestArguments {
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

pub(super) fn encode_request(request: &CapabilityRequest) -> Result<Vec<u8>, RequestCodecError> {
    let (capability, arguments) = match request {
        CapabilityRequest::WebSearch { query } => (
            "WebSearch",
            RequestArguments {
                query: Some(query.clone()),
                url: None,
            },
        ),
        CapabilityRequest::WebpageRetrieval { url } => (
            "WebpageRetrieval",
            RequestArguments {
                query: None,
                url: Some(url.clone()),
            },
        ),
    };

    let envelope = RequestEnvelope {
        version: 1,
        capability: capability.to_owned(),
        arguments,
    };

    let bytes = serde_json::to_vec(&envelope).map_err(|_| RequestCodecError::InvalidEncoding)?;

    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(RequestCodecError::TooLarge);
    }

    Ok(bytes)
}

pub(super) fn decode_request(bytes: &[u8]) -> Result<CapabilityRequest, RequestCodecError> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(RequestCodecError::TooLarge);
    }

    let envelope: RequestEnvelope =
        serde_json::from_slice(bytes).map_err(|_| RequestCodecError::InvalidEncoding)?;

    if envelope.version != 1 {
        return Err(RequestCodecError::InvalidEncoding);
    }

    let request = match (
        envelope.capability.as_str(),
        envelope.arguments.query,
        envelope.arguments.url,
    ) {
        ("WebSearch", Some(query), None) => CapabilityRequest::WebSearch { query },
        ("WebpageRetrieval", None, Some(url)) => CapabilityRequest::WebpageRetrieval { url },
        _ => return Err(RequestCodecError::InvalidEncoding),
    };

    let canonical = encode_request(&request)?;

    if canonical != bytes {
        return Err(RequestCodecError::NonCanonical);
    }

    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_round_trip() {
        let original = CapabilityRequest::WebSearch {
            query: "Rust SQLite security".to_owned(),
        };

        let encoded = encode_request(&original).unwrap();
        let decoded = decode_request(&encoded).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn webpage_retrieval_round_trip() {
        let original = CapabilityRequest::WebpageRetrieval {
            url: "https://example.com".to_owned(),
        };

        let encoded = encode_request(&original).unwrap();
        let decoded = decode_request(&encoded).unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let bytes = br#"{
            "version": 2,
            "capability": "WebSearch",
            "arguments": {"query": "test"}
        }"#;

        assert!(decode_request(bytes).is_err());
    }

    #[test]
    fn unknown_capability_is_rejected() {
        let bytes = br#"{
            "version": 1,
            "capability": "ShellExecution",
            "arguments": {"query": "test"}
        }"#;

        assert!(decode_request(bytes).is_err());
    }

    #[test]
    fn mismatched_arguments_are_rejected() {
        let bytes = br#"{
            "version": 1,
            "capability": "WebSearch",
            "arguments": {"url": "https://example.com"}
        }"#;

        assert!(decode_request(bytes).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let bytes = br#"{
            "version": 1,
            "capability": "WebSearch",
            "arguments": {
                "query": "test",
                "unexpected": true
            }
        }"#;

        assert!(decode_request(bytes).is_err());
    }

    #[test]
    fn noncanonical_json_is_rejected() {
        let bytes = br#"{
            "version": 1,
            "capability": "WebSearch",
            "arguments": {"query": "test"}
        }"#;

        assert_eq!(decode_request(bytes), Err(RequestCodecError::NonCanonical));
    }

    #[test]
    fn oversized_request_is_rejected() {
        let request = CapabilityRequest::WebSearch {
            query: "x".repeat(20_000),
        };

        assert_eq!(encode_request(&request), Err(RequestCodecError::TooLarge));
    }

    #[test]
    fn duplicate_json_fields_are_rejected() {
        let bytes = br#"{
            "version": 1,
            "version": 1,
            "capability": "WebSearch",
            "arguments": {"query": "test"}
        }"#;

        assert!(decode_request(bytes).is_err());
    }
}
