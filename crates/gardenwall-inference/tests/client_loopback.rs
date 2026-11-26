//! HTTP client behaviour against a mock runtime over loopback TCP.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use gardenwall_inference::client::{
    ChatMessage, ChatRequest, ClientError, InferenceClient, Role, StreamEvent,
};
use gardenwall_inference::launch::ApiKey;
use gardenwall_inference::transport::Connector;

const SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3,\"total_tokens\":10},\"timings\":{\"predicted_per_second\":99.5}}\n\n",
    "data: [DONE]\n\n",
);

fn authorized(headers: &HeaderMap, seen: &AtomicBool) -> bool {
    let ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer ") && v.len() == 7 + 64);
    if ok {
        seen.store(true, Ordering::SeqCst);
    }
    ok
}

async fn start(huge_props: bool) -> (InferenceClient, Arc<AtomicBool>) {
    let seen = Arc::new(AtomicBool::new(false));
    let (s1, s2, s3) = (seen.clone(), seen.clone(), seen.clone());
    let app = Router::new()
        .route(
            "/health",
            get(move |h: HeaderMap| async move {
                if authorized(&h, &s1) {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        )
        .route(
            "/props",
            get(move |h: HeaderMap| async move {
                if !authorized(&h, &s2) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                if huge_props {
                    return "x".repeat(9 * 1024 * 1024).into_response();
                }
                axum::Json(serde_json::json!({
                    "default_generation_settings": {"n_ctx": 8192},
                    "chat_template_caps": {"supports_tool_calls": true},
                    "build_info": "b12283-test"
                }))
                .into_response()
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move |h: HeaderMap, body: String| async move {
                if !authorized(&h, &s3) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                let v: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(v["stream"], true);
                assert_eq!(v["model"], "gardenwall-model");
                ([("content-type", "text/event-stream")], SSE).into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = InferenceClient::new(
        Connector::loopback(addr).unwrap(),
        ApiKey::generate().unwrap(),
    );
    (client, seen)
}

fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::new(Role::User, "hi")],
        ..ChatRequest::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn health_props_and_streaming_chat() {
    let (client, seen) = start(false).await;
    assert!(client.health().await.unwrap());
    let props = client.props().await.unwrap();
    assert_eq!(props.context_window, 8192);
    assert!(props.supports_tools);
    assert_eq!(props.build.as_deref(), Some("b12283-test"));

    let mut events = Vec::new();
    let response = client
        .chat(&request(), |e| {
            events.push(e);
            ControlFlow::Continue(())
        })
        .await
        .unwrap();
    assert_eq!(response.content, "Hello");
    assert_eq!(response.reasoning, "hmm");
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert_eq!(response.usage.unwrap().total_tokens, 10);
    assert!(response.time_to_first_token.is_some());
    assert!(events.contains(&StreamEvent::Content("Hel".into())));
    assert!(seen.load(Ordering::SeqCst), "bearer key must be sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_json_is_rejected() {
    let (client, _) = start(true).await;
    assert!(matches!(client.props().await, Err(ClientError::TooLarge)));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_stops_the_stream() {
    let (client, _) = start(false).await;
    let result = client.chat(&request(), |_| ControlFlow::Break(())).await;
    assert!(matches!(result, Err(ClientError::Cancelled)));
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_runtime_is_an_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = InferenceClient::new(
        Connector::loopback(addr).unwrap(),
        ApiKey::generate().unwrap(),
    );
    assert!(client.health().await.is_err());
}
