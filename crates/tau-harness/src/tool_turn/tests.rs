use tau_proto::{BackgroundSupport, CborValue, ToolExecutionMode};

use super::*;

fn cid(value: &str) -> AgentId {
    value.into()
}

fn call(id: &str) -> AgentToolCall {
    AgentToolCall {
        id: id.into(),
        name: ToolName::new("tool"),
        tool_type: ToolType::Function,
        arguments: CborValue::Null,
        display: None,
    }
}

fn push(machine: &mut ToolTurnMachine, cid: &AgentId, id: &str, mode: ToolExecutionMode) {
    machine.push(
        cid.clone(),
        call(id),
        mode,
        BackgroundSupport::Never,
        ToolTurnLockScope::Normal,
    );
}

fn push_scoped(
    machine: &mut ToolTurnMachine,
    cid: &AgentId,
    id: &str,
    mode: ToolExecutionMode,
    lock_scope: ToolTurnLockScope,
) {
    machine.push(
        cid.clone(),
        call(id),
        mode,
        BackgroundSupport::Never,
        lock_scope,
    );
}

fn pop_id(machine: &mut ToolTurnMachine) -> Option<String> {
    machine
        .pop_dispatchable(Instant::now())
        .map(|(pending, _)| pending.invocation.id.as_str().to_owned())
}

#[test]
fn shared_calls_from_same_conversation_dispatch_together() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "a", ToolExecutionMode::Shared);
    push(&mut machine, &conv, "b", ToolExecutionMode::Shared);

    assert_eq!(pop_id(&mut machine).as_deref(), Some("a"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("b"));
    assert_eq!(machine.in_flight_len(), 2);
}

#[test]
fn update_runs_with_shared_but_not_another_update() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "shared", ToolExecutionMode::Shared);
    push(&mut machine, &conv, "update-a", ToolExecutionMode::Update);
    push(&mut machine, &conv, "update-b", ToolExecutionMode::Update);
    push(
        &mut machine,
        &conv,
        "shared-behind",
        ToolExecutionMode::Shared,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("update-a"));
    assert_eq!(pop_id(&mut machine), None);
    assert_eq!(machine.pending_len(), 2);
    assert_eq!(
        machine
            .pending(0)
            .expect("pending call at index 0")
            .invocation
            .id
            .as_str(),
        "update-b"
    );
    assert_eq!(
        machine
            .pending(1)
            .expect("pending call at index 1")
            .invocation
            .id
            .as_str(),
        "shared-behind"
    );
}

#[test]
fn exclusive_waits_while_same_conversation_update_is_in_flight() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "update", ToolExecutionMode::Update);
    push(
        &mut machine,
        &conv,
        "exclusive",
        ToolExecutionMode::Exclusive,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("update"));
    assert_eq!(pop_id(&mut machine), None);
    machine.mark_complete(&ToolCallId::from("update"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("exclusive"));
}

#[test]
fn exclusive_waits_while_same_conversation_shared_is_in_flight() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "shared", ToolExecutionMode::Shared);
    push(
        &mut machine,
        &conv,
        "exclusive",
        ToolExecutionMode::Exclusive,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert_eq!(pop_id(&mut machine), None);
    assert_eq!(
        machine
            .pending(0)
            .expect("pending call at index 0")
            .invocation
            .id
            .as_str(),
        "exclusive"
    );
}

#[test]
fn shared_behind_blocked_exclusive_is_not_skipped_for_same_conversation() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(
        &mut machine,
        &conv,
        "shared-in-flight",
        ToolExecutionMode::Shared,
    );
    push(
        &mut machine,
        &conv,
        "exclusive",
        ToolExecutionMode::Exclusive,
    );
    push(
        &mut machine,
        &conv,
        "shared-behind",
        ToolExecutionMode::Shared,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared-in-flight"));
    assert_eq!(pop_id(&mut machine), None);
    assert_eq!(machine.pending_len(), 2);
    assert_eq!(
        machine
            .pending(0)
            .expect("pending call at index 0")
            .invocation
            .id
            .as_str(),
        "exclusive"
    );
    assert_eq!(
        machine
            .pending(1)
            .expect("pending call at index 1")
            .invocation
            .id
            .as_str(),
        "shared-behind"
    );
}

#[test]
fn different_conversations_progress_independently() {
    let mut machine = ToolTurnMachine::default();
    let a = cid("a");
    let b = cid("b");
    push(&mut machine, &a, "a-shared", ToolExecutionMode::Shared);
    push(
        &mut machine,
        &a,
        "a-exclusive",
        ToolExecutionMode::Exclusive,
    );
    push(
        &mut machine,
        &b,
        "b-exclusive",
        ToolExecutionMode::Exclusive,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("a-shared"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("b-exclusive"));
    assert_eq!(machine.pending_len(), 1);
    assert_eq!(
        machine
            .pending(0)
            .expect("pending call at index 0")
            .invocation
            .id
            .as_str(),
        "a-exclusive"
    );
}

#[test]
fn completion_releases_blocked_queued_calls() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "shared", ToolExecutionMode::Shared);
    push(
        &mut machine,
        &conv,
        "exclusive",
        ToolExecutionMode::Exclusive,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert_eq!(pop_id(&mut machine), None);
    machine.mark_complete(&ToolCallId::from("shared"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("exclusive"));
}

#[test]
fn conversation_predicates_report_pending_and_in_flight_work() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    let other = cid("other");
    push(&mut machine, &conv, "shared", ToolExecutionMode::Shared);

    assert!(machine.any_pending_for(&conv));
    assert!(!machine.any_pending_for(&other));
    assert!(!machine.any_in_flight_for(&conv));

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert!(!machine.any_pending_for(&conv));
    assert!(machine.any_in_flight_for(&conv));
    assert!(!machine.any_in_flight_for(&other));
}

#[test]
fn fifo_is_preserved_with_compatible_shared_and_exclusive_calls() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "shared-a", ToolExecutionMode::Shared);
    push(&mut machine, &conv, "shared-b", ToolExecutionMode::Shared);
    push(
        &mut machine,
        &conv,
        "exclusive",
        ToolExecutionMode::Exclusive,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared-a"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared-b"));
    assert_eq!(pop_id(&mut machine), None);
    machine.mark_complete(&ToolCallId::from("shared-a"));
    assert_eq!(pop_id(&mut machine), None);
    machine.mark_complete(&ToolCallId::from("shared-b"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("exclusive"));
}

/// Instant background support asks the harness to close the foreground at
/// dispatch time while keeping the actual tool call tracked until its real
/// result arrives.
#[test]
fn instant_background_completes_foreground_but_remains_running() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    machine.push(
        conv.clone(),
        call("bg"),
        ToolExecutionMode::Exclusive,
        BackgroundSupport::Instant,
        ToolTurnLockScope::Normal,
    );

    let (pending, action) = machine.pop_dispatchable(Instant::now()).expect("dispatch");
    assert_eq!(pending.invocation.id.as_str(), "bg");
    assert_eq!(
        action,
        ForegroundAction::Background {
            call_id: "bg".into()
        }
    );
    assert!(!machine.is_backgrounded(&"bg".into()));
    assert!(machine.any_in_flight_for(&conv));
    assert!(machine.mark_backgrounded(&"bg".into()));
    assert!(machine.is_backgrounded(&"bg".into()));
    assert!(!machine.any_in_flight_for(&conv));
    assert_eq!(machine.in_flight_len(), 1);
}

/// MinForegroundSeconds uses the dispatch instant as the start time. The
/// harness event loop can sleep until `next_background_deadline` instead of
/// polling.
#[test]
fn min_foreground_deadline_backgrounds_once_when_due() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    let start = Instant::now();
    machine.push(
        conv,
        call("slow"),
        ToolExecutionMode::Shared,
        BackgroundSupport::MinForegroundSeconds(5),
        ToolTurnLockScope::Normal,
    );
    let (_, action) = machine.pop_dispatchable(start).expect("dispatch");
    assert_eq!(action, ForegroundAction::None);
    assert_eq!(
        machine.background_due(start + std::time::Duration::from_secs(4)),
        Vec::<ToolCallId>::new()
    );

    assert_eq!(
        machine.background_due(start + std::time::Duration::from_secs(5)),
        vec![ToolCallId::from("slow")]
    );
    assert_eq!(
        machine.background_due(start + std::time::Duration::from_secs(6)),
        Vec::<ToolCallId>::new()
    );
    assert!(machine.is_backgrounded(&"slow".into()));
}

/// Never preserves old foreground behavior: no deadline is armed and the
/// call blocks same-conversation exclusive dispatch until the real result.
#[test]
fn never_background_has_no_deadline() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    machine.push(
        conv.clone(),
        call("never"),
        ToolExecutionMode::Exclusive,
        BackgroundSupport::Never,
        ToolTurnLockScope::Normal,
    );
    machine.push(
        conv,
        call("behind"),
        ToolExecutionMode::Shared,
        BackgroundSupport::Never,
        ToolTurnLockScope::Normal,
    );
    let (_, action) = machine.pop_dispatchable(Instant::now()).expect("dispatch");
    assert_eq!(action, ForegroundAction::None);
    assert!(machine.next_background_deadline().is_none());
    assert_eq!(pop_id(&mut machine), None);
}

/// Delegate parent calls are only launchers. Once their foreground placeholder
/// is published, delegate-vs-delegate locking is handled by the start-agent
/// scheduler, so the launcher must not hold the parent tool-turn lane.
#[test]
fn backgrounded_delegate_launcher_does_not_block_normal_exclusive_call() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push_scoped(
        &mut machine,
        &conv,
        "delegate",
        ToolExecutionMode::Shared,
        ToolTurnLockScope::DelegateLauncher,
    );
    push(&mut machine, &conv, "write", ToolExecutionMode::Exclusive);

    assert_eq!(pop_id(&mut machine).as_deref(), Some("delegate"));
    assert!(machine.mark_backgrounded(&"delegate".into()));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("write"));
}

/// A normal blocked call must not become a FIFO barrier for a later delegate
/// launcher in the same conversation. The launcher will background itself and
/// let start-agent scheduling decide when the sub-agent may actually run.
#[test]
fn delegate_launcher_is_not_skipped_behind_blocked_normal_call() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    push(&mut machine, &conv, "shared", ToolExecutionMode::Shared);
    push(&mut machine, &conv, "write", ToolExecutionMode::Exclusive);
    push_scoped(
        &mut machine,
        &conv,
        "delegate",
        ToolExecutionMode::Shared,
        ToolTurnLockScope::DelegateLauncher,
    );

    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("delegate"));
    assert_eq!(machine.pending_len(), 1);
    assert_eq!(
        machine
            .pending(0)
            .expect("pending call at index 0")
            .invocation
            .id
            .as_str(),
        "write"
    );
}

/// Backgrounding closes the model-visible foreground, but a normal real call
/// still holds its execution-mode lane until the actual result arrives.
/// Without this, a long-running update command could be backgrounded and a
/// second update could start while it is still mutating state.
#[test]
fn backgrounded_update_still_blocks_incompatible_calls_until_real_completion() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    machine.push(
        conv.clone(),
        call("update"),
        ToolExecutionMode::Update,
        BackgroundSupport::Instant,
        ToolTurnLockScope::Normal,
    );
    machine.push(
        conv.clone(),
        call("shared"),
        ToolExecutionMode::Shared,
        BackgroundSupport::Never,
        ToolTurnLockScope::Normal,
    );
    machine.push(
        conv,
        call("second-update"),
        ToolExecutionMode::Update,
        BackgroundSupport::Never,
        ToolTurnLockScope::Normal,
    );

    machine.pop_dispatchable(Instant::now()).expect("dispatch");
    assert!(machine.mark_backgrounded(&"update".into()));
    assert_eq!(pop_id(&mut machine).as_deref(), Some("shared"));
    assert_eq!(pop_id(&mut machine), None);

    machine.mark_complete(&"update".into());
    assert_eq!(pop_id(&mut machine).as_deref(), Some("second-update"));
}

/// A late real result removes actual-running state exactly once after the
/// foreground was already closed by the synthetic background placeholder.
#[test]
fn late_background_completion_clears_actual_running_once() {
    let mut machine = ToolTurnMachine::default();
    let conv = cid("conv");
    machine.push(
        conv,
        call("late"),
        ToolExecutionMode::Shared,
        BackgroundSupport::Instant,
        ToolTurnLockScope::Normal,
    );
    machine.pop_dispatchable(Instant::now()).expect("dispatch");
    assert!(machine.mark_backgrounded(&"late".into()));
    assert!(machine.is_backgrounded(&"late".into()));

    assert_eq!(
        machine.mark_complete(&"late".into()),
        Some(ToolExecutionMode::Shared)
    );
    assert_eq!(machine.mark_complete(&"late".into()), None);
    assert!(!machine.is_backgrounded(&"late".into()));
}
