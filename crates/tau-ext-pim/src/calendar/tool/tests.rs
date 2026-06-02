use super::*;

#[test]
fn calendar_schema_hides_timezone_and_has_command_conditionals() {
    // Weak local models need command-specific schema constraints rather
    // than prose-only guidance. Keep timezone out of model-visible args, but
    // keep range starts optional because the runtime supplies a bounded default.
    let schema = calendar_tool_spec().parameters.expect("parameters");
    let args_properties = schema
        .pointer("/properties/args/properties")
        .and_then(serde_json::Value::as_object)
        .expect("args properties");

    assert!(!args_properties.contains_key("timezone"));
    let rules = schema
        .get("allOf")
        .and_then(serde_json::Value::as_array)
        .expect("command rules");
    assert!(7 < rules.len());

    let list_events_rule = rule_for_command(rules, "list_events");
    assert!(list_events_rule.pointer("/then/required").is_none());
    assert!(
        list_events_rule
            .pointer("/then/properties/args/required")
            .is_none()
    );

    let free_busy_rule = rule_for_command(rules, "free_busy");
    assert!(free_busy_rule.pointer("/then/required").is_none());
    assert!(
        free_busy_rule
            .pointer("/then/properties/args/required")
            .is_none()
    );
}

fn rule_for_command<'a>(rules: &'a [serde_json::Value], command: &str) -> &'a serde_json::Value {
    rules
        .iter()
        .find(|rule| {
            rule.pointer("/if/properties/command/const")
                .and_then(serde_json::Value::as_str)
                == Some(command)
        })
        .expect("command rule")
}
