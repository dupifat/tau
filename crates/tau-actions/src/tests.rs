use super::*;

fn string_arg(name: &str) -> ActionArg {
    ActionArg {
        name: name.to_owned(),
        description: format!("{name} value"),
        required: true,
        suggestions: Vec::new(),
        kind: ActionArgKind::String,
    }
}

fn rest_arg(name: &str) -> ActionArg {
    ActionArg {
        name: name.to_owned(),
        description: format!("{name} value"),
        required: true,
        suggestions: Vec::new(),
        kind: ActionArgKind::RestString,
    }
}

fn leaf(name: &str, action_id: &str, args: Vec<ActionArg>) -> ActionCommand {
    ActionCommand {
        name: name.to_owned(),
        description: format!("{name} action"),
        action_id: Some(action_id.to_owned()),
        args,
        children: Vec::new(),
    }
}

fn group(name: &str, children: Vec<ActionCommand>) -> ActionCommand {
    ActionCommand {
        name: name.to_owned(),
        description: format!("{name} commands"),
        action_id: None,
        args: Vec::new(),
        children,
    }
}

fn email_schema() -> ActionSchema {
    ActionSchema {
        version: ACTION_SCHEMA_VERSION,
        roots: vec![ActionCommand {
            name: "/email".to_owned(),
            description: "Review email approvals".to_owned(),
            action_id: None,
            args: Vec::new(),
            children: vec![
                group(
                    "out",
                    vec![
                        leaf("list", "email.out.list", Vec::new()),
                        leaf("approve", "email.out.approve", vec![string_arg("id")]),
                    ],
                ),
                group(
                    "draft",
                    vec![leaf("note", "email.draft.note", vec![rest_arg("text")])],
                ),
            ],
        }],
    }
}

#[test]
fn schema_validation_accepts_nested_executable_leaves() {
    let ids = email_schema()
        .executable_action_ids()
        .expect("schema should validate");

    assert_eq!(
        ids,
        vec![
            "email.out.list".to_owned(),
            "email.out.approve".to_owned(),
            "email.draft.note".to_owned(),
        ]
    );
}

#[test]
fn schema_validation_rejects_duplicate_action_ids() {
    let schema = ActionSchema {
        version: ACTION_SCHEMA_VERSION,
        roots: vec![ActionCommand {
            name: "/email".to_owned(),
            description: String::new(),
            action_id: None,
            args: Vec::new(),
            children: vec![
                leaf("one", "email.same", Vec::new()),
                leaf("two", "email.same", Vec::new()),
            ],
        }],
    };

    let error = schema.validate().expect_err("duplicate id should fail");
    assert!(error.message().contains("duplicate action_id `email.same`"));
}

#[test]
fn schema_validation_rejects_invalid_root_names() {
    let schema = ActionSchema {
        version: ACTION_SCHEMA_VERSION,
        roots: vec![leaf("email", "email.root", Vec::new())],
    };

    let error = schema
        .validate()
        .expect_err("root without slash should fail");
    assert!(error.message().contains("invalid root action name"));
}

#[test]
fn parse_nested_action_with_positional_string_arg() {
    let parsed = email_schema()
        .parse_line("/email out approve abc-123")
        .expect("action line should parse");

    assert_eq!(parsed.action_id, "email.out.approve");
    assert_eq!(parsed.argv, vec!["abc-123".to_owned()]);
    assert_eq!(
        parsed.named_args.get("id"),
        Some(&ParsedArgValue::String("abc-123".to_owned()))
    );
}

#[test]
fn parse_rest_string_joins_remaining_tokens() {
    let parsed = email_schema()
        .parse_line("/email draft note hello from tau")
        .expect("rest action line should parse");

    assert_eq!(parsed.action_id, "email.draft.note");
    assert_eq!(parsed.argv, vec!["hello from tau".to_owned()]);
    assert_eq!(
        parsed.named_args.get("text"),
        Some(&ParsedArgValue::String("hello from tau".to_owned()))
    );
}

#[test]
fn parse_unknown_root_is_distinguishable() {
    let error = email_schema()
        .parse_line("/missing out approve abc")
        .expect_err("unknown root should fail");

    assert!(error.is_unknown_root());
}

#[test]
fn parse_incomplete_namespace_reports_child_usage() {
    let error = email_schema()
        .parse_line("/email out")
        .expect_err("namespace should not execute");

    assert_eq!(error.kind(), &ParseErrorKind::IncompleteCommand);
    assert!(error.to_string().contains("list|approve"));
}

#[test]
fn parse_missing_arg_reports_leaf_usage() {
    let error = email_schema()
        .parse_line("/email out approve")
        .expect_err("missing id should fail");

    assert_eq!(error.kind(), &ParseErrorKind::InvalidArguments);
    assert_eq!(error.usage(), Some("/email out approve <id>"));
}
