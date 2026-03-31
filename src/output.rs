use crate::cli::OutputFormat;
use crate::error::{AppError, AppResult, EXIT_ARGS};
use crate::session::Usage;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct AskOutput {
    pub ok: bool,
    pub provider: String,
    pub model: String,
    pub session_id: String,
    pub message: AssistantMessage,
    pub usage: Usage,
    pub finish_reason: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_provider_response: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistantMessage {
    pub role: String,
    pub content: String,
}

pub fn render_ask_output(
    format: OutputFormat,
    output: &AskOutput,
    raw_provider_response: bool,
) -> AppResult<String> {
    match format {
        OutputFormat::Line => {
            if raw_provider_response {
                return Err(AppError::new(
                    EXIT_ARGS,
                    "--raw-provider-response does not support --output line",
                ));
            }
            Ok(format!(
                "ok=1 session_id={} provider={} model={} finish_reason={} latency_ms={} input_tokens={} output_tokens={} total_tokens={} content={}",
                quote(&output.session_id),
                quote(&output.provider),
                quote(&output.model),
                quote(&output.finish_reason),
                output.latency_ms,
                output.usage.input_tokens.unwrap_or(0),
                output.usage.output_tokens.unwrap_or(0),
                output.usage.total_tokens.unwrap_or(0),
                quote(&output.message.content),
            ))
        }
        OutputFormat::Text => {
            if raw_provider_response {
                serde_json::to_string_pretty(
                    output
                        .raw_provider_response
                        .as_ref()
                        .ok_or_else(|| AppError::new(EXIT_ARGS, "missing raw provider response"))?,
                )
                .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to render JSON: {err}")))
            } else {
                Ok(output.message.content.clone())
            }
        }
        OutputFormat::Json => serde_json::to_string_pretty(output)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to render JSON: {err}"))),
        OutputFormat::Ndjson => serde_json::to_string(output)
            .map_err(|err| AppError::new(EXIT_ARGS, format!("failed to render JSON: {err}"))),
    }
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_output_uses_single_line() {
        let output = AskOutput {
            ok: true,
            provider: "openai".to_string(),
            model: "gpt4".to_string(),
            session_id: "sess_1".to_string(),
            message: AssistantMessage {
                role: "assistant".to_string(),
                content: "hello\nworld".to_string(),
            },
            usage: Usage {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3),
            },
            finish_reason: "stop".to_string(),
            latency_ms: 10,
            raw_provider_response: None,
        };
        let rendered = render_ask_output(OutputFormat::Line, &output, false).unwrap();
        assert!(!rendered.contains('\n'));
        assert!(rendered.contains("session_id=\"sess_1\""));
    }
}
