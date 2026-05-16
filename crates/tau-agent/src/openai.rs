//! OpenAI-compatible chat completions client.
//!
//! Works with any endpoint speaking the OpenAI chat completions API:
//! llama.cpp, vLLM, Ollama, OpenAI, etc.

use std::io::BufRead;

use serde::{Deserialize, Serialize};
use tau_proto::{ContentPart, ContextItem, ContextRole, ToolDefinition};

use crate::common::{
    LlmError, PromptPayload, StreamState, cbor_to_json, effort_wire, mix_originator_into_cache_key,
    prompt_cache_key_for, verbosity_wire,
};

/// Configuration for the OpenAI-compatible backend.
#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub api_key: String,
    pub model_id: String,
    /// Whether the provider's API accepts a `reasoning_effort` field.
    /// Read from `models.json5` provider compat flags.
    pub supports_reasoning_effort: bool,
    /// Whether the provider's API accepts a top-level `verbosity`
    /// field (OpenAI Chat Completions on GPT-5+).
    pub supports_verbosity: bool,
    /// Whether this provider accepts the `prompt_cache_key` field.
    /// The wire key is derived per `(base_url, session_id)`, then
    /// split by extension name for extension-originated turns.
    pub supports_prompt_cache_key: bool,
    /// Provider-side prompt cache retention policy, when configured.
    pub prompt_cache_retention: Option<tau_config::settings::PromptCacheRetention>,
    /// Whether to use llama.cpp Chat Completions cache extensions.
    pub supports_llama_cpp_cache: bool,
}

/// Calls the chat completions endpoint with streaming. Invokes the
/// callback with the accumulated text and (optional) thinking
/// summary on each content delta. Returns the final state.
///
/// Some OpenAI-compatible servers, including llama.cpp with reasoning
/// format enabled, stream model thoughts as `delta.reasoning_content`.
/// Those deltas are accumulated into Tau's `thinking` channel so a
/// model that exhausts its token budget mid-think still produces a
/// visible update instead of looking idle.
pub fn chat_completion_stream(
    config: &OpenAiConfig,
    request: &PromptPayload<'_>,
    on_update: &mut impl FnMut(&str, Option<&str>),
) -> Result<StreamState, LlmError> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));

    let body = build_request(config, request, true);
    tracing::debug!(
        target: crate::LOG_TARGET,
        model = %config.model_id,
        base_url = %config.base_url,
        supports_llama_cpp_cache = config.supports_llama_cpp_cache,
        cache_prompt = body.cache_prompt,
        prompt_cache_key = body.prompt_cache_key.as_deref(),
        prompt_cache_retention = body.prompt_cache_retention,
        "chat completions request cache settings"
    );
    let body_str = serde_json::to_string(&body).map_err(LlmError::Json)?;

    let response = tau_provider::oauth::proxy_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", config.api_key))
        .send_string(&body_str)
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                LlmError::HttpStatus(code, body)
            }
            other => LlmError::Http(Box::new(other)),
        })?;

    let reader = std::io::BufReader::new(response.into_reader());
    let mut state = StreamState::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(LlmError::Io)?;

        // SSE format: lines starting with "data: "
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };

        tracing::trace!(
            target: crate::LOG_TARGET,
            session_id = %request.session_id,
            line_index,
            sse_data = %data,
            "chat completions raw SSE data"
        );

        if data == "[DONE]" {
            break;
        }

        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(error) => {
                tracing::trace!(
                    target: crate::LOG_TARGET,
                    session_id = %request.session_id,
                    line_index,
                    %error,
                    sse_data = %data,
                    "ignored malformed chat completions SSE data"
                );
                continue;
            }
        };

        if tracing::enabled!(target: crate::LOG_TARGET, tracing::Level::TRACE)
            && (chunk.usage.is_some()
                || chunk.tokens_cached.is_some()
                || chunk.tokens_evaluated.is_some())
        {
            let usage_prompt_tokens = chunk.usage.as_ref().and_then(|usage| usage.prompt_tokens);
            let usage_cached_tokens = chunk.usage.as_ref().and_then(|usage| {
                usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            });
            tracing::trace!(
                target: crate::LOG_TARGET,
                usage_prompt_tokens,
                usage_cached_tokens,
                llama_tokens_cached = chunk.tokens_cached,
                llama_tokens_evaluated = chunk.tokens_evaluated,
                choices = chunk.choices.len(),
                "chat completions stream usage chunk"
            );
        }

        if let Some(usage) = chunk.usage.as_ref() {
            if state.input_tokens.is_none() {
                state.input_tokens = usage.prompt_tokens;
            }
            if state.cached_tokens.is_none() {
                state.cached_tokens = usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens);
            }
            if state.output_tokens.is_none() {
                state.output_tokens = usage.completion_tokens;
            }
        }
        if state.input_tokens.is_none() {
            state.input_tokens = chunk.tokens_evaluated;
        }
        if state.cached_tokens.is_none() {
            state.cached_tokens = chunk.tokens_cached;
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };

        apply_stream_choice(&mut state, choice, on_update);
    }

    Ok(state)
}

fn apply_stream_choice(
    state: &mut StreamState,
    choice: StreamChoice,
    on_update: &mut impl FnMut(&str, Option<&str>),
) {
    // Accumulate reasoning content before visible content. llama.cpp
    // emits Qwen `<think>` blocks this way when `--reasoning-format`
    // is enabled.
    if let Some(reasoning_content) = choice.delta.reasoning_content {
        state
            .thinking
            .get_or_insert_with(String::new)
            .push_str(&reasoning_content);
        on_update(&state.text, state.thinking.as_deref());
    }

    // Accumulate text content into a synthesized message item.
    if let Some(content) = choice.delta.content {
        state.append_chat_message_delta(&content);
        on_update(&state.text, state.thinking.as_deref());
    }

    // Accumulate tool calls into synthesized tool-call items. Chat
    // Completions exposes `tool_calls[N]` indexes rather than provider
    // output item indexes, so `StreamState` maps them onto item slots.
    if let Some(tool_calls) = choice.delta.tool_calls {
        for tc in tool_calls {
            let index = tc.index.unwrap_or(0) as usize;
            let tool_type = if matches!(tc.r#type.as_deref(), Some("custom")) || tc.custom.is_some()
            {
                tau_proto::ToolType::Custom
            } else {
                tau_proto::ToolType::Function
            };
            let acc = state.chat_tool_call_at_mut(index, tool_type);
            if let Some(id) = tc.id {
                acc.id = id;
            }
            if let Some(function) = tc.function {
                if let Some(name) = function.name {
                    acc.name = name;
                }
                if let Some(args) = function.arguments {
                    acc.arguments_json.push_str(&args);
                }
            }
            if let Some(custom) = tc.custom {
                if let Some(name) = custom.name {
                    acc.name = name;
                }
                if let Some(input) = custom.input {
                    acc.arguments_json.push_str(&input);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request building
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CompletionRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    /// Explicit per OpenAI Chat Completions; default is `true` server-side
    /// on api.openai.com, but some compatible servers default to `false`.
    /// Set only when at least one tool is offered so requests without tools
    /// stay minimal.
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// Standard OpenAI Chat Completions reasoning control. Sent only
    /// when the provider supports it and the user picked a non-Off
    /// effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    /// Top-level GPT-5 output-verbosity hint (`low`/`medium`/`high`).
    /// Sent only when the provider's `supports_verbosity` flag is on;
    /// otherwise omitted so non-GPT-5 endpoints don't reject the
    /// request with an unknown-argument error.
    #[serde(skip_serializing_if = "Option::is_none")]
    verbosity: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_retention: Option<&'static str>,
    /// Optional upstream service tier (`fast` for Fast mode, `flex` for
    /// lower-priority service).
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_prompt: Option<bool>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

fn build_request(
    config: &OpenAiConfig,
    request: &PromptPayload<'_>,
    stream: bool,
) -> CompletionRequest {
    let mut messages = Vec::new();

    if !request.system_prompt.is_empty() {
        messages.push(ApiMessage {
            role: "system".to_owned(),
            content: Some(request.system_prompt.to_owned()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    let mut context_index = 0;
    while context_index < request.context_items.len() {
        if let ContextItem::ToolCall(_) = &request.context_items[context_index] {
            let mut tool_calls = Vec::new();
            while let Some(ContextItem::ToolCall(call)) = request.context_items.get(context_index) {
                tool_calls.push(convert_tool_call(call));
                context_index += 1;
            }
            push_tool_calls(&mut messages, tool_calls);
            continue;
        }

        convert_context_item(&request.context_items[context_index], &mut messages);
        context_index += 1;
    }

    let tools: Vec<serde_json::Value> = request.tools.iter().map(convert_tool_definition).collect();
    let tool_choice = match (request.tool_choice, tools.is_empty()) {
        // Harness-forced no-tools-this-turn: send explicit `none` even
        // with tools declared so the cache prefix matches the parent
        // turn while the model is told not to call anything.
        (tau_proto::ToolChoice::None, _) => Some("none".to_owned()),
        (tau_proto::ToolChoice::Auto, false) => Some("auto".to_owned()),
        (tau_proto::ToolChoice::Auto, true) => None,
    };
    let parallel_tool_calls = (!tools.is_empty()).then_some(true);

    let reasoning_effort = if config.supports_reasoning_effort {
        effort_wire(request.params.effort)
    } else {
        None
    };
    let verbosity = if config.supports_verbosity {
        Some(verbosity_wire(request.params.verbosity))
    } else {
        None
    };
    let prompt_cache_key = config
        .supports_prompt_cache_key
        .then(|| prompt_cache_key_for(&config.base_url, request.session_id));
    let prompt_cache_key = mix_originator_into_cache_key(
        prompt_cache_key.as_deref(),
        request.originator,
        request.share_user_cache_key,
    );
    let prompt_cache_retention = config
        .prompt_cache_retention
        .map(tau_config::settings::PromptCacheRetention::as_wire);

    CompletionRequest {
        model: config.model_id.clone(),
        messages,
        tools,
        tool_choice,
        parallel_tool_calls,
        stream,
        stream_options: stream.then_some(StreamOptions {
            include_usage: true,
        }),
        reasoning_effort,
        verbosity,
        prompt_cache_key,
        prompt_cache_retention,
        service_tier: request
            .params
            .service_tier
            .map(tau_proto::ServiceTier::as_wire),
        cache_prompt: config.supports_llama_cpp_cache.then_some(true),
    }
}

fn convert_context_item(item: &ContextItem, out: &mut Vec<ApiMessage>) {
    match item {
        ContextItem::Message(msg) => match msg.role {
            ContextRole::System | ContextRole::Developer => {}
            ContextRole::User => {
                let text = msg
                    .content
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => text.as_str(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    out.push(ApiMessage {
                        role: "user".to_owned(),
                        content: Some(text),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
            ContextRole::Assistant => {
                let text_parts = msg
                    .content
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => text.clone(),
                    })
                    .collect::<Vec<_>>();
                if !text_parts.is_empty() {
                    out.push(ApiMessage {
                        role: "assistant".to_owned(),
                        content: Some(text_parts.join("\n")),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        },
        ContextItem::ToolCall(call) => push_tool_calls(out, vec![convert_tool_call(call)]),
        ContextItem::ToolResult(result) => {
            let content = match &result.output {
                tau_proto::CborValue::Text(text) => text.clone(),
                other => serde_json::to_string(&cbor_to_json(other)).unwrap_or_default(),
            };
            out.push(ApiMessage {
                role: "tool".to_owned(),
                content: Some(content),
                tool_calls: None,
                tool_call_id: Some(result.call_id.to_string()),
                name: None,
            });
        }
        ContextItem::Reasoning(_)
        | ContextItem::Compaction(_)
        | ContextItem::UnknownProviderItem(_) => {}
    }
}

fn push_tool_calls(out: &mut Vec<ApiMessage>, tool_calls: Vec<serde_json::Value>) {
    if tool_calls.is_empty() {
        return;
    }

    out.push(ApiMessage {
        role: "assistant".to_owned(),
        content: None,
        tool_calls: Some(tool_calls),
        tool_call_id: None,
        name: None,
    });
}

fn convert_tool_call(call: &tau_proto::ToolCallItem) -> serde_json::Value {
    match call.tool_type {
        tau_proto::ToolType::Function => {
            let args_json = cbor_to_json(&call.arguments);
            serde_json::json!({
                "id": call.call_id,
                "type": "function",
                "function": {
                    "name": call.name.as_str(),
                    "arguments": serde_json::to_string(&args_json).unwrap_or_default(),
                }
            })
        }
        tau_proto::ToolType::Custom => serde_json::json!({
            "id": call.call_id,
            "type": "custom",
            "custom": {
                "name": call.name.as_str(),
                "input": match &call.arguments {
                    tau_proto::CborValue::Text(text) => text.clone(),
                    other => serde_json::to_string(&cbor_to_json(other)).unwrap_or_default(),
                },
            }
        }),
    }
}

fn convert_tool_definition(tool: &ToolDefinition) -> serde_json::Value {
    let model_visible_name = tool.model_visible_name.as_ref().unwrap_or(&tool.name);
    match tool.tool_type {
        tau_proto::ToolType::Function => serde_json::json!({
            "type": "function",
            "function": {
                "name": model_visible_name.as_str(),
                "description": tool.description,
                "parameters": tool.parameters,
            }
        }),
        tau_proto::ToolType::Custom => {
            let mut custom = serde_json::Map::new();
            custom.insert(
                "name".to_owned(),
                serde_json::Value::String(model_visible_name.as_str().to_owned()),
            );
            if let Some(description) = &tool.description {
                custom.insert(
                    "description".to_owned(),
                    serde_json::Value::String(description.clone()),
                );
            }
            if let Some(format) = &tool.format {
                custom.insert("format".to_owned(), serialize_tool_format(format));
            }
            serde_json::json!({
                "type": "custom",
                "custom": custom,
            })
        }
    }
}

fn serialize_tool_format(format: &tau_proto::ToolFormat) -> serde_json::Value {
    match format {
        tau_proto::ToolFormat::Text => serde_json::json!({
            "type": "text",
        }),
        tau_proto::ToolFormat::Grammar { syntax, definition } => serde_json::json!({
            "type": "grammar",
            "grammar": {
                "syntax": match syntax {
                    tau_proto::ToolGrammarSyntax::Lark => "lark",
                    tau_proto::ToolGrammarSyntax::Regex => "regex",
                },
                "definition": definition,
            }
        }),
    }
}

// ---------------------------------------------------------------------------
// Streaming response parsing
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    tokens_cached: Option<u64>,
    #[serde(default)]
    tokens_evaluated: Option<u64>,
}

#[derive(Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Deserialize)]
struct StreamDelta {
    content: Option<String>,
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    index: Option<u32>,
    id: Option<String>,
    #[serde(rename = "type")]
    r#type: Option<String>,
    function: Option<StreamFunction>,
    custom: Option<StreamCustom>,
}

#[derive(Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct StreamCustom {
    name: Option<String>,
    input: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[cfg(test)]
mod tests;
