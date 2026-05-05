use tau_config::settings::PromptCacheRetention;
use tau_proto::Effort;

use super::*;

#[test]
fn into_tool_calls_drops_nameless_accumulator_artifacts() {
    // The streaming paths eagerly extend `tool_calls` from
    // argument-delta events so the index stays addressable. If
    // the matching name-carrying event never arrives (partial
    // item, reasoning noise, stream cancellation), the slot stays
    // nameless. Shipping it downstream would trigger a visible
    // `invalid_tool` rejection in the harness and confuse the
    // model, which never intended a second tool call.
    let state = StreamState {
        text: String::new(),
        tool_calls: vec![
            ToolCallAccumulator {
                id: String::new(),
                name: String::new(),
                arguments_json: String::from("{\"stray\": \"delta\"}"),
            },
            ToolCallAccumulator {
                id: "call_real".into(),
                name: "shell".into(),
                arguments_json: "{\"command\":\"ls\"}".into(),
            },
        ],
        input_tokens: None,
        cached_tokens: None,
        thinking: None,
    };

    let calls = state.into_tool_calls();
    assert_eq!(calls.len(), 1, "nameless accumulator must be dropped");
    assert_eq!(calls[0].id.as_str(), "call_real");
    assert_eq!(calls[0].name.as_str(), "shell");
}

#[test]
fn build_request_includes_prompt_cache_fields_when_configured() {
    let config = OpenAiConfig {
        base_url: "https://api.openai.com/v1".into(),
        api_key: "test".into(),
        model_id: "gpt-5".into(),
        supports_reasoning_effort: false,
        prompt_cache_key: Some("tau:seed".into()),
        prompt_cache_retention: Some(PromptCacheRetention::Extended24h),
    };
    let request = PromptPayload {
        system_prompt: "system",
        messages: &[],
        tools: &[],
        effort: Effort::Off,
        thinking_summary: tau_proto::ThinkingSummary::Off,
    };

    let body = serde_json::to_value(build_request(&config, &request, true)).expect("serialize");
    let prompt_cache_key = body["prompt_cache_key"].as_str().expect("prompt_cache_key");

    assert_eq!(prompt_cache_key, "tau:seed");
    assert_eq!(body["prompt_cache_retention"], "24h");
}

#[test]
fn build_request_omits_prompt_cache_fields_without_seed_or_retention() {
    let config = OpenAiConfig {
        base_url: "https://example.com/v1".into(),
        api_key: "test".into(),
        model_id: "local".into(),
        supports_reasoning_effort: false,
        prompt_cache_key: None,
        prompt_cache_retention: None,
    };
    let request = PromptPayload {
        system_prompt: "system",
        messages: &[],
        tools: &[],
        effort: Effort::Off,
        thinking_summary: tau_proto::ThinkingSummary::Off,
    };

    let body = serde_json::to_value(build_request(&config, &request, true)).expect("serialize");
    let object = body.as_object().expect("request object");

    assert!(!object.contains_key("prompt_cache_key"));
    assert!(!object.contains_key("prompt_cache_retention"));
}
