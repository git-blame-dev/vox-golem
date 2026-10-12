use super::MAX_BODY_BYTES;
use futures_util::StreamExt;
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use rmcp::{
    model::{ClientJsonRpcMessage, ClientRequest, ServerJsonRpcMessage},
    transport::{
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
        },
    },
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

type HttpError = StreamableHttpError<reqwest::Error>;

/// rmcp retains session/version negotiation and request correlation. This adapter
/// bounds request-POST bodies before JSON/SSE parsing, including HTTP error bodies.
#[derive(Clone)]
pub(super) struct BoundedHttp {
    client: reqwest::Client,
    remaining: Arc<AtomicUsize>,
}

impl BoundedHttp {
    pub(super) fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            remaining: Arc::new(AtomicUsize::new(MAX_BODY_BYTES)),
        }
    }
    pub(super) fn reset_budget(&self) {
        self.remaining.store(MAX_BODY_BYTES, Ordering::SeqCst);
    }
    fn charge(&self, bytes: usize) -> Result<(), HttpError> {
        self.remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(bytes)
            })
            .map(|_| ())
            .map_err(|_| invalid("MCP response byte budget exceeded"))
    }
    fn check_length(&self, response: &reqwest::Response) -> Result<(), HttpError> {
        if response
            .content_length()
            .is_some_and(|length| length > self.remaining.load(Ordering::SeqCst) as u64)
        {
            return Err(invalid("MCP response byte budget exceeded"));
        }
        Ok(())
    }
}

fn invalid(message: &'static str) -> HttpError {
    StreamableHttpError::UnexpectedServerResponse(message.into())
}

impl StreamableHttpClient for BoundedHttp {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth,
            headers,
            MAX_BODY_BYTES,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        // Native web endpoints are public; do not accept credential-bearing adapters.
        if auth.is_some() {
            return Err(invalid("Authenticated MCP transport is unsupported"));
        }
        let mut request = self
            .client
            .post(uri.as_ref())
            .header(ACCEPT, "application/json, text/event-stream");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let attached = session_id.is_some();
        if let Some(session) = session_id {
            request = request.header("mcp-session-id", session.as_ref());
        }
        let response = request
            .json(&message)
            .send()
            .await
            .map_err(StreamableHttpError::Client)?;
        self.check_length(&response)?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND && attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        let expects_reply = matches!(message, ClientJsonRpcMessage::Request(_));
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        let mut sse_bytes = 0;
        let mut framing = crate::custom_openai::SseDecoder::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(StreamableHttpError::Client)?;
            self.charge(chunk.len())?;
            if status.is_success() && mime == "text/event-stream" {
                // Only finite, one-response POST SSE is supported here.
                // No automatic replay/reconnect is allowed. Parse via the existing
                // bounded frontend-provider framer, then let rmcp correlate JSON-RPC.
                sse_bytes += chunk.len();
                if sse_bytes > max_sse_event_size.min(MAX_BODY_BYTES) {
                    return Err(invalid("MCP SSE response byte budget exceeded"));
                }
                framing.push(&chunk);
                while let Some(data) = framing
                    .next_data()
                    .map_err(|_| invalid("Invalid MCP SSE framing"))?
                {
                    if data.trim().is_empty() {
                        continue;
                    }
                    let parsed: ServerJsonRpcMessage = serde_json::from_str(&data)?;
                    if matches!(
                        parsed,
                        ServerJsonRpcMessage::Response(_) | ServerJsonRpcMessage::Error(_)
                    ) {
                        return Ok(StreamableHttpPostResponse::Json(parsed, session));
                    }
                    if matches!(parsed, ServerJsonRpcMessage::Request(_)) {
                        return Err(invalid("Server-initiated MCP requests are unsupported"));
                    }
                }
            } else {
                // Never trust Content-Length, and never extend before charging bytes.
                bytes.extend_from_slice(&chunk);
            }
        }
        if !status.is_success() {
            let parsed = serde_json::from_slice::<ServerJsonRpcMessage>(&bytes)
                .ok()
                .filter(|message| matches!(message, ServerJsonRpcMessage::Error(_)));
            if !attached && status.is_client_error() && !matches!(status.as_u16(), 401 | 403) {
                if let ClientJsonRpcMessage::Request(request) = &message {
                    if matches!(request.request, ClientRequest::DiscoverRequest(_)) {
                        let error = match parsed {
                            Some(ServerJsonRpcMessage::Error(error)) => error.error,
                            _ => rmcp::model::ErrorData::invalid_request(
                                "Legacy discovery rejected",
                                None,
                            ),
                        };
                        return Ok(StreamableHttpPostResponse::Json(
                            ServerJsonRpcMessage::error(error, Some(request.id.clone())),
                            None,
                        ));
                    }
                }
            }
            if mime == "application/json" {
                if let Some(parsed) = parsed {
                    return Ok(StreamableHttpPostResponse::Json(parsed, session));
                }
            }
            // No remote error payload is copied into diagnostic strings.
            return Err(StreamableHttpError::UnexpectedServerResponse(
                format!("HTTP {}", status.as_u16()).into(),
            ));
        }
        if mime == "text/event-stream" {
            return Err(StreamableHttpError::UnexpectedEndOfStream);
        }
        if mime != "application/json" {
            return Err(invalid("Unexpected MCP response content type"));
        }
        match serde_json::from_slice(&bytes) {
            Ok(parsed) => Ok(StreamableHttpPostResponse::Json(parsed, session)),
            Err(_) if !expects_reply => Ok(StreamableHttpPostResponse::Accepted),
            Err(_) => Err(invalid("Invalid MCP JSON response")),
        }
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, HttpError> {
        self.get_stream_with_max_sse_event_size(uri, session, last, auth, headers, MAX_BODY_BYTES)
            .await
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        limit: usize,
    ) -> Result<BoxedSseResponse, HttpError> {
        // rmcp's GET path does not buffer JSON/error bodies and applies its raw SSE limiter.
        self.client
            .get_stream_with_max_sse_event_size(
                uri,
                session,
                last,
                auth,
                headers,
                limit.min(MAX_BODY_BYTES),
            )
            .await
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.client
            .delete_session(uri, session, auth, headers)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn oversized_fixed_and_chunked_json_error_init_discovery_and_sse_are_bounded() {
        for method in ["initialize", "tools/list", "tools/call"] {
            for (status, mime, sse) in [
                (200, "application/json", false),
                (503, "application/json", false),
                (200, "text/event-stream", true),
            ] {
                for chunked in [false, true] {
                    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                    let uri = format!("http://{}/mcp", listener.local_addr().unwrap());
                    let server = tokio::spawn(async move {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut request = vec![0; 8192];
                        let _ = socket.read(&mut request).await.unwrap();
                        let payload = if sse {
                            format!(
                                "data: {{\"synthetic\":\"{}\"}}\n\n",
                                "x".repeat(MAX_BODY_BYTES + 1)
                            )
                        } else {
                            format!("{{\"synthetic\":\"{}\"}}", "x".repeat(MAX_BODY_BYTES + 1))
                        };
                        let size = if chunked {
                            "Transfer-Encoding: chunked\r\n".to_owned()
                        } else {
                            format!("Content-Length: {}\r\n", payload.len())
                        };
                        let wire = format!("HTTP/1.1 {status} Synthetic\r\nContent-Type: {mime}\r\n{size}Connection: close\r\n\r\n");
                        socket.write_all(wire.as_bytes()).await.unwrap();
                        for bytes in payload.as_bytes().chunks(4096) {
                            if chunked
                                && socket
                                    .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                                    .await
                                    .is_err()
                            {
                                return;
                            }
                            if socket.write_all(bytes).await.is_err() {
                                return;
                            }
                            if chunked && socket.write_all(b"\r\n").await.is_err() {
                                return;
                            }
                        }
                        if chunked {
                            let _ = socket.write_all(b"0\r\n\r\n").await;
                        }
                    });
                    let params = match method {
                        "initialize" => {
                            json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"synthetic","version":"1"}})
                        }
                        "tools/call" => json!({"name":"web_search","arguments":{}}),
                        _ => json!({}),
                    };
                    let message = serde_json::from_value(
                        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
                    )
                    .unwrap();
                    let client = BoundedHttp::new(super::super::http_client().unwrap());
                    let result = client
                        .post_message(uri.into(), message, None, None, HashMap::new())
                        .await;
                    assert!(
                        matches!(result, Err(StreamableHttpError::UnexpectedServerResponse(ref message)) if message.contains("byte budget")),
                        "{method} {status} {mime} chunked={chunked}: {result:?}"
                    );
                    assert!(client.remaining.load(Ordering::SeqCst) <= MAX_BODY_BYTES);
                    server.await.unwrap();
                }
            }
        }
    }
}
