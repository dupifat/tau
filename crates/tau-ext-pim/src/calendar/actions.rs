use tau_proto::{ACTION_SCHEMA_VERSION, ActionArg, ActionArgKind, ActionCommand, ActionSchema};

/// Return the `/calendar` action schema.
pub fn calendar_action_schema() -> ActionSchema {
    fn string_arg(name: &str, description: &str) -> ActionArg {
        ActionArg {
            name: name.to_owned(),
            description: description.to_owned(),
            required: true,
            kind: ActionArgKind::String,
        }
    }
    fn rest_string_arg(name: &str, description: &str) -> ActionArg {
        ActionArg {
            name: name.to_owned(),
            description: description.to_owned(),
            required: true,
            kind: ActionArgKind::RestString,
        }
    }
    fn optional_integer_arg(name: &str, description: &str) -> ActionArg {
        ActionArg {
            name: name.to_owned(),
            description: description.to_owned(),
            required: false,
            kind: ActionArgKind::Integer,
        }
    }

    ActionSchema {
        version: ACTION_SCHEMA_VERSION,
        roots: vec![ActionCommand {
            name: "/calendar".to_owned(),
            description: "Review calendar logs and pending calendar changes".to_owned(),
            action_id: None,
            args: Vec::new(),
            children: vec![
                ActionCommand {
                    name: "log".to_owned(),
                    description: "Calendar activity log".to_owned(),
                    action_id: None,
                    args: Vec::new(),
                    children: vec![ActionCommand {
                        name: "last".to_owned(),
                        description: "Show recent calendar log entries".to_owned(),
                        action_id: Some("calendar.log.last".to_owned()),
                        args: vec![optional_integer_arg(
                            "number",
                            "Maximum number of log entries to show",
                        )],
                        children: Vec::new(),
                    }],
                },
                ActionCommand {
                    name: "change".to_owned(),
                    description: "Pending calendar changes".to_owned(),
                    action_id: None,
                    args: Vec::new(),
                    children: vec![
                        ActionCommand {
                            name: "list".to_owned(),
                            description: "List pending calendar changes".to_owned(),
                            action_id: Some("calendar.change.list".to_owned()),
                            args: Vec::new(),
                            children: Vec::new(),
                        },
                        ActionCommand {
                            name: "open".to_owned(),
                            description: "Open a pending calendar change".to_owned(),
                            action_id: Some("calendar.change.open".to_owned()),
                            args: vec![string_arg("id", "Pending change id")],
                            children: Vec::new(),
                        },
                        ActionCommand {
                            name: "approve".to_owned(),
                            description: "Approve one or more pending calendar changes".to_owned(),
                            action_id: Some("calendar.change.approve".to_owned()),
                            args: vec![rest_string_arg("ids", "Pending change id(s)")],
                            children: Vec::new(),
                        },
                        ActionCommand {
                            name: "deny".to_owned(),
                            description: "Deny one or more pending calendar changes".to_owned(),
                            action_id: Some("calendar.change.deny".to_owned()),
                            args: vec![rest_string_arg("ids", "Pending change id(s)")],
                            children: Vec::new(),
                        },
                    ],
                },
            ],
        }],
    }
}
