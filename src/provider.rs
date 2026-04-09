use crate::config::{ModelConfig, ProviderConfig};
use crate::error::{AppError, AppResult, EXIT_AUTH, EXIT_NETWORK, EXIT_PROVIDER, EXIT_RATE_LIMIT};
use crate::media::MessageImage;
use crate::session::Usage;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::error::Error as _;
use std::time::{Duration, Instant};

const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 0;

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    pub images: Vec<MessageImage>,
    #[allow(dead_code)]
    pub tool_calls: Option<Vec<Value>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub provider_id: String,
    pub provider: ProviderConfig,
    pub model_id: String,
    pub model: ModelConfig,
    pub api_key: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub params: BTreeMap<String, Value>,
    pub timeout_secs: Option<u64>,
    pub tools: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub provider_id: String,
    pub model_id: String,
    pub content: String,
    pub finish_reason: String,
    pub usage: Usage,
    pub latency_ms: u64,
    pub raw: Value,
    pub tool_calls: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ChatStreamChunk {
    pub delta: String,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub tool_calls_delta: Vec<Value>,
    pub raw: Value,
}

pub async fn send_chat(request: ChatRequest) -> AppResult<ChatResponse> {
    match request.provider.kind.as_str() {
        "openai_compatible" => send_openai_compatible(request).await,
        "anthropic" => send_anthropic(request).await,
        "ollama" => send_ollama(request).await,
        other => Err(AppError::new(
            EXIT_PROVIDER,
            format!("provider kind `{other}` is not implemented yet"),
        )),
    }
}

pub async fn stream_chat<F>(request: ChatRequest, on_chunk: F) -> AppResult<ChatResponse>
where
    F: FnMut(ChatStreamChunk) -> AppResult<()>,
{
    match request.provider.kind.as_str() {
        "openai_compatible" => stream_openai_compatible(request, on_chunk).await,
        "anthropic" => stream_anthropic(request, on_chunk).await,
        "ollama" => stream_ollama(request, on_chunk).await,
        other => Err(AppError::new(
            EXIT_PROVIDER,
            format!("provider kind `{other}` is not implemented yet"),
        )),
    }
}

pub async fn test_provider(
    provider_id: &str,
    provider: &ProviderConfig,
    api_key: &str,
    models: &BTreeMap<String, ModelConfig>,
) -> AppResult<()> {
    match provider.kind.as_str() {
        "openai_compatible" => {
            let base_url = provider_base_url(provider)?;
            let client = build_client(provider.timeout)?;
            let headers = build_openai_headers(provider, api_key)?;
            let models_url = format!("{}/models", base_url.trim_end_matches('/'));
            match execute_healthcheck(client.get(models_url).headers(headers.clone()), provider_id)
                .await
            {
                Ok(()) => Ok(()),
                Err(err) if err.code == EXIT_NETWORK && err.message.contains("HTTP 404") => {
                    let Some(default_model) = &provider.default_model else {
                        return Err(err);
                    };
                    let remote_name = models
                        .get(default_model)
                        .map(|m| m.remote_name.as_str())
                        .unwrap_or(default_model);
                    let chat_url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
                    let body = json!({
                        "model": remote_name,
                        "messages": [{"role":"user","content":"ping"}],
                        "max_tokens": 1,
                        "stream": false,
                    });
                    execute_healthcheck(
                        client.post(chat_url).headers(headers).json(&body),
                        provider_id,
                    )
                    .await
                }
                Err(err) => Err(err),
            }
        }
        "anthropic" => {
            let base_url = provider_base_url(provider)?;
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let client = build_client(provider.timeout)?;
            let headers = build_anthropic_headers(provider, api_key)?;
            execute_healthcheck(client.get(url).headers(headers), provider_id).await
        }
        "ollama" => {
            let base_url = provider_base_url(provider)?;
            let url = format!("{}/tags", base_url.trim_end_matches('/'));
            let client = build_client(provider.timeout)?;
            let headers = build_ollama_headers(provider, api_key)?;
            execute_healthcheck(client.get(url).headers(headers), provider_id).await
        }
        other => Err(AppError::new(
            EXIT_PROVIDER,
            format!("provider kind `{other}` is not implemented yet"),
        )),
    }
}

async fn execute_healthcheck(request: reqwest::RequestBuilder, provider_id: &str) -> AppResult<()> {
    let response =
        send_request_with_retry(request, format!("provider `{provider_id}` request")).await?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(map_http_error(status.as_u16(), &text))
    }
}

async fn send_openai_compatible(request: ChatRequest) -> AppResult<ChatResponse> {
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_openai_headers(&request.provider, &request.api_key)?;
    let body = build_openai_body(&request, false);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    let text = response.text().await.map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format_provider_read_error("response", &err, timeout_secs),
        )
    })?;
    if !status.is_success() {
        return Err(map_http_error(status.as_u16(), &text));
    }
    let raw: Value = serde_json::from_str(&text).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider response: {err}"),
        )
    })?;
    let content = combine_reasoning_and_content(
        extract_openai_reasoning_content(&raw),
        extract_openai_content(&raw),
    )
    .unwrap_or_default();
    let finish_reason = raw["choices"][0]["finish_reason"]
        .as_str()
        .unwrap_or("stop")
        .to_string();
    let tool_calls = raw["choices"][0]["message"]["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let usage = Usage {
        input_tokens: raw["usage"]["prompt_tokens"].as_u64(),
        output_tokens: raw["usage"]["completion_tokens"].as_u64(),
        total_tokens: raw["usage"]["total_tokens"].as_u64(),
    };
    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason,
        usage,
        latency_ms: elapsed_ms(started),
        raw,
        tool_calls,
    })
}

async fn stream_openai_compatible<F>(
    request: ChatRequest,
    mut on_chunk: F,
) -> AppResult<ChatResponse>
where
    F: FnMut(ChatStreamChunk) -> AppResult<()>,
{
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_openai_headers(&request.provider, &request.api_key)?;
    let body = build_openai_body(&request, true);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(map_http_error(status.as_u16(), &text));
    }

    let mut parser = SseParser::default();
    let mut content = String::new();
    let mut finish_reason = "stop".to_string();
    let mut usage = Usage::default();
    let mut raw_events = Vec::new();

    let mut tool_calls_acc: Vec<Value> = Vec::new();
    let mut in_reasoning = false;

    let mut byte_stream = response.bytes_stream();
    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = chunk_result.map_err(|err| {
            AppError::new(
                EXIT_NETWORK,
                format_provider_read_error("stream chunk", &err, timeout_secs),
            )
        })?;
        for payload in parser.push_bytes(&chunk)? {
            if let Some(mut event) = parse_openai_stream_payload(&payload)? {
                event.delta = decorate_openai_stream_delta(
                    &mut in_reasoning,
                    &extract_openai_reasoning_delta(&event.raw),
                    &event.delta,
                    event.finish_reason.as_deref(),
                );
                accumulate_stream_event(
                    &mut content,
                    &mut finish_reason,
                    &mut usage,
                    &mut raw_events,
                    &event,
                );
                accumulate_tool_calls(&mut tool_calls_acc, &event.tool_calls_delta);
                on_chunk(event)?;
            }
        }
    }

    for payload in parser.finish()? {
        if let Some(mut event) = parse_openai_stream_payload(&payload)? {
            event.delta = decorate_openai_stream_delta(
                &mut in_reasoning,
                &extract_openai_reasoning_delta(&event.raw),
                &event.delta,
                event.finish_reason.as_deref(),
            );
            accumulate_stream_event(
                &mut content,
                &mut finish_reason,
                &mut usage,
                &mut raw_events,
                &event,
            );
            accumulate_tool_calls(&mut tool_calls_acc, &event.tool_calls_delta);
            on_chunk(event)?;
        }
    }

    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason,
        usage,
        latency_ms: elapsed_ms(started),
        raw: Value::Array(raw_events),
        tool_calls: tool_calls_acc,
    })
}

async fn send_anthropic(request: ChatRequest) -> AppResult<ChatResponse> {
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_anthropic_headers(&request.provider, &request.api_key)?;
    let body = build_anthropic_body(&request, false);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    let text = response.text().await.map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format_provider_read_error("response", &err, timeout_secs),
        )
    })?;
    if !status.is_success() {
        return Err(map_http_error(status.as_u16(), &text));
    }
    let raw: Value = serde_json::from_str(&text).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider response: {err}"),
        )
    })?;
    let content = extract_anthropic_content(&raw).ok_or_else(|| {
        AppError::new(
            EXIT_NETWORK,
            "provider response did not contain assistant content",
        )
    })?;
    let finish_reason = raw["stop_reason"]
        .as_str()
        .unwrap_or("end_turn")
        .to_string();
    let usage = Usage {
        input_tokens: raw["usage"]["input_tokens"].as_u64(),
        output_tokens: raw["usage"]["output_tokens"].as_u64(),
        total_tokens: sum_optional(
            raw["usage"]["input_tokens"].as_u64(),
            raw["usage"]["output_tokens"].as_u64(),
        ),
    };
    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason,
        usage,
        latency_ms: elapsed_ms(started),
        raw,
        tool_calls: Vec::new(),
    })
}

async fn stream_anthropic<F>(request: ChatRequest, mut on_chunk: F) -> AppResult<ChatResponse>
where
    F: FnMut(ChatStreamChunk) -> AppResult<()>,
{
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_anthropic_headers(&request.provider, &request.api_key)?;
    let body = build_anthropic_body(&request, true);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(map_http_error(status.as_u16(), &text));
    }

    let mut parser = SseParser::default();
    let mut content = String::new();
    let mut finish_reason = "end_turn".to_string();
    let mut usage = Usage::default();
    let mut raw_events = Vec::new();

    let mut byte_stream = response.bytes_stream();
    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = chunk_result.map_err(|err| {
            AppError::new(
                EXIT_NETWORK,
                format_provider_read_error("stream chunk", &err, timeout_secs),
            )
        })?;
        for payload in parser.push_bytes(&chunk)? {
            if let Some(event) = parse_anthropic_stream_payload(&payload)? {
                accumulate_stream_event(
                    &mut content,
                    &mut finish_reason,
                    &mut usage,
                    &mut raw_events,
                    &event,
                );
                on_chunk(event)?;
            }
        }
    }

    for payload in parser.finish()? {
        if let Some(event) = parse_anthropic_stream_payload(&payload)? {
            accumulate_stream_event(
                &mut content,
                &mut finish_reason,
                &mut usage,
                &mut raw_events,
                &event,
            );
            on_chunk(event)?;
        }
    }

    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason,
        usage,
        latency_ms: elapsed_ms(started),
        raw: Value::Array(raw_events),
        tool_calls: Vec::new(),
    })
}

async fn send_ollama(request: ChatRequest) -> AppResult<ChatResponse> {
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/chat", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_ollama_headers(&request.provider, &request.api_key)?;
    let body = build_ollama_body(&request, false);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    let text = response.text().await.map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format_provider_read_error("response", &err, timeout_secs),
        )
    })?;
    if !status.is_success() {
        return Err(map_http_error(status.as_u16(), &text));
    }
    let raw: Value = serde_json::from_str(&text).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider response: {err}"),
        )
    })?;
    let content = extract_ollama_content(&raw).ok_or_else(|| {
        AppError::new(
            EXIT_NETWORK,
            "provider response did not contain assistant content",
        )
    })?;
    let usage = extract_ollama_usage(&raw).unwrap_or_default();
    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason: raw["done_reason"].as_str().unwrap_or("stop").to_string(),
        usage,
        latency_ms: elapsed_ms(started),
        raw,
        tool_calls: Vec::new(),
    })
}

async fn stream_ollama<F>(request: ChatRequest, mut on_chunk: F) -> AppResult<ChatResponse>
where
    F: FnMut(ChatStreamChunk) -> AppResult<()>,
{
    let base_url = provider_base_url(&request.provider)?;
    let url = format!("{}/chat", base_url.trim_end_matches('/'));
    let timeout_secs = request.timeout_secs.or(request.provider.timeout);
    let client = build_client(timeout_secs)?;
    let headers = build_ollama_headers(&request.provider, &request.api_key)?;
    let body = build_ollama_body(&request, true);

    let started = Instant::now();
    let response = send_request_with_retry(
        client.post(url).headers(headers).json(&body),
        "chat request".to_string(),
    )
    .await?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(map_http_error(status.as_u16(), &text));
    }

    let mut parser = JsonLineParser::default();
    let mut content = String::new();
    let mut finish_reason = "stop".to_string();
    let mut usage = Usage::default();
    let mut raw_events = Vec::new();

    let mut byte_stream = response.bytes_stream();
    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = chunk_result.map_err(|err| {
            AppError::new(
                EXIT_NETWORK,
                format_provider_read_error("stream chunk", &err, timeout_secs),
            )
        })?;
        for payload in parser.push_bytes(&chunk)? {
            if let Some(event) = parse_ollama_stream_payload(&payload)? {
                accumulate_stream_event(
                    &mut content,
                    &mut finish_reason,
                    &mut usage,
                    &mut raw_events,
                    &event,
                );
                on_chunk(event)?;
            }
        }
    }

    for payload in parser.finish()? {
        if let Some(event) = parse_ollama_stream_payload(&payload)? {
            accumulate_stream_event(
                &mut content,
                &mut finish_reason,
                &mut usage,
                &mut raw_events,
                &event,
            );
            on_chunk(event)?;
        }
    }

    Ok(ChatResponse {
        provider_id: request.provider_id,
        model_id: request.model_id,
        content,
        finish_reason,
        usage,
        latency_ms: elapsed_ms(started),
        raw: Value::Array(raw_events),
        tool_calls: Vec::new(),
    })
}

fn build_client(timeout_secs: Option<u64>) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS));
    if let Some(timeout) = request_timeout(effective_request_timeout_secs(timeout_secs)) {
        builder = builder.timeout(timeout);
    }
    builder
        .build()
        .map_err(|err| AppError::new(EXIT_NETWORK, format!("failed to build HTTP client: {err}")))
}

fn effective_request_timeout_secs(timeout_secs: Option<u64>) -> u64 {
    timeout_secs.unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS)
}

fn request_timeout(timeout_secs: u64) -> Option<Duration> {
    (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs))
}

fn format_provider_read_error(
    kind: &str,
    err: &reqwest::Error,
    timeout_secs: Option<u64>,
) -> String {
    let details = reqwest_error_details(err);
    match request_timeout(effective_request_timeout_secs(timeout_secs)) {
        Some(timeout) if err.is_timeout() => format!(
            "failed to read provider {kind}: request timed out after {}s: {details}",
            timeout.as_secs()
        ),
        _ => format!("failed to read provider {kind}: {details}"),
    }
}

fn reqwest_error_details(err: &reqwest::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(inner) = source {
        let text = inner.to_string();
        if !text.is_empty() && !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        source = inner.source();
    }
    parts.join(": ")
}

async fn send_request_with_retry(
    request: reqwest::RequestBuilder,
    context: String,
) -> AppResult<reqwest::Response> {
    let retry_request = request.try_clone();
    match request.send().await {
        Ok(response) => Ok(response),
        Err(first_err) => {
            let Some(retry_request) = retry_request else {
                return Err(AppError::new(
                    EXIT_NETWORK,
                    format!("{context} failed: {first_err}"),
                ));
            };
            retry_request.send().await.map_err(|retry_err| {
                AppError::new(
                    EXIT_NETWORK,
                    format!("{context} failed after retry: {retry_err}"),
                )
            })
        }
    }
}

fn build_openai_body(request: &ChatRequest, stream: bool) -> Value {
    let messages = patched_messages(request);
    let mut body = Map::new();
    body.insert(
        "model".to_string(),
        Value::String(request.model.remote_name.clone()),
    );
    body.insert(
        "messages".to_string(),
        Value::Array(
            messages
                .iter()
                .map(|msg| {
                    let mut m = Map::new();
                    m.insert("role".to_string(), json!(msg.role));
                    if msg.role == "tool" {
                        m.insert("content".to_string(), json!(msg.content));
                        if let Some(id) = &msg.tool_call_id {
                            m.insert("tool_call_id".to_string(), json!(id));
                        }
                    } else if msg.role == "assistant" {
                        if let Some(tc) = &msg.tool_calls {
                            m.insert("tool_calls".to_string(), Value::Array(tc.clone()));
                            // content can be null when assistant has tool_calls
                            if msg.content.is_empty() {
                                m.insert("content".to_string(), Value::Null);
                            } else {
                                m.insert("content".to_string(), json!(msg.content));
                            }
                        } else {
                            m.insert(
                                "content".to_string(),
                                build_openai_message_content_value(msg),
                            );
                        }
                    } else {
                        m.insert(
                            "content".to_string(),
                            build_openai_message_content_value(msg),
                        );
                    }
                    Value::Object(m)
                })
                .collect(),
        ),
    );
    body.insert("stream".to_string(), Value::Bool(stream));
    if stream {
        body.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }
    if !request.tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(request.tools.clone()));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(max_tokens) = request.max_output_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens));
    }
    if let Some(reasoning_effort) = resolved_openai_reasoning_effort(request) {
        body.insert("reasoning_effort".to_string(), json!(reasoning_effort));
    }
    for (key, value) in &request.params {
        body.insert(key.clone(), value.clone());
    }
    Value::Object(body)
}

fn resolved_openai_reasoning_effort(request: &ChatRequest) -> Option<String> {
    if request.params.contains_key("reasoning_effort") || request.params.contains_key("thinking") {
        return None;
    }
    if let Some(reasoning_effort) = request.model.reasoning_effort.clone() {
        let trimmed = reasoning_effort.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if !request
        .model
        .capabilities
        .iter()
        .any(|capability| capability == "reasoning")
    {
        return None;
    }
    None
}

fn build_openai_message_content_value(message: &ChatMessage) -> Value {
    if message.images.is_empty() {
        return json!(message.content);
    }

    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(json!({
            "type": "text",
            "text": message.content,
        }));
    }
    for image in &message.images {
        parts.push(json!({
            "type": "image_url",
            "image_url": {
                "url": image.data_url(),
            }
        }));
    }
    Value::Array(parts)
}

fn build_anthropic_body(request: &ChatRequest, stream: bool) -> Value {
    let messages = patched_messages(request);
    let mut body = Map::new();
    body.insert(
        "model".to_string(),
        Value::String(request.model.remote_name.clone()),
    );
    body.insert(
        "max_tokens".to_string(),
        json!(request.max_output_tokens.unwrap_or(1024)),
    );
    let (system, messages) = split_system_messages(&messages);
    if let Some(system) = system {
        body.insert("system".to_string(), Value::String(system));
    }
    body.insert("messages".to_string(), Value::Array(messages));
    if stream {
        body.insert("stream".to_string(), Value::Bool(true));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    for (key, value) in &request.params {
        body.insert(key.clone(), value.clone());
    }
    Value::Object(body)
}

fn build_ollama_body(request: &ChatRequest, stream: bool) -> Value {
    let messages = patched_messages(request);
    let mut body = Map::new();
    body.insert(
        "model".to_string(),
        Value::String(request.model.remote_name.clone()),
    );
    body.insert(
        "messages".to_string(),
        Value::Array(
            messages
                .iter()
                .map(|msg| json!({ "role": msg.role, "content": msg.content }))
                .collect(),
        ),
    );
    body.insert("stream".to_string(), Value::Bool(stream));
    if let Some(temperature) = request.temperature {
        let mut options = Map::new();
        options.insert("temperature".to_string(), json!(temperature));
        body.insert("options".to_string(), Value::Object(options));
    }
    for (key, value) in &request.params {
        body.insert(key.clone(), value.clone());
    }
    Value::Object(body)
}

fn patched_messages(request: &ChatRequest) -> Vec<ChatMessage> {
    let mut messages = request.messages.clone();
    if request.model.patches.system_to_user.unwrap_or(false) {
        for message in &mut messages {
            if message.role == "system" {
                message.role = "user".to_string();
            }
        }
    }
    messages
}

fn split_system_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system_parts = Vec::new();
    let mut result = Vec::new();
    for message in messages {
        if message.role == "system" {
            system_parts.push(message.content.clone());
        } else {
            result.push(json!({
                "role": message.role,
                "content": build_anthropic_message_content_value(message),
            }));
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (system, result)
}

fn build_anthropic_message_content_value(message: &ChatMessage) -> Value {
    if message.images.is_empty() {
        return json!(message.content);
    }

    let mut parts = Vec::new();
    if !message.content.is_empty() {
        parts.push(json!({
            "type": "text",
            "text": message.content,
        }));
    }
    for image in &message.images {
        parts.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": image.data,
            }
        }));
    }
    Value::Array(parts)
}

fn build_openai_headers(provider: &ProviderConfig, api_key: &str) -> AppResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    add_openai_compatible_auth(&mut headers, provider, api_key)?;
    if let Some(org) = &provider.org {
        let value = HeaderValue::from_str(org).map_err(|err| {
            AppError::new(EXIT_PROVIDER, format!("invalid org header value: {err}"))
        })?;
        headers.insert(HeaderName::from_static("openai-organization"), value);
    }
    if let Some(project) = &provider.project {
        let value = HeaderValue::from_str(project).map_err(|err| {
            AppError::new(
                EXIT_PROVIDER,
                format!("invalid project header value: {err}"),
            )
        })?;
        headers.insert(HeaderName::from_static("openai-project"), value);
    }
    apply_custom_headers(&mut headers, provider)?;
    Ok(headers)
}

fn add_openai_compatible_auth(
    headers: &mut HeaderMap,
    provider: &ProviderConfig,
    api_key: &str,
) -> AppResult<()> {
    if api_key.trim().is_empty() {
        return Ok(());
    }
    let value = if openai_compatible_uses_raw_authorization(provider) {
        api_key.to_string()
    } else {
        format!("Bearer {api_key}")
    };
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&value)
            .map_err(|err| AppError::new(EXIT_AUTH, format!("invalid API key header: {err}")))?,
    );
    Ok(())
}

fn build_anthropic_headers(provider: &ProviderConfig, api_key: &str) -> AppResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    if api_key.trim().is_empty() {
        return Err(AppError::new(
            EXIT_AUTH,
            "missing API key for anthropic provider",
        ));
    }
    headers.insert(
        HeaderName::from_static("x-api-key"),
        HeaderValue::from_str(api_key)
            .map_err(|err| AppError::new(EXIT_AUTH, format!("invalid API key header: {err}")))?,
    );
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );
    apply_custom_headers(&mut headers, provider)?;
    Ok(headers)
}

fn build_ollama_headers(provider: &ProviderConfig, api_key: &str) -> AppResult<HeaderMap> {
    let mut headers = HeaderMap::new();
    add_bearer_auth(&mut headers, api_key)?;
    apply_custom_headers(&mut headers, provider)?;
    Ok(headers)
}

fn add_bearer_auth(headers: &mut HeaderMap, api_key: &str) -> AppResult<()> {
    if api_key.trim().is_empty() {
        return Ok(());
    }
    let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|err| AppError::new(EXIT_AUTH, format!("invalid API key header: {err}")))?;
    headers.insert(AUTHORIZATION, auth_value);
    Ok(())
}

fn openai_compatible_uses_raw_authorization(_provider: &ProviderConfig) -> bool {
    false
}

fn apply_custom_headers(headers: &mut HeaderMap, provider: &ProviderConfig) -> AppResult<()> {
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    for (key, value) in &provider.headers {
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|err| {
            AppError::new(EXIT_PROVIDER, format!("invalid header name `{key}`: {err}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|err| {
            AppError::new(
                EXIT_PROVIDER,
                format!("invalid header value for `{key}`: {err}"),
            )
        })?;
        headers.insert(name, value);
    }
    Ok(())
}

fn provider_base_url(provider: &ProviderConfig) -> AppResult<String> {
    match provider.kind.as_str() {
        "anthropic" => Ok(provider
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string())),
        "ollama" => Ok(provider
            .base_url
            .clone()
            .unwrap_or_else(|| "http://localhost:11434/api".to_string())),
        _ => provider.base_url.clone().ok_or_else(|| {
            AppError::new(
                EXIT_PROVIDER,
                "provider.base_url is required for this provider",
            )
        }),
    }
}

fn extract_openai_content(raw: &Value) -> Option<String> {
    extract_openai_text_value(&raw["choices"][0]["message"]["content"])
}

fn extract_openai_reasoning_content(raw: &Value) -> Option<String> {
    extract_openai_text_value(&raw["choices"][0]["message"]["reasoning_content"])
        .or_else(|| extract_openai_text_value(&raw["choices"][0]["message"]["reasoning"]))
}

fn extract_anthropic_content(raw: &Value) -> Option<String> {
    let parts = raw["content"].as_array()?;
    let mut merged = String::new();
    for part in parts {
        if part["type"].as_str() == Some("text") {
            if let Some(text) = part["text"].as_str() {
                merged.push_str(text);
            }
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn extract_ollama_content(raw: &Value) -> Option<String> {
    raw["message"]["content"]
        .as_str()
        .map(|value| value.to_string())
}

fn extract_openai_delta(raw: &Value) -> String {
    extract_openai_text_value(&raw["choices"][0]["delta"]["content"]).unwrap_or_default()
}

fn extract_openai_reasoning_delta(raw: &Value) -> String {
    extract_openai_text_value(&raw["choices"][0]["delta"]["reasoning_content"])
        .or_else(|| extract_openai_text_value(&raw["choices"][0]["delta"]["reasoning"]))
        .unwrap_or_default()
}

fn extract_openai_text_value(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let parts = value.as_array()?;
    let mut merged = String::new();
    for part in parts {
        if let Some(text) = part["text"].as_str() {
            merged.push_str(text);
        } else if let Some(text) = part["content"].as_str() {
            merged.push_str(text);
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn combine_reasoning_and_content(
    reasoning: Option<String>,
    content: Option<String>,
) -> Option<String> {
    let reasoning = reasoning.filter(|value| !value.trim().is_empty());
    let content = content.filter(|value| !value.trim().is_empty());
    match (reasoning, content) {
        (Some(reasoning), Some(content)) => {
            Some(format!("<think>\n{reasoning}\n</think>\n\n{content}"))
        }
        (Some(reasoning), None) => Some(format!("<think>\n{reasoning}\n</think>")),
        (None, Some(content)) => Some(content),
        (None, None) => None,
    }
}

fn decorate_openai_stream_delta(
    in_reasoning: &mut bool,
    reasoning_delta: &str,
    answer_delta: &str,
    finish_reason: Option<&str>,
) -> String {
    let answer_delta = if is_openai_control_content_delta(answer_delta) {
        ""
    } else {
        answer_delta
    };
    let mut output = String::new();
    if !reasoning_delta.is_empty() {
        if !*in_reasoning {
            output.push_str("<think>\n");
            *in_reasoning = true;
        }
        output.push_str(reasoning_delta);
    }
    if *in_reasoning && (!answer_delta.is_empty() || finish_reason.is_some()) {
        output.push_str("</think>\n\n");
        *in_reasoning = false;
    }
    output.push_str(answer_delta);
    output
}

fn is_openai_control_content_delta(delta: &str) -> bool {
    matches!(delta.trim(), "FINISHED")
}

fn extract_ollama_usage(raw: &Value) -> Option<Usage> {
    let input = raw["prompt_eval_count"].as_u64();
    let output = raw["eval_count"].as_u64();
    let total = sum_optional(input, output);
    if input.is_none() && output.is_none() && total.is_none() {
        None
    } else {
        Some(Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        })
    }
}

fn sum_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn accumulate_stream_event(
    content: &mut String,
    finish_reason: &mut String,
    usage: &mut Usage,
    raw_events: &mut Vec<Value>,
    event: &ChatStreamChunk,
) {
    if !event.delta.is_empty() {
        content.push_str(&event.delta);
    }
    if let Some(reason) = &event.finish_reason {
        *finish_reason = reason.clone();
    }
    if let Some(stream_usage) = &event.usage {
        merge_usage(usage, stream_usage);
    }
    raw_events.push(event.raw.clone());
}

fn merge_usage(target: &mut Usage, update: &Usage) {
    target.input_tokens = update.input_tokens.or(target.input_tokens);
    target.output_tokens = update.output_tokens.or(target.output_tokens);
    target.total_tokens = sum_optional(target.input_tokens, target.output_tokens)
        .or(update.total_tokens)
        .or(target.total_tokens);
}

/// Accumulate streaming tool_calls deltas into complete tool_call objects.
/// In OpenAI streaming, tool_calls arrive as incremental deltas with an index field.
fn accumulate_tool_calls(acc: &mut Vec<Value>, deltas: &[Value]) {
    for delta in deltas {
        let index = delta["index"].as_u64().unwrap_or(0) as usize;

        // Grow the accumulator if needed
        while acc.len() <= index {
            acc.push(
                json!({"id": "", "type": "function", "function": {"name": "", "arguments": ""}}),
            );
        }

        // Merge fields
        if let Some(id) = delta["id"].as_str() {
            acc[index]["id"] = json!(id);
        }
        if let Some(name) = delta["function"]["name"].as_str() {
            let existing = acc[index]["function"]["name"].as_str().unwrap_or("");
            acc[index]["function"]["name"] = json!(format!("{}{}", existing, name));
        }
        if let Some(args) = delta["function"]["arguments"].as_str() {
            let existing = acc[index]["function"]["arguments"].as_str().unwrap_or("");
            acc[index]["function"]["arguments"] = json!(format!("{}{}", existing, args));
        }
    }
}

fn parse_openai_stream_payload(payload: &str) -> AppResult<Option<ChatStreamChunk>> {
    if payload.trim().is_empty() {
        return Ok(None);
    }
    if payload.trim() == "[DONE]" {
        return Ok(None);
    }
    let raw: Value = serde_json::from_str(payload).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider stream event: {err}"),
        )
    })?;
    let tool_calls_delta = raw["choices"][0]["delta"]["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(Some(ChatStreamChunk {
        delta: extract_openai_delta(&raw),
        finish_reason: raw["choices"][0]["finish_reason"]
            .as_str()
            .map(|value| value.to_string()),
        usage: extract_openai_usage(&raw),
        tool_calls_delta,
        raw,
    }))
}

fn parse_anthropic_stream_payload(payload: &str) -> AppResult<Option<ChatStreamChunk>> {
    if payload.trim().is_empty() {
        return Ok(None);
    }
    let raw: Value = serde_json::from_str(payload).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider stream event: {err}"),
        )
    })?;
    match raw["type"].as_str().unwrap_or_default() {
        "ping" | "content_block_start" | "content_block_stop" | "message_stop" => Ok(None),
        "error" => {
            let message = raw["error"]["message"]
                .as_str()
                .or_else(|| raw["message"].as_str())
                .unwrap_or("provider stream error");
            let code = match raw["error"]["type"].as_str() {
                Some("overloaded_error") => EXIT_RATE_LIMIT,
                _ => EXIT_NETWORK,
            };
            Err(AppError::new(code, message))
        }
        "message_start" => {
            let usage = Usage {
                input_tokens: raw["message"]["usage"]["input_tokens"].as_u64(),
                output_tokens: raw["message"]["usage"]["output_tokens"].as_u64(),
                total_tokens: sum_optional(
                    raw["message"]["usage"]["input_tokens"].as_u64(),
                    raw["message"]["usage"]["output_tokens"].as_u64(),
                ),
            };
            Ok(Some(ChatStreamChunk {
                delta: String::new(),
                finish_reason: None,
                usage: Some(usage),
                tool_calls_delta: Vec::new(),
                raw,
            }))
        }
        "content_block_delta" => {
            let delta = if raw["delta"]["type"].as_str() == Some("text_delta") {
                raw["delta"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            } else {
                String::new()
            };
            Ok(Some(ChatStreamChunk {
                delta,
                finish_reason: None,
                usage: None,
                tool_calls_delta: Vec::new(),
                raw,
            }))
        }
        "message_delta" => {
            let input = raw["usage"]["input_tokens"].as_u64();
            let output = raw["usage"]["output_tokens"].as_u64();
            Ok(Some(ChatStreamChunk {
                delta: String::new(),
                finish_reason: raw["delta"]["stop_reason"]
                    .as_str()
                    .map(|value| value.to_string()),
                usage: Some(Usage {
                    input_tokens: input,
                    output_tokens: output,
                    total_tokens: sum_optional(input, output),
                }),
                tool_calls_delta: Vec::new(),
                raw,
            }))
        }
        _ => Ok(None),
    }
}

fn parse_ollama_stream_payload(payload: &str) -> AppResult<Option<ChatStreamChunk>> {
    if payload.trim().is_empty() {
        return Ok(None);
    }
    let raw: Value = serde_json::from_str(payload).map_err(|err| {
        AppError::new(
            EXIT_NETWORK,
            format!("failed to parse provider stream event: {err}"),
        )
    })?;
    if let Some(error) = raw["error"].as_str() {
        return Err(AppError::new(EXIT_NETWORK, error));
    }
    Ok(Some(ChatStreamChunk {
        delta: raw["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        finish_reason: if raw["done"].as_bool().unwrap_or(false) {
            raw["done_reason"].as_str().map(|value| value.to_string())
        } else {
            None
        },
        usage: extract_ollama_usage(&raw),
        tool_calls_delta: Vec::new(),
        raw,
    }))
}

fn extract_openai_usage(raw: &Value) -> Option<Usage> {
    let usage_value = &raw["usage"];
    if usage_value.is_null() {
        return None;
    }
    let usage = Usage {
        input_tokens: usage_value["prompt_tokens"].as_u64(),
        output_tokens: usage_value["completion_tokens"].as_u64(),
        total_tokens: usage_value["total_tokens"].as_u64(),
    };
    if usage.input_tokens.is_none() && usage.output_tokens.is_none() && usage.total_tokens.is_none()
    {
        None
    } else {
        Some(usage)
    }
}

#[derive(Default)]
struct SseParser {
    pending: Vec<u8>,
    event_lines: Vec<String>,
}

impl SseParser {
    fn push_bytes(&mut self, bytes: &[u8]) -> AppResult<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line_bytes = self.pending.drain(..=position).collect::<Vec<u8>>();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = std::str::from_utf8(&line_bytes).map_err(|err| {
                AppError::new(
                    EXIT_NETWORK,
                    format!("provider stream contained invalid UTF-8: {err}"),
                )
            })?;
            if line.is_empty() {
                if let Some(event) = self.take_event() {
                    events.push(event);
                }
            } else {
                self.event_lines.push(line.to_string());
            }
        }
        Ok(events)
    }

    fn finish(&mut self) -> AppResult<Vec<String>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let line = std::str::from_utf8(&self.pending).map_err(|err| {
                AppError::new(
                    EXIT_NETWORK,
                    format!("provider stream contained invalid UTF-8: {err}"),
                )
            })?;
            let line = line.trim_end_matches('\r');
            if !line.is_empty() {
                self.event_lines.push(line.to_string());
            }
            self.pending.clear();
        }
        if let Some(event) = self.take_event() {
            events.push(event);
        }
        Ok(events)
    }

    fn take_event(&mut self) -> Option<String> {
        if self.event_lines.is_empty() {
            return None;
        }
        let mut data_lines = Vec::new();
        for line in self.event_lines.drain(..) {
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start().to_string());
            }
        }
        let payload = data_lines.join("\n");
        if payload.trim().is_empty() {
            None
        } else {
            Some(payload)
        }
    }
}

#[derive(Default)]
struct JsonLineParser {
    pending: Vec<u8>,
}

impl JsonLineParser {
    fn push_bytes(&mut self, bytes: &[u8]) -> AppResult<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line_bytes = self.pending.drain(..=position).collect::<Vec<u8>>();
            if line_bytes.last() == Some(&b'\n') {
                line_bytes.pop();
            }
            if line_bytes.last() == Some(&b'\r') {
                line_bytes.pop();
            }
            let line = std::str::from_utf8(&line_bytes).map_err(|err| {
                AppError::new(
                    EXIT_NETWORK,
                    format!("provider stream contained invalid UTF-8: {err}"),
                )
            })?;
            if !line.trim().is_empty() {
                lines.push(line.to_string());
            }
        }
        Ok(lines)
    }

    fn finish(&mut self) -> AppResult<Vec<String>> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let line = std::str::from_utf8(&self.pending)
            .map_err(|err| {
                AppError::new(
                    EXIT_NETWORK,
                    format!("provider stream contained invalid UTF-8: {err}"),
                )
            })?
            .trim()
            .to_string();
        self.pending.clear();
        if line.is_empty() {
            Ok(Vec::new())
        } else {
            Ok(vec![line])
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn map_http_error(status: u16, text: &str) -> AppError {
    let message = if let Ok(json) = serde_json::from_str::<Value>(text) {
        json["error"]["message"]
            .as_str()
            .or_else(|| json["error"].as_str())
            .or_else(|| json["message"].as_str())
            .unwrap_or(text)
            .to_string()
    } else {
        text.to_string()
    };
    match status {
        401 | 403 => AppError::new(EXIT_AUTH, format!("authentication failed: {message}")),
        429 | 529 => AppError::new(EXIT_RATE_LIMIT, format!("rate limited: {message}")),
        _ => AppError::new(
            EXIT_NETWORK,
            format!("provider returned HTTP {status}: {message}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelPatchConfig;

    #[test]
    fn sse_parser_handles_chunk_boundaries() {
        let mut parser = SseParser::default();
        let first = parser
            .push_bytes(b"data: {\"choices\":[{\"delta\":{\"content\":\"hel")
            .unwrap();
        assert!(first.is_empty());
        let second = parser
            .push_bytes(b"lo\"},\"finish_reason\":null}]}\n\n")
            .unwrap();
        assert_eq!(second.len(), 1);
        let event = parse_openai_stream_payload(&second[0]).unwrap().unwrap();
        assert_eq!(event.delta, "hello");
        assert!(event.finish_reason.is_none());
    }

    #[test]
    fn parse_openai_stream_usage_event() {
        let event = parse_openai_stream_payload(
            "{\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}",
        )
        .unwrap()
        .unwrap();
        assert_eq!(event.usage.unwrap().total_tokens, Some(3));
    }

    #[test]
    fn parse_openai_stream_ignores_empty_payload() {
        assert!(parse_openai_stream_payload("").unwrap().is_none());
        assert!(parse_openai_stream_payload("   ").unwrap().is_none());
    }

    #[test]
    fn extract_openai_reasoning_content_from_message() {
        let raw = json!({
            "choices": [{
                "message": {
                    "content": "2",
                    "reasoning_content": "先做加法。"
                }
            }]
        });
        let combined = combine_reasoning_and_content(
            extract_openai_reasoning_content(&raw),
            extract_openai_content(&raw),
        )
        .unwrap();
        assert!(combined.contains("<think>"));
        assert!(combined.contains("先做加法。"));
        assert!(combined.ends_with("2"));
    }

    #[test]
    fn decorate_openai_stream_delta_wraps_reasoning_before_answer() {
        let mut in_reasoning = false;
        let first = decorate_openai_stream_delta(&mut in_reasoning, "先分析", "", None);
        assert_eq!(first, "<think>\n先分析");
        assert!(in_reasoning);

        let second = decorate_openai_stream_delta(&mut in_reasoning, "", "结论", None);
        assert_eq!(second, "</think>\n\n结论");
        assert!(!in_reasoning);
    }

    #[test]
    fn decorate_openai_stream_delta_ignores_finished_control_tokens() {
        let mut in_reasoning = false;
        let first = decorate_openai_stream_delta(&mut in_reasoning, "先搜索", "", None);
        assert_eq!(first, "<think>\n先搜索");
        assert!(in_reasoning);

        let middle = decorate_openai_stream_delta(&mut in_reasoning, "", "FINISHED", None);
        assert!(middle.is_empty());
        assert!(in_reasoning);

        let more_reasoning = decorate_openai_stream_delta(&mut in_reasoning, "继续搜索", "", None);
        assert_eq!(more_reasoning, "继续搜索");
        assert!(in_reasoning);

        let answer = decorate_openai_stream_delta(&mut in_reasoning, "", "最终答案", None);
        assert_eq!(answer, "</think>\n\n最终答案");
        assert!(!in_reasoning);
    }

    #[test]
    fn build_openai_body_does_not_infer_reasoning_effort_without_config() {
        let request = ChatRequest {
            provider_id: "openclawbs".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "claude-sonnet-4-6".to_string(),
            model: ModelConfig {
                provider: "openclawbs".to_string(),
                remote_name: "claude-sonnet-4-6".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["chat".to_string(), "reasoning".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig::default(),
            },
            api_key: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        let body = build_openai_body(&request, false);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn build_openai_body_respects_explicit_reasoning_effort_param() {
        let mut params = BTreeMap::new();
        params.insert("reasoning_effort".to_string(), json!("high"));
        let request = ChatRequest {
            provider_id: "openclawbs".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "claude-sonnet-4-6".to_string(),
            model: ModelConfig {
                provider: "openclawbs".to_string(),
                remote_name: "claude-sonnet-4-6".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["reasoning".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig::default(),
            },
            api_key: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
            params,
            timeout_secs: None,
            tools: Vec::new(),
        };
        let body = build_openai_body(&request, false);
        assert_eq!(body["reasoning_effort"].as_str(), Some("high"));
    }

    #[test]
    fn build_openai_body_uses_model_reasoning_effort_from_config() {
        let request = ChatRequest {
            provider_id: "openclawbs".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "claude-sonnet-4-6".to_string(),
            model: ModelConfig {
                provider: "openclawbs".to_string(),
                remote_name: "claude-sonnet-4-6".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["reasoning".to_string()],
                temperature: None,
                reasoning_effort: Some("high".to_string()),
                patches: ModelPatchConfig::default(),
            },
            api_key: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        let body = build_openai_body(&request, false);
        assert_eq!(body["reasoning_effort"].as_str(), Some("high"));
    }

    #[test]
    fn build_openai_body_serializes_image_parts_for_user_messages() {
        let request = ChatRequest {
            provider_id: "cpap".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "qw-coder-model".to_string(),
            model: ModelConfig {
                provider: "cpap".to_string(),
                remote_name: "qw/coder-model".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["chat".to_string(), "vision".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig::default(),
            },
            api_key: String::new(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "describe".to_string(),
                images: vec![MessageImage {
                    media_type: "image/png".to_string(),
                    data: "YWJj".to_string(),
                }],
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        let body = build_openai_body(&request, false);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"].as_str(), Some("text"));
        assert_eq!(content[0]["text"].as_str(), Some("describe"));
        assert_eq!(content[1]["type"].as_str(), Some("image_url"));
        assert_eq!(
            content[1]["image_url"]["url"].as_str(),
            Some("data:image/png;base64,YWJj")
        );
    }

    #[test]
    fn build_openai_body_applies_system_to_user_patch() {
        let request = ChatRequest {
            provider_id: "cpap".to_string(),
            provider: ProviderConfig {
                kind: "openai_compatible".to_string(),
                ..ProviderConfig::default()
            },
            model_id: "team-gpt-5-4".to_string(),
            model: ModelConfig {
                provider: "cpap".to_string(),
                remote_name: "team/gpt-5.4".to_string(),
                display_name: None,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["chat".to_string()],
                temperature: None,
                reasoning_effort: None,
                patches: ModelPatchConfig {
                    system_to_user: Some(true),
                },
            },
            api_key: String::new(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "follow the rules".to_string(),
                    images: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                    images: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            temperature: None,
            max_output_tokens: None,
            params: BTreeMap::new(),
            timeout_secs: None,
            tools: Vec::new(),
        };
        let body = build_openai_body(&request, false);
        assert_eq!(body["messages"][0]["role"].as_str(), Some("user"));
        assert_eq!(
            body["messages"][0]["content"].as_str(),
            Some("follow the rules")
        );
    }

    #[test]
    fn parse_anthropic_stream_delta() {
        let event = parse_anthropic_stream_payload(
            "{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}",
        )
        .unwrap()
        .unwrap();
        assert_eq!(event.delta, "hello");
    }

    #[test]
    fn json_line_parser_handles_chunk_boundaries() {
        let mut parser = JsonLineParser::default();
        let first = parser
            .push_bytes(b"{\"message\":{\"content\":\"he\"}")
            .unwrap();
        assert!(first.is_empty());
        let second = parser.push_bytes(b",\"done\":false}\n").unwrap();
        assert_eq!(second.len(), 1);
        let event = parse_ollama_stream_payload(&second[0]).unwrap().unwrap();
        assert_eq!(event.delta, "he");
    }

    #[test]
    fn sse_parser_ignores_event_without_data_lines() {
        let mut parser = SseParser::default();
        let events = parser.push_bytes(b"event: ping\n\n").unwrap();
        assert!(events.is_empty());

        let events = parser.push_bytes(b": keep-alive\n\n").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn request_timeout_is_disabled_by_default() {
        assert_eq!(effective_request_timeout_secs(None), 0);
        assert_eq!(effective_request_timeout_secs(Some(0)), 0);
        assert_eq!(effective_request_timeout_secs(Some(600)), 600);
        assert_eq!(request_timeout(0), None);
        assert_eq!(request_timeout(600), Some(Duration::from_secs(600)));
    }
}
