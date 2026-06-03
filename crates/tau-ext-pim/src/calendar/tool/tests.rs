use super::*;

#[test]
fn calendar_schema_hides_timezone_and_has_command_conditionals() {
    // Weak local models need command-specific split-tool schemas rather than
    // prose-only guidance. Keep timezone out of model-visible args, but keep
    // range starts optional because the runtime supplies a bounded default.
    let schemas = calendar_tool_specs();
    let list_events_schema = schemas
        .iter()
        .find(|spec| spec.name.as_str() == "calendar_search")
        .and_then(|spec| spec.parameters.as_ref())
        .expect("search parameters");
    let properties = list_events_schema
        .pointer("/properties")
        .and_then(serde_json::Value::as_object)
        .expect("list events properties");

    assert!(!properties.contains_key("timezone"));
    assert!(
        list_events_schema
            .pointer("/required")
            .is_some_and(|required| {
                required
                    .as_array()
                    .is_some_and(|required| required.is_empty())
            })
    );

    let search_title_description = list_events_schema
        .pointer("/properties/title/description")
        .and_then(serde_json::Value::as_str)
        .expect("search title description");
    assert!(search_title_description.contains("substring filter"));
    assert!(!search_title_description.contains("calendar_create"));

    let create_schema = schemas
        .iter()
        .find(|spec| spec.name.as_str() == "calendar_create")
        .and_then(|spec| spec.parameters.as_ref())
        .expect("create parameters");
    let create_title_description = create_schema
        .pointer("/properties/title/description")
        .and_then(serde_json::Value::as_str)
        .expect("create title description");
    assert_eq!(create_title_description, "Event title.");

    let free_busy_schema = schemas
        .iter()
        .find(|spec| spec.name.as_str() == "calendar_free_busy")
        .and_then(|spec| spec.parameters.as_ref())
        .expect("free busy parameters");
    assert!(
        free_busy_schema
            .pointer("/required")
            .is_some_and(|required| {
                required
                    .as_array()
                    .is_some_and(|required| required.is_empty())
            })
    );

    let update_schema = schemas
        .iter()
        .find(|spec| spec.name.as_str() == "calendar_update")
        .and_then(|spec| spec.parameters.as_ref())
        .expect("update parameters");
    assert_eq!(
        update_schema.pointer("/required").expect("required"),
        &serde_json::json!(["event_id", "field", "new_value"])
    );
    assert!(update_schema.pointer("/dependentRequired/end").is_none());
    assert_eq!(
        update_schema
            .pointer("/properties/field/enum")
            .expect("fields"),
        &serde_json::json!(["title", "description", "location", "start", "attendees"])
    );
    assert!(update_schema.pointer("/anyOf").is_none());
}
