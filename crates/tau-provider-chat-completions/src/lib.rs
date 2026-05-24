//! OpenAI-compatible Chat Completions backend helpers.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};

use serde::{Deserialize, Serialize};
use tau_proto::{
    ContentPart, ContextItem, ContextRole, Event, Frame, FrameWriter, ModelId, ModelName,
    ProviderBackend, ProviderBackendKind, ProviderBackendTransport, ProviderModelInfo,
    ProviderName, ProviderResponseFinished, ProviderResponseUpdated, ProviderStopReason,
    ProviderTokenUsage, SessionPromptId, ThinkingSummary, ToolCallItem, ToolChoice, ToolDefinition,
    ToolResponseHeader, ToolResultStatus, ToolType,
};

const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// One Chat Completions-compatible provider entry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionsProvider {
    /// Base URL without `/chat/completions`, e.g. `https://api.openai.com/v1`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// Bearer token sent in the `Authorization` header. Empty for local or
    /// otherwise keyless providers.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Model ids to publish under this provider namespace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ChatCompletionsModel>,
    /// Extra JSON fields merged into each Chat Completions request body.
    ///
    /// Local and OpenAI-compatible servers use non-standard knobs for reasoning
    /// (`chat_template_kwargs`, `reasoning`, `enable_thinking`, etc.). Keeping
    /// this map provider-scoped lets users opt into those fields without Tau
    /// needing a compatibility switch for every backend.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_body: BTreeMap<String, serde_json::Value>,
    /// Explicit provider compatibility switches.
    #[serde(default)]
    pub compat: ChatCompletionsCompat,
}

/// One model published by a Chat Completions-compatible provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionsModel {
    /// Upstream model id sent in the `model` request field.
    pub id: ModelName,
    /// Optional UI display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Context window size surfaced to the harness.
    #[serde(default = "default_context_window")]
    pub context_window: u64,
}

/// Compatibility switches for OpenAI-compatible Chat Completions APIs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompletionsCompat {
    /// Whether to send `stream_options: { include_usage: true }`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stream_options: bool,
    /// Whether to send `parallel_tool_calls` when tools are declared.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parallel_tool_calls: bool,
    /// Whether to send OpenAI's `prompt_cache_key` field.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prompt_cache_key: bool,
    /// Whether to send `reasoning_effort`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reasoning_effort: bool,
    /// Whether to use `max_completion_tokens` for future output caps.
    #[serde(default, skip_serializing_if = "is_false")]
    pub max_completion_tokens: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

const fn default_context_window() -> u64 {
    DEFAULT_CONTEXT_WINDOW
}

impl ChatCompletionsCompat {
    /// Compatibility switches for OpenAI's public Chat Completions API.
    #[must_use]
    pub const fn openai_defaults() -> Self {
        Self {
            stream_options: true,
            parallel_tool_calls: true,
            prompt_cache_key: true,
            reasoning_effort: true,
            max_completion_tokens: true,
        }
    }
}

fn run_prompt<W: Write>(
    session_prompt_id: &SessionPromptId,
    prompt: &tau_proto::SessionPromptCreated,
    provider: ResolvedProvider,
    model: ChatCompletionsModel,
    writer: &mut FrameWriter<W>,
) -> ProviderResponseFinished {
    let mut on_update = |text: &str, thinking: Option<&str>| {
        let _ = writer.write_frame(&Frame::Event(Event::ProviderResponseUpdated(
            ProviderResponseUpdated {
                session_prompt_id: session_prompt_id.clone(),
                text: text.to_owned(),
                thinking: thinking.map(str::to_owned),
                originator: prompt.originator.clone(),
            },
        )));
        let _ = writer.flush();
    };
    match chat_completions_stream(&provider, &model, prompt, &mut on_update) {
        Ok(state) => finish_success(session_prompt_id, prompt, &provider, state),
        Err(error) => finish_error(session_prompt_id, prompt, &provider, error),
    }
}

/// Runs one prompt against a registered Chat Completions-compatible provider
/// profile.
pub fn run_prompt_for_provider<W: Write>(
    session_prompt_id: &SessionPromptId,
    prompt: &tau_proto::SessionPromptCreated,
    provider: &ChatCompletionsProvider,
    model: &ChatCompletionsModel,
    writer: &mut FrameWriter<W>,
) -> ProviderResponseFinished {
    run_prompt(
        session_prompt_id,
        prompt,
        ResolvedProvider {
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            extra_body: provider.extra_body.clone(),
            compat: provider.compat,
        },
        model.clone(),
        writer,
    )
}

#[derive(Clone)]
struct ResolvedProvider {
    base_url: String,
    api_key: String,
    extra_body: BTreeMap<String, serde_json::Value>,
    compat: ChatCompletionsCompat,
}

/// Returns model publication records for one Chat Completions-compatible
/// provider profile.
pub fn models_for_provider(
    provider_name: &ProviderName,
    provider: &ChatCompletionsProvider,
) -> Vec<ProviderModelInfo> {
    provider
        .models
        .iter()
        .map(|model| ProviderModelInfo {
            id: ModelId::new(provider_name.clone(), model.id.clone()),
            display_name: model.display_name.clone(),
            default_affinity: 0,
            context_window: model.context_window,
            efforts: model_efforts(provider.compat),
            verbosities: vec![tau_proto::Verbosity::Medium],
            thinking_summaries: vec![ThinkingSummary::Off],
            supports_compaction: false,
        })
        .collect()
}

fn model_efforts(compat: ChatCompletionsCompat) -> Vec<tau_proto::Effort> {
    if compat.reasoning_effort {
        vec![
            tau_proto::Effort::Off,
            tau_proto::Effort::Minimal,
            tau_proto::Effort::Low,
            tau_proto::Effort::Medium,
            tau_proto::Effort::High,
            tau_proto::Effort::XHigh,
        ]
    } else {
        vec![tau_proto::Effort::Off]
    }
}

#[derive(Debug)]
enum LlmError {
    Http(Box<ureq::Error>),
    HttpStatus(u16, String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(f, "HTTP error: {error}"),
            Self::HttpStatus(code, body) => write!(f, "HTTP {code}: {body}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
        }
    }
}

struct StreamState {
    text: String,
    thinking: String,
    pending_content: String,
    in_think_tag: bool,
    tool_calls: HashMap<usize, ToolCallAccumulator>,
    input_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    output_tokens: Option<u64>,
    stop_reason: ProviderStopReason,
}

impl StreamState {
    fn new() -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            pending_content: String::new(),
            in_think_tag: false,
            tool_calls: HashMap::new(),
            input_tokens: None,
            cached_tokens: None,
            output_tokens: None,
            stop_reason: ProviderStopReason::EndTurn,
        }
    }

    fn output_items(&self) -> Vec<ContextItem> {
        let mut items = Vec::new();
        if !self.text.is_empty() {
            items.push(assistant_text_item(self.text.clone()));
        }
        let mut tool_calls = self.tool_calls.iter().collect::<Vec<_>>();
        tool_calls.sort_by_key(|(index, _)| **index);
        for (_, call) in tool_calls {
            if !call.name.is_empty() {
                items.push(ContextItem::ToolCall(ToolCallItem {
                    call_id: call.id.clone().into(),
                    name: tau_proto::ToolName::new(call.name.clone()),
                    tool_type: ToolType::Function,
                    arguments: serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .map(|value| json_to_cbor(&value))
                        .unwrap_or(tau_proto::CborValue::Null),
                }));
            }
        }
        items
    }

    fn usage(&self) -> Option<ProviderTokenUsage> {
        let input = self.input_tokens.unwrap_or(0);
        let cached = self.cached_tokens.unwrap_or(0);
        let output = self.output_tokens.unwrap_or(0);
        if input == 0 && cached == 0 && output == 0 {
            None
        } else {
            Some(ProviderTokenUsage {
                model: None,
                prompt_sent_tokens: input,
                prompt_cached_tokens: cached,
                response_received_tokens: output,
                stats: Default::default(),
            })
        }
    }
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

fn chat_completions_stream(
    provider: &ResolvedProvider,
    model: &ChatCompletionsModel,
    prompt: &tau_proto::SessionPromptCreated,
    on_update: &mut impl FnMut(&str, Option<&str>),
) -> Result<StreamState, LlmError> {
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let body = build_request(provider, model, prompt);
    let body_str = serde_json::to_string(&body).map_err(LlmError::Json)?;
    let mut request = tau_provider::oauth::proxy_agent()
        .post(&url)
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream");
    if !provider.api_key.trim().is_empty() {
        request = request.set("Authorization", &format!("Bearer {}", provider.api_key));
    }
    let response = request
        .send_string(&body_str)
        .map_err(|error| match error {
            ureq::Error::Status(code, response) => {
                LlmError::HttpStatus(code, response.into_string().unwrap_or_default())
            }
            other => LlmError::Http(Box::new(other)),
        })?;

    let mut state = StreamState::new();
    let reader = BufReader::new(response.into_reader());
    for line in reader.lines() {
        let line = line.map_err(LlmError::Io)?;
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let event: serde_json::Value = match serde_json::from_str(data) {
            Ok(event) => event,
            Err(_) => continue,
        };
        apply_event(&mut state, &event, on_update);
    }
    flush_pending_content(&mut state, on_update);
    Ok(state)
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(flatten)]
    extra_body: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

fn build_request(
    provider: &ResolvedProvider,
    model: &ChatCompletionsModel,
    prompt: &tau_proto::SessionPromptCreated,
) -> ChatRequest {
    let mut messages = Vec::new();
    if !prompt.system_prompt.trim().is_empty() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": prompt.system_prompt,
        }));
    }
    for item in &prompt.context_items {
        append_context_item(item, &mut messages);
    }
    let tools = prompt
        .tools
        .iter()
        .filter_map(convert_tool_definition)
        .collect::<Vec<_>>();
    let tool_choice = match (prompt.tool_choice, tools.is_empty()) {
        (ToolChoice::None, _) => Some("none"),
        (ToolChoice::Auto, false) => Some("auto"),
        (ToolChoice::Auto, true) => None,
    };
    ChatRequest {
        model: model.id.as_str().to_owned(),
        messages,
        stream: true,
        stream_options: provider.compat.stream_options.then_some(StreamOptions {
            include_usage: true,
        }),
        parallel_tool_calls: (provider.compat.parallel_tool_calls && !tools.is_empty())
            .then_some(true),
        prompt_cache_key: provider
            .compat
            .prompt_cache_key
            .then(|| format!("tau:{}", prompt.session_id)),
        reasoning_effort: provider
            .compat
            .reasoning_effort
            .then(|| effort_wire(prompt.model_params.effort)),
        extra_body: provider.extra_body.clone(),
        tools,
        tool_choice,
    }
}

fn append_context_item(item: &ContextItem, messages: &mut Vec<serde_json::Value>) {
    match item {
        ContextItem::Message(message) => {
            let text = message_text(message);
            if message.role == ContextRole::User && text.trim().is_empty() {
                return;
            }
            if text.is_empty() {
                return;
            }
            messages.push(serde_json::json!({
                "role": role_wire(&message.role),
                "content": text,
            }));
        }
        ContextItem::ToolCall(call) => {
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call.call_id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": cbor_to_json(&call.arguments).to_string(),
                    }
                }]
            }));
        }
        ContextItem::ToolResult(result) => {
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": tool_result_text(result.status.clone(), &result.output),
            }));
        }
        ContextItem::Reasoning(_)
        | ContextItem::Compaction(_)
        | ContextItem::UnknownProviderItem(_) => {}
    }
}

fn message_text(message: &tau_proto::MessageItem) -> String {
    let mut text = String::new();
    for part in &message.content {
        match part {
            ContentPart::Text { text: part } => text.push_str(part),
        }
    }
    text
}

fn role_wire(role: &ContextRole) -> &'static str {
    match role {
        ContextRole::System => "system",
        ContextRole::Developer => "system",
        ContextRole::User => "user",
        ContextRole::Assistant => "assistant",
    }
}

fn tool_result_text(status: ToolResultStatus, output: &tau_proto::ToolResponse) -> String {
    match status {
        ToolResultStatus::Success => output.render(),
        ToolResultStatus::Error { message } => {
            let mut response = output.clone();
            response.headers.insert(
                0,
                ToolResponseHeader {
                    key: "error".to_owned(),
                    value: message,
                },
            );
            response.render()
        }
        ToolResultStatus::Cancelled { reason } => tau_proto::ToolResponse {
            raw: tau_proto::CborValue::Null,
            headers: vec![ToolResponseHeader {
                key: "cancelled".to_owned(),
                value: reason,
            }],
            body: String::new(),
        }
        .render(),
    }
}

fn convert_tool_definition(tool: &ToolDefinition) -> Option<serde_json::Value> {
    if tool.tool_type != ToolType::Function {
        return None;
    }
    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.model_visible_name.as_ref().unwrap_or(&tool.name),
            "description": tool.description,
            "parameters": tool.parameters,
        }
    }))
}

fn apply_event(
    state: &mut StreamState,
    event: &serde_json::Value,
    on_update: &mut impl FnMut(&str, Option<&str>),
) {
    if let Some(usage) = event.get("usage") {
        capture_usage(state, usage);
    }
    let Some(choice) = event["choices"]
        .as_array()
        .and_then(|choices| choices.first())
    else {
        return;
    };
    let delta = &choice["delta"];
    let mut changed = false;
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(reasoning) = delta[key].as_str()
            && !reasoning.is_empty()
        {
            state.thinking.push_str(reasoning);
            changed = true;
        }
    }
    if let Some(content) = delta["content"].as_str()
        && !content.is_empty()
    {
        changed |= append_content_delta(state, content);
    }
    if changed {
        on_update(&state.text, thinking_for_update(state));
    }
    if let Some(tool_calls) = delta["tool_calls"].as_array() {
        for tool_call in tool_calls {
            let index = tool_call["index"].as_u64().unwrap_or(0) as usize;
            let entry = state.tool_calls.entry(index).or_default();
            if let Some(id) = tool_call["id"].as_str()
                && !id.is_empty()
            {
                entry.id = id.to_owned();
            }
            let function = &tool_call["function"];
            if let Some(name) = function["name"].as_str()
                && !name.is_empty()
            {
                entry.name = name.to_owned();
            }
            if let Some(arguments) = function["arguments"].as_str() {
                entry.arguments.push_str(arguments);
            }
        }
    }
    match choice["finish_reason"].as_str() {
        Some("tool_calls") => state.stop_reason = ProviderStopReason::ToolCalls,
        Some("stop") => state.stop_reason = ProviderStopReason::EndTurn,
        _ => {}
    }
}

fn append_content_delta(state: &mut StreamState, content: &str) -> bool {
    state.pending_content.push_str(content);
    let mut changed = false;
    loop {
        if state.pending_content.is_empty() {
            return changed;
        }
        if state.in_think_tag {
            if let Some(index) = state.pending_content.find("</think>") {
                state.thinking.push_str(&state.pending_content[..index]);
                state.pending_content.drain(..index + "</think>".len());
                state.in_think_tag = false;
                changed = true;
                continue;
            }
            let keep = partial_tag_suffix_len(&state.pending_content, "</think>");
            let emit_len = state.pending_content.len() - keep;
            if emit_len == 0 {
                return changed;
            }
            state.thinking.push_str(&state.pending_content[..emit_len]);
            state.pending_content.drain(..emit_len);
            return true;
        }

        if let Some(index) = state.pending_content.find("<think>") {
            state.text.push_str(&state.pending_content[..index]);
            state.pending_content.drain(..index + "<think>".len());
            state.in_think_tag = true;
            changed = true;
            continue;
        }
        let keep = partial_tag_suffix_len(&state.pending_content, "<think>");
        let emit_len = state.pending_content.len() - keep;
        if emit_len == 0 {
            return changed;
        }
        state.text.push_str(&state.pending_content[..emit_len]);
        state.pending_content.drain(..emit_len);
        return true;
    }
}

fn partial_tag_suffix_len(text: &str, tag: &str) -> usize {
    let mut keep = 0;
    for len in 1..tag.len() {
        if text.ends_with(&tag[..len]) {
            keep = len;
        }
    }
    keep
}

fn flush_pending_content(state: &mut StreamState, on_update: &mut impl FnMut(&str, Option<&str>)) {
    if state.pending_content.is_empty() {
        return;
    }
    if state.in_think_tag {
        state.thinking.push_str(&state.pending_content);
    } else {
        state.text.push_str(&state.pending_content);
    }
    state.pending_content.clear();
    on_update(&state.text, thinking_for_update(state));
}

fn thinking_for_update(state: &StreamState) -> Option<&str> {
    (!state.thinking.is_empty()).then_some(state.thinking.as_str())
}

fn capture_usage(state: &mut StreamState, usage: &serde_json::Value) {
    state.input_tokens = usage["prompt_tokens"].as_u64();
    state.output_tokens = usage["completion_tokens"].as_u64();
    state.cached_tokens = usage["prompt_tokens_details"]["cached_tokens"].as_u64();
}

fn finish_success(
    session_prompt_id: &SessionPromptId,
    prompt: &tau_proto::SessionPromptCreated,
    provider: &ResolvedProvider,
    state: StreamState,
) -> ProviderResponseFinished {
    ProviderResponseFinished {
        session_prompt_id: session_prompt_id.clone(),
        target_agent_id: None,
        output_items: state.output_items(),
        stop_reason: state.stop_reason,
        originator: prompt.originator.clone(),
        usage: state.usage(),
        backend: Some(backend_descriptor(provider)),
        provider_response_id: None,
        ws_pool_delta: None,
    }
}

fn finish_error(
    session_prompt_id: &SessionPromptId,
    prompt: &tau_proto::SessionPromptCreated,
    provider: &ResolvedProvider,
    error: LlmError,
) -> ProviderResponseFinished {
    ProviderResponseFinished {
        session_prompt_id: session_prompt_id.clone(),
        target_agent_id: None,
        output_items: vec![assistant_text_item(format!("LLM error: {error}"))],
        stop_reason: ProviderStopReason::Error,
        originator: prompt.originator.clone(),
        usage: None,
        backend: Some(backend_descriptor(provider)),
        provider_response_id: None,
        ws_pool_delta: None,
    }
}

fn backend_descriptor(provider: &ResolvedProvider) -> ProviderBackend {
    ProviderBackend {
        kind: ProviderBackendKind::ChatCompletions,
        base_url: provider.base_url.clone(),
        transport: ProviderBackendTransport::HttpSse,
        stale_chain_fallback: false,
    }
}

fn assistant_text_item(text: impl Into<String>) -> ContextItem {
    ContextItem::Message(tau_proto::MessageItem {
        role: ContextRole::Assistant,
        content: vec![ContentPart::Text { text: text.into() }],
        phase: None,
    })
}

fn effort_wire(effort: tau_proto::Effort) -> &'static str {
    match effort {
        tau_proto::Effort::Off => "none",
        tau_proto::Effort::Minimal => "minimal",
        tau_proto::Effort::Low => "low",
        tau_proto::Effort::Medium => "medium",
        tau_proto::Effort::High => "high",
        tau_proto::Effort::XHigh => "high",
    }
}

fn cbor_to_json(value: &tau_proto::CborValue) -> serde_json::Value {
    match value {
        tau_proto::CborValue::Null => serde_json::Value::Null,
        tau_proto::CborValue::Bool(v) => serde_json::Value::Bool(*v),
        tau_proto::CborValue::Integer(v) => {
            let n: i128 = (*v).into();
            serde_json::json!(n)
        }
        tau_proto::CborValue::Float(v) => serde_json::Number::from_f64(*v)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        tau_proto::CborValue::Text(v) => serde_json::Value::String(v.clone()),
        tau_proto::CborValue::Bytes(bytes) => serde_json::Value::Array(
            bytes
                .iter()
                .map(|byte| serde_json::Value::Number((*byte).into()))
                .collect(),
        ),
        tau_proto::CborValue::Array(items) => {
            serde_json::Value::Array(items.iter().map(cbor_to_json).collect())
        }
        tau_proto::CborValue::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (key, value) in entries {
                let key = match key {
                    tau_proto::CborValue::Text(text) => text.clone(),
                    other => serde_json::to_string(&cbor_to_json(other)).unwrap_or_default(),
                };
                map.insert(key, cbor_to_json(value));
            }
            serde_json::Value::Object(map)
        }
        tau_proto::CborValue::Tag(_, inner) => cbor_to_json(inner),
        _ => serde_json::Value::Null,
    }
}

fn json_to_cbor(value: &serde_json::Value) -> tau_proto::CborValue {
    match value {
        serde_json::Value::Null => tau_proto::CborValue::Null,
        serde_json::Value::Bool(v) => tau_proto::CborValue::Bool(*v),
        serde_json::Value::Number(v) => {
            if let Some(n) = v.as_i64() {
                tau_proto::CborValue::Integer(n.into())
            } else if let Some(n) = v.as_u64() {
                tau_proto::CborValue::Integer(n.into())
            } else if let Some(n) = v.as_f64() {
                tau_proto::CborValue::Float(n)
            } else {
                tau_proto::CborValue::Null
            }
        }
        serde_json::Value::String(v) => tau_proto::CborValue::Text(v.clone()),
        serde_json::Value::Array(items) => {
            tau_proto::CborValue::Array(items.iter().map(json_to_cbor).collect())
        }
        serde_json::Value::Object(map) => tau_proto::CborValue::Map(
            map.iter()
                .map(|(key, value)| (tau_proto::CborValue::Text(key.clone()), json_to_cbor(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests;
