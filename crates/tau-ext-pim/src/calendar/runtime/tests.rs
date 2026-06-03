use super::*;
use crate::calendar::config::{
    CalendarAccountConfig, CalendarBackendConfig, CalendarSelectionConfig, ValidatedReadPolicy,
    ValidatedWritePolicy,
};

#[test]
fn list_calendars_reports_flattened_calendar_ids() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());

    let output = engine.list_calendars().expect("list calendars");
    let data = cbor_field(&output, "data").expect("data");

    assert_eq!(cbor_text_field(&output, "command"), Some("list_calendars"));
    assert_eq!(cbor_text_field(&output, "status"), Some("ok"));
    assert_eq!(cbor_text_field(data, "format"), Some(LIST_CALENDARS_FORMAT));
    assert_eq!(
        line_payload(data, "calendars"),
        "feed/main read_only \"Feed\""
    );
}

#[test]
fn omitted_calendar_account_defaults_to_first_enabled_account() {
    // Match email's default-scope behavior so weaker local models that omit
    // a calendar can continue. Calendar read outputs include a flattened
    // calendar id, so the selected default is visible to the model afterwards.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let mut engine = test_engine(temp.path());
    engine.config.accounts.insert(
        "later".to_owned(),
        ValidatedAccount {
            id: "later".to_owned(),
            enable: true,
            display_name: Some("Later".to_owned()),
            backend: Some(ValidatedBackendConfig::IcsFeed {
                url_secret: None,
                url: Some("https://example.test/later.ics".to_owned()),
            }),
            default_calendar: Some("other".to_owned()),
            allowed_calendars: vec!["other".to_owned()],
            timezone: Some("UTC".to_owned()),
        },
    );
    engine.config.account_order.push("later".to_owned());

    let account = engine.single_account(None).expect("default account");
    assert_eq!(account.id, "feed");
    let (account, calendar) = engine.resolve_calendar_arg(None).expect("default calendar");
    assert_eq!(account.id, "feed");
    assert_eq!(calendar, "main");

    let invocation = ToolInvocation {
        command: CalendarCommand::ListEvents,
        args: Some(cbor_map(vec![(
            "start",
            CborValue::Text("2026-05-29".to_owned()),
        )])),
    };
    let result = ok_envelope(
        "list_events",
        "ok",
        cbor_map(vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            ("events", CborValue::Array(Vec::new())),
        ]),
    );
    let entry = engine
        .calendar_log_entry(&invocation, &result)
        .expect("log entry");
    assert_eq!(entry.account.as_deref(), Some("feed"));
    assert_eq!(entry.calendar.as_deref(), Some("main"));
}

#[test]
fn calendar_log_records_tool_reads_and_action_lists_them() {
    // Calendar entries contain sensitive schedule metadata. Tool reads need
    // an audit trail that the user can review without exposing event bodies.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());

    let output = dispatch_test(&engine, command_args("list_calendars", vec![]));
    let data = cbor_field(&output, "data").expect("data");
    assert_eq!(cbor_text_field(data, "format"), Some(LIST_CALENDARS_FORMAT));
    assert_eq!(
        tau_proto::ToolResponse::from_cbor(&output).render(),
        "ok: true\ncommand: list_calendars\nstatus: ok\nformat: calendar_id flags display_name\n\nfeed/main read_only \"Feed\""
    );
    assert_eq!(success_display(&output).args, "list_calendars");

    let log = engine.action_log_last(10).expect("log output");

    assert!(log.contains("Last 1 calendar log entry(s):"), "{log}");
    assert!(log.contains("kind=tool"), "{log}");
    assert!(log.contains("command=list_calendars"), "{log}");
    assert!(log.contains("status=ok"), "{log}");
    assert!(log.contains("items=1"), "{log}");
}

#[test]
fn calendar_success_display_keeps_queued_event_target() {
    // Queued write results are the final model-visible status for default
    // approval policy, so keep the target event and range visible there too.
    let mut change = CalendarChangeApproval::pending("update_event", "google", "primary");
    change.event_id = Some("evt1".to_owned());
    change.start = Some("2026-05-29T10:00:00Z".to_owned());
    change.end = Some("2026-05-29T11:00:00Z".to_owned());

    let result = format_change_queued("change1", &change);

    let display = success_display(&result);
    assert_eq!(display.args, "update_event google/primary event=evt1");
    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-05-29T10:00".to_owned()),
            end: Some("2026-05-29T11:00".to_owned()),
        })
    );
}

#[test]
fn calendar_direct_write_result_uses_flattened_calendar_id() {
    // When write approval is disabled, mutation results are directly visible to
    // the model and must still hide the separate account concept.
    let mut change = CalendarChangeApproval::pending("delete_event", "google", "primary");
    change.event_id = Some("evt1".to_owned());

    let result =
        format_mutation_result_envelope("change1", &change, &CalendarMutationResult::Deleted);
    let data = cbor_field(&result, "data").expect("data");

    assert_eq!(cbor_text_field(data, "calendar"), Some("google/primary"));
    assert!(cbor_text_field(data, "account").is_none());
}
#[test]
fn calendar_initial_display_shows_scope_and_range() {
    // Calendar reads can be slow/networked; keep the live status chip useful by
    // showing the same scope/range information that matters for the result.
    let display = initial_display(&command_args(
        "list_events",
        vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            ("start", CborValue::Text("2026-05-29".to_owned())),
            ("end", CborValue::Text("2026-05-30".to_owned())),
        ],
    ));

    assert_eq!(display.args, "list_events feed/main");
    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-05-29".to_owned()),
            end: Some("2026-05-30".to_owned()),
        })
    );
}

#[test]
fn calendar_display_preserves_non_midnight_range_times() {
    // Date-only and midnight bounds are compacted to dates, but meaningful
    // non-midnight times must remain visible for hourly reads and writes.
    let display = initial_display(&command_args(
        "list_events",
        vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            (
                "start",
                CborValue::Text("2026-05-29T13:30:00-07:00".to_owned()),
            ),
            (
                "end",
                CborValue::Text("2026-05-29T15:00:00-07:00".to_owned()),
            ),
        ],
    ));

    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-05-29T13:30".to_owned()),
            end: Some("2026-05-29T15:00".to_owned()),
        })
    );
}

#[test]
fn calendar_display_does_not_panic_on_non_ascii_date_suffix() {
    // Initial display runs on raw invocation arguments before validation. A
    // value with an ISO-looking date prefix but non-ASCII suffix must not panic
    // while trying to compact it.
    let display = initial_display(&command_args(
        "list_events",
        vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            ("start", CborValue::Text("2026-05-29éééééé".to_owned())),
            ("end", CborValue::Text("2026-05-30Tnot-a-date".to_owned())),
        ],
    ));

    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-05-29éééééé".to_owned()),
            end: Some("2026-05-30Tnot-a-date".to_owned()),
        })
    );
}

#[test]
fn calendar_error_display_keeps_range_separate_from_args() {
    // Error displays use invocation arguments rather than result data. Keep the
    // same range field there so failed ranged calls do not lose context.
    let arguments = command_args(
        "free_busy",
        vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            ("start", CborValue::Text("2026-05-29".to_owned())),
            ("end", CborValue::Text("2026-05-30".to_owned())),
        ],
    );
    let details = cbor_map(vec![("command", CborValue::Text("free_busy".to_owned()))]);

    let display = error_display(&arguments, &details, "boom");

    assert_eq!(display.args, "free_busy feed/main");
    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-05-29".to_owned()),
            end: Some("2026-05-30".to_owned()),
        })
    );
}

#[test]
fn calendar_success_display_keeps_list_events_compact() {
    // List-event display already has generic item stats, so avoid repeating the
    // same count in labelled chips and keep the date range separate from the
    // calendar scope.
    let output = ok_envelope(
        "list_events",
        "ok",
        cbor_map(vec![
            ("calendar", CborValue::Text("proton/main".to_owned())),
            (
                "start",
                CborValue::Text("2026-06-10T00:00:00-07:00".to_owned()),
            ),
            (
                "end",
                CborValue::Text("2026-06-17T00:00:00-07:00".to_owned()),
            ),
            (
                "events",
                CborValue::Array(vec![
                    CborValue::Text("evt1".to_owned()),
                    CborValue::Text("evt2".to_owned()),
                ]),
            ),
            ("returned_events", CborValue::Integer(2.into())),
            ("scanned_events", CborValue::Integer(2.into())),
        ]),
    );

    let display = success_display(&output);

    assert_eq!(display.args, "list_events proton/main");
    assert_eq!(
        display.range,
        Some(ToolUseRange {
            start: Some("2026-06-10".to_owned()),
            end: Some("2026-06-17".to_owned()),
        })
    );
    assert_eq!(display.stats.matches, Some(2));
    assert_eq!(display.stats.lines, None);
    assert_eq!(display.stats.bytes, None);
    assert!(display.info_chips.is_empty());

    let empty_output = ok_envelope(
        "list_events",
        "ok",
        cbor_map(vec![("events", CborValue::Array(Vec::new()))]),
    );
    assert_eq!(success_display(&empty_output).stats.matches, Some(0));
}

#[test]
fn list_events_uses_start_end_range_names_and_rejects_old_names() {
    // Range reads now use the same `start`/`end` names as event payloads,
    // parsed through a command-specific struct. The old time_min/time_max
    // names must fail instead of being accepted as a second vocabulary.
    let invocation = ToolInvocation {
        command: CalendarCommand::ListEvents,
        args: Some(cbor_map(vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            (
                "start",
                CborValue::Text("2026-05-29T00:00:00-07:00".to_owned()),
            ),
            (
                "end",
                CborValue::Text("2026-05-30T00:00:00-07:00".to_owned()),
            ),
        ])),
    };
    let args = parse_invocation_args::<CalendarRangeArgs>(&invocation).expect("range args");
    assert_eq!(args.start.as_deref(), Some("2026-05-29T00:00:00-07:00"));
    assert_eq!(args.end.as_deref(), Some("2026-05-30T00:00:00-07:00"));

    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let output = dispatch_test(
        &engine,
        command_args(
            "list_events",
            vec![
                ("calendar", CborValue::Text("feed/main".to_owned())),
                (
                    "time_min",
                    CborValue::Text("2026-05-29T00:00:00Z".to_owned()),
                ),
            ],
        ),
    );

    assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
    assert_eq!(cbor_text_field(&output, "command"), Some("list_events"));
    let message = cbor_nested_text_field(&output, "error", "message").expect("message");
    assert_eq!(message, "list_events does not accept `time_min`");
}

#[test]
fn free_busy_rejects_title_filter_instead_of_ignoring_it() {
    // `free_busy` should not leak title probing; use `list_events` for title
    // filters.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());

    let output = dispatch_test(
        &engine,
        command_args(
            "free_busy",
            vec![
                ("calendar", CborValue::Text("feed/main".to_owned())),
                ("start", CborValue::Text("2026-05-29".to_owned())),
                ("title", CborValue::Text("tau".to_owned())),
            ],
        ),
    );

    assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
    assert_eq!(cbor_text_field(&output, "command"), Some("free_busy"));
    let message = cbor_nested_text_field(&output, "error", "message").expect("message");
    assert_eq!(
        message,
        "free_busy does not accept `title`; use list_events for title filtering"
    );
}

#[test]
fn calendar_range_args_accept_local_bounds_and_default_end() {
    // Agents often know the date but omit an offset. Range reads should
    // interpret local date/date-time values in the account timezone and
    // stay bounded even when `end` is omitted.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let account = engine.config.accounts.get("feed").expect("account");

    let range = parse_range(
        &CalendarRangeArgs {
            start: Some("2026-05-30T12:34:56".to_owned()),
            ..Default::default()
        },
        account,
    )
    .expect("local datetime range");
    assert_eq!(
        range
            .min
            .expect("min")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format min"),
        "2026-05-30T12:34:56Z"
    );
    assert_eq!(
        range
            .max
            .expect("max")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format max"),
        "2026-06-06T12:34:56Z"
    );

    let range = parse_range(
        &CalendarRangeArgs {
            start: Some("2026-05-30".to_owned()),
            end: Some("2026-05-31".to_owned()),
            ..Default::default()
        },
        account,
    )
    .expect("local date range");
    assert_eq!(
        range
            .min
            .expect("min")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format min"),
        "2026-05-30T00:00:00Z"
    );
    assert_eq!(
        range
            .max
            .expect("max")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format max"),
        "2026-05-31T00:00:00Z"
    );

    let la_start = parse_read_bound("2026-05-30T00:00:00", "start", Some("America/Los_Angeles"))
        .expect("la local start");
    assert_eq!(
        la_start
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format la start"),
        "2026-05-30T00:00:00-07:00"
    );

    let la_fall_start =
        parse_read_bound("2026-10-31T00:00:00", "start", Some("America/Los_Angeles"))
            .expect("la fall start");
    let la_fall_end = default_read_end_bound(
        "2026-10-31T00:00:00",
        la_fall_start,
        Some("America/Los_Angeles"),
    )
    .expect("la fall default end");
    assert_eq!(
        la_fall_end
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format la fall end"),
        "2026-11-07T00:00:00-08:00"
    );
}

#[test]
fn list_events_ignores_blank_title_filter() {
    // Agents may include an empty `title` when they mean "no filter".
    // Treat whitespace-only filters as absent instead of failing the read.
    assert_eq!(
        optional_trimmed_line(Some(" \n\t "), "title").expect("blank title"),
        None
    );
    assert_eq!(
        optional_trimmed_line(Some("  project sync\n"), "title").expect("trimmed title"),
        Some("project sync".to_owned())
    );
}

#[test]
fn calendar_range_args_default_to_recent_week() {
    // Regression coverage for weak models that omit `start` after a calendar
    // error. Missing bounds must remain safe and bounded instead of failing
    // into an auto-retry loop or creating an unbounded read.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let account = engine.config.accounts.get("feed").expect("account");
    let today_before = time::OffsetDateTime::now_utc().date();

    let range = parse_range(&CalendarRangeArgs::default(), account).expect("default range");

    let today_after = time::OffsetDateTime::now_utc().date();
    let min = range.min.expect("min");
    let max = range.max.expect("max");
    let expected_before =
        date_days_before(today_before, DEFAULT_READ_LOOKBACK_DAYS).expect("expected before date");
    let expected_after =
        date_days_before(today_after, DEFAULT_READ_LOOKBACK_DAYS).expect("expected after date");
    assert!(
        min.date() == expected_before || min.date() == expected_after,
        "min {min:?} should default to midnight two days before today"
    );
    assert_eq!(min.time(), time::Time::MIDNIGHT);
    assert_eq!(max - min, time::Duration::days(DEFAULT_READ_WINDOW_DAYS));
}

#[test]
fn calendar_log_prefers_effective_default_range_over_blank_args() {
    // Blank read bounds are treated like omission. The audit log should record
    // the effective default range returned by the tool, not the model's blank
    // raw arguments.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let invocation = ToolInvocation {
        command: CalendarCommand::ListEvents,
        args: Some(cbor_map(vec![
            ("start", CborValue::Text(" \t".to_owned())),
            ("end", CborValue::Text("".to_owned())),
        ])),
    };
    let result = ok_envelope(
        "list_events",
        "ok",
        cbor_map(vec![
            ("calendar", CborValue::Text("feed/main".to_owned())),
            ("start", CborValue::Text("2026-05-30T00:00:00Z".to_owned())),
            ("end", CborValue::Text("2026-06-06T00:00:00Z".to_owned())),
            ("events", CborValue::Array(Vec::new())),
        ]),
    );

    let entry = engine
        .calendar_log_entry(&invocation, &result)
        .expect("log entry");

    assert_eq!(entry.start.as_deref(), Some("2026-05-30T00:00:00Z"));
    assert_eq!(entry.end.as_deref(), Some("2026-06-06T00:00:00Z"));
}

#[test]
fn calendar_log_records_failed_write_attempts_without_payloads() {
    // Write commands are still unsupported, but attempts should be visible
    // in the audit log before mutation approval plumbing is added.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());

    let err = dispatch_test(
        &engine,
        command_args(
            "create_event",
            vec![
                ("calendar", CborValue::Text("feed/main".to_owned())),
                ("title", CborValue::Text("private title".to_owned())),
            ],
        ),
    );
    assert_eq!(cbor_bool_field(&err, "ok"), Some(false));
    let err_text = calendar_error_message(&err);
    assert!(
        err_text.contains("does not support calendar writes"),
        "{err_text}"
    );

    let log = engine.action_log_last(10).expect("log output");

    assert!(log.contains("command=create_event"), "{log}");
    assert!(log.contains("status=invalid_input"), "{log}");
    assert!(log.contains("account=feed"), "{log}");
    assert!(log.contains("calendar=main"), "{log}");
    assert!(!log.contains("private title"), "{log}");
}

#[test]
fn calendar_approve_all_accepts_empty_pending_list() {
    // `/calendar change approve all` should be a valid convenience command
    // even when there is nothing queued.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());

    let output = engine
        .action_change_approve_args(&["all".to_owned()])
        .expect("approve all");

    assert_eq!(output, "No pending calendar changes to approve.");
}

#[test]
fn google_writes_queue_pending_calendar_changes() {
    // Calendar writes can send attendee notifications or alter the user's
    // schedule, so the default policy persists a pending change for review
    // instead of calling Google immediately.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "google".to_owned(),
            enable: true,
            backend: Some(CalendarBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: Some("refresh".to_owned()),
                api_base: None,
            }),
            calendars: CalendarSelectionConfig {
                default: Some("primary".to_owned()),
                allow: vec!["primary".to_owned()],
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let engine = Engine {
        config: cfg.validate().expect("valid config"),
        state: StateStore::open(temp.path().join("state")).expect("state"),
        google: GoogleBackend::new(BTreeMap::new()),
        ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        etags: RefCell::new(BTreeMap::new()),
        last_events: RefCell::new(BTreeMap::new()),
    };

    let output = dispatch_test(
        &engine,
        command_args(
            "create_event",
            vec![
                ("calendar", CborValue::Text("google/primary".to_owned())),
                ("title", CborValue::Text("Team Sync".to_owned())),
                ("start", CborValue::Text("2026-05-28T12:00:00Z".to_owned())),
                ("end", CborValue::Text("2026-05-28T13:00:00Z".to_owned())),
                (
                    "attendees",
                    CborValue::Array(vec![CborValue::Text("a@example.com".to_owned())]),
                ),
            ],
        ),
    );
    let data = cbor_field(&output, "data").expect("data");

    assert_eq!(
        cbor_text_field(&output, "status"),
        Some("approval_required")
    );
    assert_eq!(cbor_text_field(data, "approval_id"), Some("1"));
    let list = engine.action_change_list().expect("change list");
    assert!(list.contains("command=create_event"), "{list}");
    assert!(list.contains("title=Team Sync"), "{list}");
    let open = engine.action_change_open("1").expect("change open");
    assert!(open.contains("attendees: a@example.com"), "{open}");
    assert_eq!(
        engine.action_change_deny("1"),
        Ok("Denied calendar change 1.".to_owned())
    );
}

#[test]
fn create_event_defaults_missing_end() {
    // Small local models often omit `end` even when they identified a
    // concrete start. Queueing a safe default prevents an avoidable retry
    // loop while keeping the pending change visible for user approval.
    let (start, end) = create_event_time_pair(Some("2026-05-28T12:00:00Z"), None, Some("UTC"))
        .expect("default date-time end");
    assert_eq!(start, "2026-05-28T12:00:00Z");
    assert_eq!(end, "2026-05-28T13:00:00Z");

    let (start, end) =
        create_event_time_pair(Some("2026-05-28"), None, Some("UTC")).expect("default all-day end");
    assert_eq!(start, "2026-05-28");
    assert_eq!(end, "2026-05-29");

    let (start, end) = create_event_time_pair(
        Some("2026-05-28T12:00:00"),
        Some("2026-05-28T13:00:00"),
        Some("UTC"),
    )
    .expect("local date-times use account timezone");
    assert_eq!(start, "2026-05-28T12:00:00Z");
    assert_eq!(end, "2026-05-28T13:00:00Z");
}

#[test]
fn google_create_event_queues_pending_change_with_default_end() {
    // Calendar writes are still queued for approval; this only fills in a
    // low-risk default duration when the model omits `end`.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "google".to_owned(),
            enable: true,
            backend: Some(CalendarBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: Some("refresh".to_owned()),
                api_base: None,
            }),
            calendars: CalendarSelectionConfig {
                default: Some("primary".to_owned()),
                allow: vec!["primary".to_owned()],
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let engine = Engine {
        config: cfg.validate().expect("valid config"),
        state: StateStore::open(temp.path().join("state")).expect("state"),
        google: GoogleBackend::new(BTreeMap::new()),
        ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        etags: RefCell::new(BTreeMap::new()),
        last_events: RefCell::new(BTreeMap::new()),
    };

    let output = dispatch_test(
        &engine,
        command_args(
            "create_event",
            vec![
                ("title", CborValue::Text("Team Sync".to_owned())),
                ("start", CborValue::Text("2026-05-28T12:00:00Z".to_owned())),
            ],
        ),
    );
    let data = cbor_field(&output, "data").expect("data");

    assert_eq!(
        cbor_text_field(&output, "status"),
        Some("approval_required")
    );
    assert_eq!(cbor_text_field(data, "approval_id"), Some("1"));
    let open = engine.action_change_open("1").expect("change open");
    assert!(open.contains("start: 2026-05-28T12:00:00Z"), "{open}");
    assert!(open.contains("end: 2026-05-28T13:00:00Z"), "{open}");
}

#[test]
fn google_reads_without_stored_auth_report_auth_error() {
    // Accounts that opt into action-owned OAuth should fail before any
    // network call until `/calendar auth google` stores a refresh token.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "google".to_owned(),
            enable: true,
            backend: Some(CalendarBackendConfig::Google {
                client_id_secret: "client".to_owned(),
                client_secret_secret: None,
                refresh_token_secret: None,
                api_base: None,
            }),
            calendars: CalendarSelectionConfig {
                default: Some("primary".to_owned()),
                allow: vec!["primary".to_owned()],
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    let engine = Engine {
        config: cfg.validate().expect("valid config"),
        state: StateStore::open(temp.path().join("state")).expect("state"),
        google: GoogleBackend::new(BTreeMap::new()),
        ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        etags: RefCell::new(BTreeMap::new()),
        last_events: RefCell::new(BTreeMap::new()),
    };

    let output = dispatch_test(&engine, command_args("list_calendars", vec![]));

    assert_eq!(cbor_bool_field(&output, "ok"), Some(false));
    assert_eq!(
        cbor_nested_text_field(&output, "error", "code"),
        Some("auth_error")
    );
    assert!(
        calendar_error_message(&output).contains("/calendar auth google start google"),
        "{}",
        calendar_error_message(&output)
    );
}

#[test]
fn private_event_details_are_busy_only_by_default() {
    // Provider-private events should not leak summaries or descriptions to
    // the model unless policy explicitly opts into details.
    let account = ValidatedAccount {
        id: "google".to_owned(),
        enable: true,
        display_name: None,
        backend: Some(ValidatedBackendConfig::Google {
            client_id_secret: "client".to_owned(),
            client_secret_secret: None,
            refresh_token_secret: Some("refresh".to_owned()),
            api_base: None,
        }),
        default_calendar: Some("primary".to_owned()),
        allowed_calendars: vec!["primary".to_owned()],
        timezone: Some("UTC".to_owned()),
    };
    let event = BackendEvent::Google(GoogleEvent {
        id: "evt".to_owned(),
        etag: Some("abc".to_owned()),
        i_cal_uid: None,
        summary: "Private title".to_owned(),
        description: Some("private body".to_owned()),
        location: Some("Secret room".to_owned()),
        start: "2026-05-28T12:00:00Z".to_owned(),
        end: "2026-05-28T13:00:00Z".to_owned(),
        status: Some("confirmed".to_owned()),
        visibility: Some("private".to_owned()),
        transparency: None,
        organizer: Some("org@example.com".to_owned()),
        attendees: vec!["a@example.com".to_owned()],
        self_response_status: None,
        recurring: false,
    });
    let policy = ValidatedPolicy {
        read: ValidatedReadPolicy {
            private_events: PrivateEventsPolicy::BusyOnly,
            descriptions: DescriptionPolicy::ApprovedOnly,
        },
        write: ValidatedWritePolicy {
            require_approval: true,
            max_attendees: 50,
        },
    };

    let detail = format_event_detail(&policy, &account, "primary", &event).join("\n");

    assert!(detail.contains("summary (private)"), "{detail}");
    assert!(
        detail.contains("flags read_only,private_busy_only"),
        "{detail}"
    );
    assert!(!detail.contains("Private title"), "{detail}");
    assert!(!detail.contains("private body"), "{detail}");
    assert!(!detail.contains("Secret room"), "{detail}");
}

#[test]
fn google_event_details_hide_etag_from_agent() {
    // Google read responses keep ETags internally for conditional writes;
    // model-visible event details should stay focused on user data.
    let account = ValidatedAccount {
        id: "google".to_owned(),
        enable: true,
        display_name: None,
        backend: Some(ValidatedBackendConfig::Google {
            client_id_secret: "client".to_owned(),
            client_secret_secret: None,
            refresh_token_secret: Some("refresh".to_owned()),
            api_base: None,
        }),
        default_calendar: Some("primary".to_owned()),
        allowed_calendars: vec!["primary".to_owned()],
        timezone: Some("UTC".to_owned()),
    };
    let event = BackendEvent::Google(GoogleEvent {
        id: "evt".to_owned(),
        etag: Some("abc".to_owned()),
        i_cal_uid: Some("uid@example.com".to_owned()),
        summary: "Team Sync".to_owned(),
        description: Some("line 1\nline 2".to_owned()),
        location: Some("Room 1".to_owned()),
        start: "2026-05-28T12:00:00Z".to_owned(),
        end: "2026-05-28T13:00:00Z".to_owned(),
        status: Some("confirmed".to_owned()),
        visibility: None,
        transparency: None,
        organizer: Some("org@example.com".to_owned()),
        attendees: vec!["a@example.com".to_owned(), "b@example.com".to_owned()],
        self_response_status: None,
        recurring: true,
    });
    let policy = ValidatedPolicy {
        read: ValidatedReadPolicy {
            private_events: PrivateEventsPolicy::BusyOnly,
            descriptions: DescriptionPolicy::Always,
        },
        write: ValidatedWritePolicy {
            require_approval: true,
            max_attendees: 50,
        },
    };

    assert_eq!(
        format_event_detail(&policy, &account, "primary", &event).join("\n"),
        "calendar google/primary\nevent_id evt\nstart 2026-05-28T12:00:00Z\nend 2026-05-28T13:00:00Z\nflags read_only,recurring\nsummary Team_Sync\nuid uid@example.com\nstatus confirmed\nlocation Room_1\norganizer org@example.com\nattendees a@example.com,b@example.com\ndescription line 1 line 2"
    );
}

#[test]
fn calendar_change_detail_hides_internal_etag() {
    // Approval details may be echoed into agent-visible transcripts. Keep
    // provider precondition tokens internal even though pending changes
    // persist them for later approval execution.
    let mut change = CalendarChangeApproval::pending("update_event", "google", "primary");
    change.id = "1".to_owned();
    change.event_id = Some("evt".to_owned());
    change.etag = Some("abc".to_owned());
    change.title = Some("Team Sync".to_owned());

    let detail = format_change_detail(&change);

    assert!(detail.contains("event_id: evt"), "{detail}");
    assert!(!detail.contains("etag"), "{detail}");
    assert!(!detail.contains("abc"), "{detail}");
}

#[test]
fn google_event_etag_cache_is_cleared_by_missing_provider_etag() {
    // A malformed or degraded provider response without an ETag must fail
    // closed instead of leaving an older precondition token active.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let account = ValidatedAccount {
        id: "google".to_owned(),
        enable: true,
        display_name: None,
        backend: Some(ValidatedBackendConfig::Google {
            client_id_secret: "client".to_owned(),
            client_secret_secret: None,
            refresh_token_secret: Some("refresh".to_owned()),
            api_base: None,
        }),
        default_calendar: Some("primary".to_owned()),
        allowed_calendars: vec!["primary".to_owned()],
        timezone: Some("UTC".to_owned()),
    };
    let mut event = BackendEvent::Google(GoogleEvent {
        id: "evt".to_owned(),
        etag: Some("abc".to_owned()),
        i_cal_uid: None,
        summary: "Team Sync".to_owned(),
        description: None,
        location: None,
        start: "2026-05-28T12:00:00Z".to_owned(),
        end: "2026-05-28T13:00:00Z".to_owned(),
        status: Some("confirmed".to_owned()),
        visibility: None,
        transparency: None,
        organizer: None,
        attendees: Vec::new(),
        self_response_status: None,
        recurring: false,
    });
    let mut change = CalendarChangeApproval::pending("update_event", "google", "primary");
    change.event_id = Some("evt".to_owned());

    engine.remember_event_etag(&account, "primary", &event);
    assert_eq!(
        engine.cached_etag_for_change(&change).expect("cached etag"),
        "abc"
    );

    if let BackendEvent::Google(event) = &mut event {
        event.etag = None;
    }
    engine.remember_event_etag(&account, "primary", &event);

    assert!(engine.cached_etag_for_change(&change).is_err());
}

#[test]
fn title_filter_matches_visible_event_summaries() {
    let events = vec![
        BackendEvent::Google(GoogleEvent {
            id: "evt1".to_owned(),
            etag: None,
            i_cal_uid: None,
            summary: "Tau Testing Party".to_owned(),
            description: None,
            location: None,
            start: "2026-05-28".to_owned(),
            end: "2026-05-29".to_owned(),
            status: Some("confirmed".to_owned()),
            visibility: None,
            transparency: None,
            organizer: None,
            attendees: Vec::new(),
            self_response_status: None,
            recurring: false,
        }),
        BackendEvent::Google(GoogleEvent {
            id: "evt2".to_owned(),
            etag: None,
            i_cal_uid: None,
            summary: "Lunch".to_owned(),
            description: None,
            location: None,
            start: "2026-05-28".to_owned(),
            end: "2026-05-29".to_owned(),
            status: Some("confirmed".to_owned()),
            visibility: None,
            transparency: None,
            organizer: None,
            attendees: Vec::new(),
            self_response_status: None,
            recurring: false,
        }),
    ];
    let policy = ValidatedPolicy {
        read: ValidatedReadPolicy {
            private_events: PrivateEventsPolicy::BusyOnly,
            descriptions: DescriptionPolicy::Always,
        },
        write: ValidatedWritePolicy {
            require_approval: true,
            max_attendees: 50,
        },
    };

    let filtered = filtered_events(&policy, &events, Some("tau"));

    assert_eq!(filtered.len(), 1);
    assert_eq!(event_id(filtered[0]), "evt1");
}

#[test]
fn read_event_can_use_single_recent_event_for_agent() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let engine = test_engine(temp.path());
    let agent_id = AgentId::from("agent");
    let account = ValidatedAccount {
        id: "feed".to_owned(),
        enable: true,
        display_name: None,
        backend: None,
        default_calendar: Some("main".to_owned()),
        allowed_calendars: vec!["main".to_owned()],
        timezone: Some("UTC".to_owned()),
    };
    let event = BackendEvent::Ics(IcsEvent {
        id: "evt".to_owned(),
        uid: "uid".to_owned(),
        summary: "Tau Testing Party".to_owned(),
        description: None,
        location: None,
        start: "2026-05-28".to_owned(),
        end: "2026-05-29".to_owned(),
        start_utc: None,
        end_utc: None,
        status: None,
        organizer: None,
        attendees: Vec::new(),
        private: false,
        recurring: false,
        time_unparsed: false,
    });
    engine.remember_visible_events(&agent_id, &account, "main", &[&event]);

    let event_id = engine
        .resolve_read_event_id(&agent_id, &account, "main", None)
        .expect("single recent event id");

    assert_eq!(event_id, "evt");
}

#[test]
fn natural_date_bounds_are_accepted_without_configured_timezone() {
    parse_range(
        &CalendarRangeArgs {
            start: Some("2 days".to_owned()),
            ..Default::default()
        },
        &ValidatedAccount {
            id: "google".to_owned(),
            enable: true,
            display_name: None,
            backend: None,
            default_calendar: Some("primary".to_owned()),
            allowed_calendars: vec!["primary".to_owned()],
            timezone: None,
        },
    )
    .expect("natural date without configured timezone");
}

#[test]
fn duplicate_account_ids_are_rejected() {
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![
            CalendarAccountConfig {
                id: "work".to_owned(),
                ..Default::default()
            },
            CalendarAccountConfig {
                id: "work".to_owned(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let err = match cfg.validate() {
        Ok(_) => panic!("duplicate ids should fail"),
        Err(err) => err,
    };
    assert!(err.contains("duplicate calendar account id"), "{err}");

    let slash_cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "work/account".to_owned(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let slash_err = slash_cfg.validate().err().expect("slash id rejected");
    assert!(
        slash_err.contains("calendar account id must not contain `/`"),
        "{slash_err}"
    );
}

#[test]
fn ics_feed_requires_exactly_one_url_source() {
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "feed".to_owned(),
            backend: Some(CalendarBackendConfig::IcsFeed {
                url_secret: None,
                url: None,
            }),
            ..Default::default()
        }],
        ..Default::default()
    };

    let err = match cfg.validate() {
        Ok(_) => panic!("missing feed source should fail"),
        Err(err) => err,
    };
    assert!(err.contains("requires exactly one"), "{err}");
}

fn test_engine(root: &std::path::Path) -> Engine {
    let cfg = CalendarExtensionConfig {
        enable: true,
        accounts: vec![CalendarAccountConfig {
            id: "feed".to_owned(),
            enable: true,
            display_name: Some("Feed".to_owned()),
            backend: Some(CalendarBackendConfig::IcsFeed {
                url_secret: None,
                url: Some("https://example.test/calendar.ics".to_owned()),
            }),
            calendars: CalendarSelectionConfig {
                default: Some("main".to_owned()),
                allow: vec!["main".to_owned()],
            },
            timezone: Some("UTC".to_owned()),
        }],
        ..Default::default()
    };
    Engine {
        config: cfg.validate().expect("valid config"),
        state: StateStore::open(root.join("state")).expect("state"),
        google: GoogleBackend::new(BTreeMap::new()),
        ics_feed: IcsFeedBackend::new(BTreeMap::new()),
        etags: RefCell::new(BTreeMap::new()),
        last_events: RefCell::new(BTreeMap::new()),
    }
}

fn dispatch_test(engine: &Engine, arguments: CborValue) -> CborValue {
    engine.dispatch(&arguments, &AgentId::from("test-agent"))
}

fn command_args(command: &str, args: Vec<(&str, CborValue)>) -> CborValue {
    cbor_map(vec![
        ("command", CborValue::Text(command.to_owned())),
        ("args", cbor_map(args)),
    ])
}

fn cbor_map(entries: Vec<(&str, CborValue)>) -> CborValue {
    CborValue::Map(
        entries
            .into_iter()
            .map(|(key, value)| (CborValue::Text(key.to_owned()), value))
            .collect(),
    )
}

fn line_payload(data: &CborValue, field: &str) -> String {
    cbor_array_field(data, field)
        .expect("line array")
        .iter()
        .map(|value| match value {
            CborValue::Text(value) => value.as_str(),
            _ => panic!("line array contains non-text value"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
