use std::path::PathBuf;

use tau_proto::{
    AgentHeadMoved, AgentId, AgentPromptSubmitted, Event, PromptMessageClass, PromptOriginator,
    SessionAgentLoaded, SessionAgentUnloaded, SessionId,
};

use crate::{
    AgentEntry, AgentEventParent, AgentStore, AgentStoreError, NodeId, SessionStore,
    SessionStoreError,
};

fn temp_dir(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "tau-core-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    path
}

fn agent_prompt(agent_id: &str, text: &str) -> Event {
    Event::AgentPromptSubmitted(AgentPromptSubmitted {
        agent_id: AgentId::from(agent_id),
        text: text.to_owned(),
        message_class: PromptMessageClass::User,
        originator: PromptOriginator::User,
        ctx_id: None,
    })
}

#[test]
fn agent_store_persists_transcript_under_agent_directory() {
    let agents_dir = temp_dir("agents");
    let mut store = AgentStore::open(&agents_dir).expect("open agent store");

    let outcome = store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "hello"))
        .expect("append agent event");

    assert_eq!(outcome.id.get(), 0);
    assert_eq!(outcome.folded_node_id.map(|id| id.get()), Some(0));
    assert!(agents_dir.join("agent-1").join("events.cbor").exists());

    let reopened = AgentStore::open(&agents_dir).expect("reopen agent store");
    let tree = reopened.agent("agent-1").expect("agent tree");
    assert_eq!(tree.agent_id(), "agent-1");
    assert_eq!(tree.current_branch().len(), 1);
    assert!(matches!(
        tree.current_branch()[0],
        AgentEntry::UserInput { .. }
    ));

    let events = reopened.agent_events("agent-1").expect("agent events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, agent_prompt("agent-1", "hello"));

    let _ = std::fs::remove_dir_all(agents_dir);
}

#[test]
fn agent_store_replays_explicit_root_parent_after_reopen() {
    let agents_dir = temp_dir("agents-explicit-root-parent");
    let mut store = AgentStore::open(&agents_dir).expect("open agent store");

    store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "first"))
        .expect("append first prompt");
    store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "second"))
        .expect("append second prompt");
    store
        .append_agent_event_at(
            "agent-1",
            None,
            AgentEventParent::Root,
            agent_prompt("agent-1", "fresh branch"),
            tau_proto::UnixMicros::now(),
        )
        .expect("append fresh branch prompt");

    let reopened = AgentStore::open(&agents_dir).expect("reopen agent store");
    let tree = reopened.agent("agent-1").expect("agent tree");
    let fresh_branch = tree.nodes().last().expect("fresh branch node");

    assert_eq!(fresh_branch.parent_id, None);
    assert_eq!(tree.head(), Some(fresh_branch.id));

    let _ = std::fs::remove_dir_all(agents_dir);
}

#[test]
fn agent_store_rejects_unknown_explicit_parent_before_persisting() {
    let agents_dir = temp_dir("agents-unknown-explicit-parent");
    let mut store = AgentStore::open(&agents_dir).expect("open agent store");

    store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "first"))
        .expect("append first prompt");

    let error = store
        .append_agent_event_at(
            "agent-1",
            None,
            AgentEventParent::Under(NodeId::new(999)),
            agent_prompt("agent-1", "dangling parent"),
            tau_proto::UnixMicros::now(),
        )
        .expect_err("agent store must reject unknown explicit parents");
    match error {
        AgentStoreError::InvalidEvent { source } => {
            assert!(source.to_string().contains("unknown node_id: 999"));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    let events = store.agent_events("agent-1").expect("agent events");
    assert_eq!(events.len(), 1);
    let tree = store.agent("agent-1").expect("agent tree");
    assert_eq!(tree.nodes().len(), 1);

    let _ = std::fs::remove_dir_all(agents_dir);
}

#[test]
fn agent_store_restores_head_move_before_next_append() {
    let agents_dir = temp_dir("agents-head-move");
    let mut store = AgentStore::open(&agents_dir).expect("open agent store");

    store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "first"))
        .expect("append first prompt");
    store
        .append_agent_event("agent-1", None, agent_prompt("agent-1", "second"))
        .expect("append second prompt");
    store
        .append_agent_event(
            "agent-1",
            None,
            Event::AgentHeadMoved(AgentHeadMoved {
                agent_id: AgentId::from("agent-1"),
                node_id: NodeId::new(0),
            }),
        )
        .expect("persist head move");
    drop(store);

    let mut reopened = AgentStore::open(&agents_dir).expect("reopen agent store");
    let tree = reopened.agent("agent-1").expect("agent tree after reopen");
    assert_eq!(tree.head(), Some(NodeId::new(0)));

    reopened
        .append_agent_event(
            "agent-1",
            None,
            agent_prompt("agent-1", "branched after resume"),
        )
        .expect("append resumed branch prompt");

    let tree = reopened.agent("agent-1").expect("agent tree after append");
    let branched = tree.nodes().last().expect("branched node");
    assert_eq!(branched.parent_id, Some(NodeId::new(0)));

    let _ = std::fs::remove_dir_all(agents_dir);
}

#[test]
fn session_store_persists_only_membership_facts() {
    let sessions_dir = temp_dir("sessions");
    let mut store = SessionStore::open(&sessions_dir).expect("open session store");

    let loaded = Event::SessionAgentLoaded(SessionAgentLoaded {
        session_id: SessionId::from("session-1"),
        agent_id: AgentId::from("agent-1"),
    });
    let outcome = store
        .append_session_event("session-1", None, loaded.clone())
        .expect("append loaded");

    assert_eq!(outcome.id.get(), 0);
    assert_eq!(outcome.folded_node_id, None);
    assert!(sessions_dir.join("session-1").join("events.cbor").exists());
    assert!(
        store
            .session("session-1")
            .expect("session membership")
            .contains_agent(&AgentId::from("agent-1"))
    );

    store
        .append_session_event(
            "session-1",
            None,
            Event::SessionAgentUnloaded(SessionAgentUnloaded {
                session_id: SessionId::from("session-1"),
                agent_id: AgentId::from("agent-1"),
            }),
        )
        .expect("append unloaded");

    let reopened = SessionStore::open(&sessions_dir).expect("reopen session store");
    let membership = reopened.session("session-1").expect("session membership");
    assert_eq!(membership.session_id(), "session-1");
    assert!(!membership.contains_agent(&AgentId::from("agent-1")));
    let events = reopened.session_events("session-1").expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, loaded);

    let _ = std::fs::remove_dir_all(sessions_dir);
}

#[test]
fn agent_store_rejects_non_agent_transcript_events() {
    let agents_dir = temp_dir("agent-rejects-non-transcript");
    let mut store = AgentStore::open(&agents_dir).expect("open agent store");

    let session_event = Event::SessionAgentLoaded(SessionAgentLoaded {
        session_id: SessionId::from("session-1"),
        agent_id: AgentId::from("agent-1"),
    });
    let error = store
        .append_agent_event("agent-1", None, session_event)
        .expect_err("agent store must reject session membership events");
    assert!(matches!(error, AgentStoreError::InvalidEvent { .. }));

    let mismatched = agent_prompt("agent-2", "not this agent");
    let error = store
        .append_agent_event("agent-1", None, mismatched)
        .expect_err("agent store must reject mismatched agent events");
    assert!(matches!(error, AgentStoreError::InvalidEvent { .. }));
    assert!(!agents_dir.join("agent-1").join("events.cbor").exists());

    let _ = std::fs::remove_dir_all(agents_dir);
}

#[test]
fn session_store_rejects_transcript_events() {
    let sessions_dir = temp_dir("session-rejects-transcript");
    let mut store = SessionStore::open(&sessions_dir).expect("open session store");

    let error = store
        .append_session_event("session-1", None, agent_prompt("agent-1", "not membership"))
        .expect_err("session store must reject transcript events");

    assert!(matches!(error, SessionStoreError::InvalidEvent { .. }));
    assert!(!sessions_dir.join("session-1").join("events.cbor").exists());

    let _ = std::fs::remove_dir_all(sessions_dir);
}
