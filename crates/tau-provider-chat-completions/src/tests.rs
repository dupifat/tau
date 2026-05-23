use super::*;

fn provider() -> ChatCompletionsProvider {
    ChatCompletionsProvider {
        base_url: "https://api.openai.com/v1".to_owned(),
        api_key: "key".to_owned(),
        models: vec![ChatCompletionsModel {
            id: ModelName::new("gpt-4o"),
            display_name: None,
            context_window: 128_000,
        }],
        extra_body: BTreeMap::new(),
        compat: ChatCompletionsCompat::openai_defaults(),
    }
}

#[test]
fn publishes_configured_models_for_registered_provider() {
    // Built-in provider profiles derive the Tau provider namespace from the
    // profile filename; the Chat Completions backend only turns one registered
    // profile into model publication records.
    let models = models_for_provider(&ProviderName::new("openai"), &provider());

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id.to_string(), "openai/gpt-4o");
    assert!(!models[0].supports_compaction);
}

#[test]
fn provider_with_reasoning_effort_publishes_effort_levels() {
    // Role effort selection is clamped to provider-advertised levels. OpenAI
    // compatible profiles that opt into reasoning_effort must publish the
    // corresponding choices.
    let models = models_for_provider(&ProviderName::new("openai"), &provider());

    assert!(models[0].efforts.contains(&tau_proto::Effort::High));
    assert!(models[0].efforts.contains(&tau_proto::Effort::Off));
}

#[test]
fn tool_result_text_uses_structured_status_headers() {
    // Chat Completions and Responses API providers should expose identical
    // provider-facing text for non-success tool results, so model behavior
    // does not depend on the selected OpenAI-compatible API surface.
    let output = tau_proto::ToolResponse::from_cbor(&tau_proto::CborValue::Text("body".into()));

    assert_eq!(
        tool_result_text(
            ToolResultStatus::Error {
                message: "failed".to_owned(),
            },
            &output,
        ),
        "error: failed\n\nbody",
    );
    assert_eq!(
        tool_result_text(
            ToolResultStatus::Cancelled {
                reason: "stopped".to_owned(),
            },
            &output,
        ),
        "cancelled: stopped\n\n",
    );
}
