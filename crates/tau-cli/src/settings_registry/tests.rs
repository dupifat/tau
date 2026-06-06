/// `/set show-messages` is registry-driven, so the registry must expose all
/// documented modes for parsing and completion.
#[test]
fn show_messages_values_are_registered() {
    let setting = super::find("show-messages").expect("show-messages setting");
    let values: Vec<_> = setting.values.iter().map(|value| value.value).collect();

    assert_eq!(
        values,
        vec![
            "none",
            "self-summary",
            "self-full",
            "all-summary",
            "all-full"
        ]
    );
}

/// `/set show-ui-io` is a boolean status-bar toggle, so it should use the
/// standard true/false values that completion and validation expect.
#[test]
fn show_ui_io_values_are_registered() {
    let setting = super::find("show-ui-io").expect("show-ui-io setting");
    let values: Vec<_> = setting.values.iter().map(|value| value.value).collect();

    assert_eq!(values, vec!["true", "false"]);
}
