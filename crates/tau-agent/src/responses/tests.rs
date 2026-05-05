use tau_config::settings::PromptCacheRetention;
use tau_proto::Effort;

use super::*;

#[test]
fn build_request_includes_prompt_cache_fields_when_configured() {
    let config = ResponsesConfig {
        base_url: "https://chatgpt.com/backend-api".into(),
        api_key: "test".into(),
        model_id: "gpt-5-codex".into(),
        account_id: None,
        supports_reasoning_effort: false,
        supports_reasoning_summary: false,
        prompt_cache_key: Some("tau:seed".into()),
        prompt_cache_retention: Some(PromptCacheRetention::InMemory),
    };
    let request = PromptPayload {
        system_prompt: "system",
        messages: &[],
        tools: &[],
        effort: Effort::Off,
        thinking_summary: tau_proto::ThinkingSummary::Off,
    };

    let body = serde_json::to_value(build_request(&config, &request)).expect("serialize");
    let prompt_cache_key = body["prompt_cache_key"].as_str().expect("prompt_cache_key");

    assert_eq!(prompt_cache_key, "tau:seed");
    assert_eq!(body["prompt_cache_retention"], "in_memory");
}

#[test]
fn build_request_omits_prompt_cache_fields_without_seed_or_retention() {
    let config = ResponsesConfig {
        base_url: "https://chatgpt.com/backend-api".into(),
        api_key: "test".into(),
        model_id: "gpt-5-codex".into(),
        account_id: None,
        supports_reasoning_effort: false,
        supports_reasoning_summary: false,
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

    let body = serde_json::to_value(build_request(&config, &request)).expect("serialize");
    let object = body.as_object().expect("request object");

    assert!(!object.contains_key("prompt_cache_key"));
    assert!(!object.contains_key("prompt_cache_retention"));
}
