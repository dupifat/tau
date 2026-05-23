use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use tungstenite::Message;

use super::*;
use crate::common::PromptPayload;
use crate::responses::ResponsesSurface;

#[test]
fn keys_distinguish_sessions_under_same_account() {
    let cfg = make_config("https://chatgpt.com/backend-api", Some("acc"));
    let a = PoolKey::for_request(&cfg, "session-a", &tau_proto::PromptOriginator::User);
    let b = PoolKey::for_request(&cfg, "session-b", &tau_proto::PromptOriginator::User);
    assert_ne!(a, b);
}

#[test]
fn keys_distinguish_side_conversations_under_same_session() {
    // Side conversations can share the parent session_id while representing
    // independent extension query chains. They must not share one mutable
    // WS cache slot when prompt workers run concurrently.
    let cfg = make_config("https://chatgpt.com/backend-api", Some("acc"));
    let a = PoolKey::for_request(
        &cfg,
        "shared-session",
        &tau_proto::PromptOriginator::Extension {
            name: tau_proto::ExtensionName::new("delegate"),
            query_id: "q1".to_owned(),
        },
    );
    let b = PoolKey::for_request(
        &cfg,
        "shared-session",
        &tau_proto::PromptOriginator::Extension {
            name: tau_proto::ExtensionName::new("delegate"),
            query_id: "q2".to_owned(),
        },
    );
    assert_ne!(a, b);
}

#[test]
fn keys_distinguish_accounts_under_same_session() {
    let a = PoolKey::for_request(
        &make_config("https://chatgpt.com/backend-api", Some("acc-1")),
        "session",
        &tau_proto::PromptOriginator::User,
    );
    let b = PoolKey::for_request(
        &make_config("https://chatgpt.com/backend-api", Some("acc-2")),
        "session",
        &tau_proto::PromptOriginator::User,
    );
    assert_ne!(a, b);
}

/// The headline pool invariant: alternating between two sessions
/// must NOT cause the second session's turn to flush the first
/// session's connection. Each `(account, session)` must hold its
/// own socket so the OpenAI connection-local
/// `previous_response_id` cache stays warm across context
/// switches.
#[test]
fn pool_routes_each_session_to_its_own_socket_and_reuses_them() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};

    // Two turns on session A, interleaved with one on session B.
    // Expected: 2 upgrades total (one per session), 3 turns.
    for session in ["session-a", "session-b", "session-a"] {
        let session_id = tau_proto::SessionId::new(session);
        let request = PromptPayload {
            system_prompt: "sys",
            context_items: &[],
            tools: &[],
            params: tau_proto::ModelParams::default(),
            tool_choice: tau_proto::ToolChoice::default(),
            previous_response: None,
            originator: &tau_proto::PromptOriginator::User,
            session_id: &session_id,
            share_user_cache_key: false,
        };
        run_turn_through_pool(
            &mut pool,
            &config,
            session,
            "sp-test",
            &request,
            &mut on_update,
        )
        .expect("turn ok");
    }

    let state = server.lock().expect("server state lock");
    assert_eq!(
        state.upgrade_count, 2,
        "expected one upgrade per distinct session_id (alternating A/B/A — reuses A's socket)"
    );
    assert_eq!(
        state.turns_per_connection,
        vec![2, 1],
        "session-a's socket should have served two turns; session-b's, one"
    );
}

/// Concurrent same-key turns must serialize at the shared-pool reservation
/// boundary. Without that reservation, both workers can observe an empty
/// map while the first turn owns the socket and open two sockets for one
/// conversation chain.
#[test]
fn shared_pool_serializes_same_key_turns() {
    let (addr, server) = spawn_fake_codex_server();
    server.lock().expect("server lock").response_delay = Duration::from_millis(100);
    let config = Arc::new(make_config(
        &format!("http://{addr}/backend-api"),
        Some("acc"),
    ));
    let pool = Arc::new(SharedWsPool::new());
    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for idx in 0..2 {
        let config = config.clone();
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            run_shared_turn(&pool, &config, "same-session", &format!("sp-{idx}"));
        }));
    }
    for handle in handles {
        handle.join().expect("worker join");
    }

    let state = server.lock().expect("server state lock");
    assert_eq!(
        state.upgrade_count, 1,
        "same PoolKey must reuse one reserved socket rather than opening a parallel chain"
    );
    assert_eq!(state.turns_per_connection, vec![2]);
    assert_eq!(
        state.max_active_turns, 1,
        "same-key WS turns should run one at a time"
    );
}

/// A prompt canceled while it is queued behind a same-key WS reservation
/// must stop waiting instead of sending a stale network request after the
/// active turn releases. The pool polls prompt cancellation while parked on
/// the condvar so the waiter can unwind and let the worker emit its
/// terminal canceled response/PromptDone.
#[test]
fn shared_pool_checkout_wait_aborts_when_canceled() {
    let config = make_config("https://chatgpt.com/backend-api", Some("acc"));
    let key = PoolKey::for_request(&config, "same-session", &tau_proto::PromptOriginator::User);
    let pool = Arc::new(SharedWsPool::new());
    pool.inner
        .lock()
        .expect("pool lock")
        .busy
        .insert(key.clone());

    let canceled = Arc::new(AtomicBool::new(false));
    let started = Arc::new(Barrier::new(2));
    let handle = {
        let pool = pool.clone();
        let key = key.clone();
        let canceled = canceled.clone();
        let started = started.clone();
        thread::spawn(move || {
            started.wait();
            pool.checkout_until(&key, "test", &mut || canceled.load(Ordering::SeqCst))
        })
    };

    started.wait();
    thread::sleep(Duration::from_millis(100));
    canceled.store(true, Ordering::SeqCst);

    let result = handle.join().expect("checkout waiter join");
    assert!(matches!(result, Err(WsTurnError::Canceled)));
    assert!(
        pool.inner.lock().expect("pool lock").busy.contains(&key),
        "a canceled waiter must not steal or clear the active worker's reservation"
    );
}

/// Prewarm runs on the provider main loop, so it must never park behind an
/// active same-key reservation. A busy key means a real turn is already
/// doing the warming work; skip best-effort prewarm instead of delaying
/// cancellation, worker output, PromptDone, or ACK processing.
#[test]
fn shared_prewarm_skips_busy_same_key_without_waiting() {
    let (addr, _server) = spawn_fake_codex_server();
    let config = Arc::new(make_config(
        &format!("http://{addr}/backend-api"),
        Some("acc"),
    ));
    let pool = Arc::new(SharedWsPool::new());
    let key = PoolKey::for_request(&config, "same-session", &tau_proto::PromptOriginator::User);
    pool.inner
        .lock()
        .expect("pool lock")
        .busy
        .insert(key.clone());

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = {
        let config = config.clone();
        let pool = pool.clone();
        thread::spawn(move || {
            let session_id = tau_proto::SessionId::new("same-session");
            let originator = tau_proto::PromptOriginator::User;
            let request = PromptPayload {
                system_prompt: "sys",
                context_items: &[],
                tools: &[],
                params: tau_proto::ModelParams::default(),
                tool_choice: tau_proto::ToolChoice::default(),
                previous_response: None,
                originator: &originator,
                session_id: &session_id,
                share_user_cache_key: false,
            };
            let started = std::time::Instant::now();
            let result = run_prewarm_through_shared_pool(&pool, &config, "same-session", &request);
            tx.send((started.elapsed(), result.is_ok()))
                .expect("send prewarm result");
        })
    };

    let (elapsed, ok) = match rx.recv_timeout(Duration::from_millis(150)) {
        Ok(result) => result,
        Err(error) => {
            pool.inner.lock().expect("pool lock").busy.remove(&key);
            pool.changed.notify_all();
            handle.join().expect("prewarm join after unblocking");
            panic!("prewarm blocked on a busy same-key reservation: {error}");
        }
    };
    handle.join().expect("prewarm join");

    assert!(ok, "skipped prewarm should report success");
    assert!(
        elapsed < Duration::from_millis(50),
        "prewarm should not wait for the checkout poll interval; elapsed {elapsed:?}"
    );
    assert!(
        pool.inner.lock().expect("pool lock").busy.contains(&key),
        "skipped prewarm must not clear the active worker's reservation"
    );
    assert_eq!(
        pool.stats().expect("pool stats").upgrades,
        0,
        "skipped prewarm should not open a socket"
    );
}

/// Different pool keys should not be serialized by the same-key guard. The
/// shared mutex may protect bookkeeping, but it must not cover network I/O.
#[test]
fn shared_pool_allows_different_keys_to_run_concurrently() {
    let (addr, server) = spawn_fake_codex_server();
    server.lock().expect("server lock").response_delay = Duration::from_millis(150);
    let config = Arc::new(make_config(
        &format!("http://{addr}/backend-api"),
        Some("acc"),
    ));
    let pool = Arc::new(SharedWsPool::new());
    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for (idx, session) in ["session-a", "session-b"].into_iter().enumerate() {
        let config = config.clone();
        let pool = pool.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            run_shared_turn(&pool, &config, session, &format!("sp-{idx}"));
        }));
    }
    for handle in handles {
        handle.join().expect("worker join");
    }

    let state = server.lock().expect("server state lock");
    assert_eq!(
        state.upgrade_count, 2,
        "different keys use different sockets"
    );
    assert_eq!(
        state.max_active_turns, 2,
        "different-key WS network turns should overlap"
    );
}

/// Cap the pool at 2 and exercise three sessions. The
/// least-recently-used session's socket must get evicted; a
/// follow-up turn on that session triggers a fresh upgrade.
#[test]
fn pool_evicts_lru_when_capacity_exceeded() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    pool.max = 2;
    let mut on_update = |_: &str, _: Option<&str>| {};

    // A → B → C: three different sessions, cap=2.
    // After C: A (LRU) is evicted, pool holds {B, C}.
    for session in ["a", "b", "c"] {
        run_turn(&mut pool, &config, session, &mut on_update);
    }
    assert_eq!(pool.len(), 2);
    assert_eq!(server.lock().expect("server state lock").upgrade_count, 3);

    // Touching A again must re-upgrade (its old socket got
    // evicted on C's release).
    run_turn(&mut pool, &config, "a", &mut on_update);
    assert_eq!(server.lock().expect("server state lock").upgrade_count, 4);
}

/// Connections older than `MAX_CONNECTION_AGE` must be
/// pre-emptively reopened on checkout, so the server's 60-min
/// hard cap never fires mid-turn.
#[test]
fn pool_reopens_aged_out_connections_on_checkout() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};

    // First turn opens connection #1.
    run_turn(&mut pool, &config, "session-aged", &mut on_update);
    assert_eq!(server.lock().expect("server state lock").upgrade_count, 1);

    // Forcibly age the cached connection past the threshold.
    let key = PoolKey::for_request(&config, "session-aged", &tau_proto::PromptOriginator::User);
    if let Some(conn) = pool.conns.get_mut(&key) {
        conn.opened_at = std::time::Instant::now() - MAX_CONNECTION_AGE - Duration::from_secs(1);
    } else {
        panic!("expected connection in pool");
    }

    // Next turn must reopen rather than send on the stale socket.
    run_turn(&mut pool, &config, "session-aged", &mut on_update);
    assert_eq!(
        server.lock().expect("server state lock").upgrade_count,
        2,
        "aged-out connection should have been replaced"
    );
}

/// HTTP+SSE base + plain TCP fake server doubles as the WS
/// transport's smoke test: connect, send a turn, read all the
/// expected events back, see `response_id` captured.
#[test]
fn ws_turn_captures_response_id_for_chain_continuation() {
    let (addr, _server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut last_text = String::new();
    let mut on_update = |text: &str, _thinking: Option<&str>| {
        last_text = text.to_owned();
    };

    let session_id = tau_proto::SessionId::new("session-x");
    let request = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: None,
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };

    let state = run_turn_through_pool(
        &mut pool,
        &config,
        "session-x",
        "sp-test",
        &request,
        &mut on_update,
    )
    .expect("turn ok");
    assert_eq!(last_text, "hello");
    assert!(
        state.response_id.is_some(),
        "response_id must be captured so the next turn can chain via previous_response_id"
    );
}

/// A `generate:false` prewarm is only useful if the next matching
/// real turn chains off the prewarm's `response.id` and sends just
/// the new input delta. Sending the full prompt again on the same
/// socket leaves the prewarm as dead weight and misses the Codex
/// stateful-cache fast path.
#[test]
fn prewarm_anchor_chains_next_matching_turn_as_delta() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};
    let session_id = tau_proto::SessionId::new("session-prewarm");
    let prewarmed_messages = vec![user_msg("AGENTS.md context")];
    let real_messages = vec![user_msg("AGENTS.md context"), user_msg("actual request")];

    let prewarm = PromptPayload {
        system_prompt: "sys",
        context_items: &prewarmed_messages,
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: None,
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    run_prewarm_through_pool(&mut pool, &config, "session-prewarm", &prewarm).expect("prewarm ok");

    let real = PromptPayload {
        context_items: &real_messages,
        ..prewarm
    };
    run_turn_through_pool(
        &mut pool,
        &config,
        "session-prewarm",
        "sp-test",
        &real,
        &mut on_update,
    )
    .expect("turn ok");

    let s = server.lock().expect("server lock");
    assert_eq!(s.upgrade_count, 1, "prewarm and turn must share one socket");
    assert_eq!(s.requests.len(), 2, "expected prewarm plus real turn");
    let warm = &s.requests[0];
    let turn = &s.requests[1];
    assert_eq!(
        warm.get("generate").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        turn.get("previous_response_id")
            .and_then(serde_json::Value::as_str),
        Some("resp_0_1")
    );
    assert_eq!(
        turn.get("input")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "real turn should send only messages added after the prewarmed prefix",
    );
}

/// Codex's WS `previous_response_id` cache is connection-local. When the
/// pool opens a fresh socket for a chained turn, the new socket has no
/// knowledge of the prior response id. The pool strips the id, replays the
/// full prompt once over WS, and keeps the fresh socket warm for the next
/// turn instead of sticky-falling back to HTTP.
#[test]
fn fresh_open_with_previous_response_rebuilds_ws_warmth() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};

    let session_id = tau_proto::SessionId::new("session-fresh");
    let request = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: Some(crate::common::PreviousResponse {
            id: "resp_from_a_dead_socket",
            next_item_index: 0,
            transport: Some(tau_proto::ProviderBackendTransport::Websocket),
        }),
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    run_turn_through_pool(
        &mut pool,
        &config,
        "session-fresh",
        "sp-test",
        &request,
        &mut on_update,
    )
    .expect("fresh chained WS turn should rebuild warmth");

    let s = server.lock().expect("server lock");
    assert_eq!(s.upgrade_count, 1, "must open a replacement WS socket");
    assert_eq!(s.requests.len(), 1, "expected one WS full replay envelope");
    assert!(
        s.requests[0].get("previous_response_id").is_none(),
        "fresh WS socket must not receive a stale chain id"
    );
    assert_eq!(
        pool.stats().chain_strips_on_fresh,
        1,
        "chain strip counter should report the cold WS rebuild"
    );
}

#[test]
fn fresh_open_with_previous_response_preserves_compacted_items() {
    let (addr, server) = spawn_fake_codex_server();
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};
    let session_id = tau_proto::SessionId::new("session-compacted");
    let messages = vec![
        tau_proto::ContextItem::Compaction(tau_proto::OpaqueProviderItem(
            crate::common::json_to_cbor(&serde_json::json!({
                "type": "message",
                "role": "user",
                "content": "compacted-sentinel",
            })),
        )),
        user_msg("after compaction"),
    ];
    let request = PromptPayload {
        system_prompt: "sys",
        context_items: &messages,
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: Some(crate::common::PreviousResponse {
            id: "resp_from_a_dead_socket",
            next_item_index: 0,
            transport: Some(tau_proto::ProviderBackendTransport::Websocket),
        }),
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };

    run_turn_through_pool(
        &mut pool,
        &config,
        "session-compacted",
        "sp-test",
        &request,
        &mut on_update,
    )
    .expect("fresh chained WS turn should replay compacted context");

    let s = server.lock().expect("server lock");
    let input = s.requests[0]
        .get("input")
        .and_then(serde_json::Value::as_array)
        .expect("input array");
    assert!(
        input.iter().any(
            |item| item.get("content").and_then(serde_json::Value::as_str)
                == Some("compacted-sentinel")
        ),
        "fresh WS replay must keep compacted input items when stripping the stale chain id",
    );
}

/// A cached connection dies mid-turn (keepalive timeout / TCP reset). If
/// the request has a `previous_response_id`, the pool must reopen a fresh
/// WS socket, strip the stale chain id, and leave the replacement socket in
/// the pool so later turns regain cache warmth.
#[test]
fn mid_stream_close_with_chain_rebuilds_ws_warmth() {
    let (addr, server) = spawn_fake_codex_server();
    // Make connection #0 die mid-turn-#2 (after_turn=1 -> the
    // second arriving turn on conn 0 is the one that gets closed).
    server.lock().expect("server lock").fault = Some(MidStreamCloseFault {
        on_conn_index: 0,
        after_turn: 1,
    });
    let config = make_config(&format!("http://{addr}/backend-api"), Some("acc"));
    let mut pool = WsPool::new();
    let mut on_update = |_: &str, _: Option<&str>| {};

    // Turn 1: opens conn-0, returns a `response_id` the harness
    // would chain off for turn 2.
    let session_id = tau_proto::SessionId::new("session-die");
    let req1 = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: None,
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    let state1 = run_turn_through_pool(
        &mut pool,
        &config,
        "session-die",
        "sp-test-1",
        &req1,
        &mut on_update,
    )
    .expect("first turn ok");
    let prev_id = state1.response_id.expect("first turn yielded response_id");

    // Turn 2: harness wants to chain via `prev_id`. The cached socket dies
    // mid-stream; pool must reopen cold WS and strip the chain id rather
    // than sticky-disabling WS for the session.
    let req2 = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: Some(crate::common::PreviousResponse {
            id: &prev_id,
            next_item_index: 0,
            transport: Some(tau_proto::ProviderBackendTransport::Websocket),
        }),
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    run_turn_through_pool(
        &mut pool,
        &config,
        "session-die",
        "sp-test-2",
        &req2,
        &mut on_update,
    )
    .expect("chained reconnect should rebuild WS warmth");

    let s = server.lock().expect("server lock");
    assert_eq!(
        s.upgrade_count, 2,
        "mid-stream close should force one replacement WS upgrade"
    );
    // Three captured requests in arrival order:
    //   #0: turn-1 on conn-0 (no chain id, no prior response)
    //   #1: turn-2 on conn-0 (had chain id; this is the one that died)
    //   #2: turn-2 replay on conn-1 (chain stripped for fresh WS)
    assert_eq!(s.requests.len(), 3, "expected one WS replay envelope");
    assert!(
        s.requests[1].get("previous_response_id").is_some(),
        "turn-2 on the warm socket should still carry the chain id (warm cache path)"
    );
    assert!(
        s.requests[2].get("previous_response_id").is_none(),
        "replacement socket must not receive a stale chain id"
    );
    assert_eq!(
        pool.stats().silent_reconnects,
        1,
        "stat counter should record the silent reconnect"
    );
    assert_eq!(
        pool.stats().chain_strips_on_fresh,
        1,
        "stat counter should record the cold WS rebuild"
    );
}

/// Every error shape `WsConn::run_turn` can emit must be
/// classified recoverable so the silent-reconnect path catches
/// it. The old narrow allow-list (`"ws closed"` /
/// `"previous_response"` / `"response not found"`) silently
/// missed `"ws writer task gone"` and `"ws keepalive failed:
/// ..."` after the tokio-tungstenite refactor — a dead cached
/// socket would then leak its error to the user instead of
/// being reopened transparently. Guards against re-tightening.
#[test]
fn all_run_turn_error_shapes_are_recoverable() {
    let cases = [
        "stream error: ws closed",
        "stream error: ws closed mid-stream (code=1011 reason=keepalive ping timeout)",
        "stream error: ws writer task gone",
        "stream error: ws reader task gone",
        "stream error: ws send failed: Connection closed normally",
        "stream error: ws keepalive failed: IO error: broken pipe",
        "stream error: Previous response not found",
        "stream error: previous_response_id expired",
        "stream error: response not found",
        "stream error: WebSocket protocol error: bad frame",
    ];
    for body in cases {
        let err = LlmError::HttpStatus(0, body.to_owned());
        assert!(
            is_recoverable_ws_error(&err),
            "expected recoverable: {body}"
        );
    }
}

/// Inverse: only `HttpStatus(0, "stream error: ...")` is in
/// scope. Other code paths (real HTTP errors, JSON failures,
/// non-stream `HttpStatus(0, ...)` bodies) must not be
/// transparently retried — they could be terminal user-facing
/// problems (bad auth, malformed request) where reopening the
/// socket changes nothing.
#[test]
fn non_run_turn_errors_are_not_recoverable() {
    let cases = [
        LlmError::HttpStatus(0, "response failed: model overloaded".to_owned()),
        LlmError::HttpStatus(401, "Unauthorized".to_owned()),
        LlmError::HttpStatus(429, "rate limit".to_owned()),
        LlmError::HttpStatus(0, "some unrelated body".to_owned()),
    ];
    for err in cases {
        assert!(
            !is_recoverable_ws_error(&err),
            "expected NOT recoverable: {err:?}"
        );
    }
}

/// Account-level caps (usage_limit_reached etc.) ride the same
/// `stream error: …` envelope as transport hiccups but are NOT
/// fixable by reopening the socket. The pool must surface them
/// up to `LlmError::retry_after` (which also returns `None` for
/// these) instead of burning a fresh upgrade.
#[test]
fn account_limit_stream_errors_are_not_silent_reconnects() {
    let cases = [
        "stream error: usage limit (type=usage_limit_reached)",
        "stream error: rate limit (type=rate_limit_exceeded)",
        "stream error: quota (type=quota_exceeded)",
    ];
    for body in cases {
        let err = LlmError::HttpStatus(0, body.to_owned());
        assert!(
            !is_recoverable_ws_error(&err),
            "account cap must short-circuit, not silent-reconnect: {body}",
        );
    }
}

// -----------------------------------------------------------------
// Fake Codex server: minimal blocking tungstenite acceptor.
// -----------------------------------------------------------------

#[derive(Default)]
struct ServerState {
    /// How many TCP+upgrade pairs we've accepted. Each
    /// `(account, session)` pair the pool keys against should
    /// produce exactly one upgrade across its lifetime (modulo
    /// age-out / OAuth refresh).
    upgrade_count: usize,
    /// `turns_per_connection[i]` is the number of
    /// `response.create` envelopes connection `i` served before
    /// closing. Lets pool-reuse tests assert that A's two turns
    /// landed on one socket.
    turns_per_connection: Vec<usize>,
    /// Captured request bodies, in arrival order across all
    /// connections. Available for tests that want to inspect
    /// what the client actually sent (chain ids, model knobs).
    requests: Vec<serde_json::Value>,
    /// Artificial per-turn response delay used by concurrency tests to make
    /// overlapping network turns observable.
    response_delay: Duration,
    /// Number of fake server turns currently sleeping/streaming.
    active_turns: usize,
    /// Maximum simultaneous fake server turns observed during a test.
    max_active_turns: usize,
    /// Fault injection. When `Some`, the worker for a matching
    /// connection drops the socket with a 1011 close frame
    /// instead of serving the offending turn — mimicking the
    /// "keepalive ping timeout" the live Codex server produces
    /// when its idle reaper fires. Tests use this to exercise
    /// the silent-reconnect path.
    fault: Option<MidStreamCloseFault>,
}

/// "After connection index `on_conn_index` has fully served
/// `after_turn` turns, drop the next incoming turn mid-stream."
/// Indices are zero-based; `after_turn: 1` means the second
/// arriving turn on that connection is the one that gets killed.
#[derive(Clone, Copy)]
struct MidStreamCloseFault {
    on_conn_index: usize,
    after_turn: usize,
}

fn spawn_fake_codex_server() -> (SocketAddr, Arc<Mutex<ServerState>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let state = Arc::new(Mutex::new(ServerState::default()));
    let state_clone = state.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let conn_state = state_clone.clone();
            thread::spawn(move || handle_one_connection(stream, conn_state));
        }
    });
    (addr, state)
}

fn handle_one_connection(stream: TcpStream, state: Arc<Mutex<ServerState>>) {
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(_) => return,
    };
    let conn_idx;
    {
        let mut s = state.lock().expect("server state lock");
        s.upgrade_count += 1;
        conn_idx = s.turns_per_connection.len();
        s.turns_per_connection.push(0);
    }

    let mut turn_counter = 0_usize;
    loop {
        let msg = match ws.read() {
            Ok(m) => m,
            Err(_) => return,
        };
        match msg {
            Message::Text(text) => {
                let parsed: serde_json::Value =
                    serde_json::from_str(text.as_str()).unwrap_or(serde_json::Value::Null);
                let (fault_now, response_delay) = {
                    let mut s = state.lock().expect("server state lock");
                    s.requests.push(parsed.clone());
                    s.turns_per_connection[conn_idx] += 1;
                    s.active_turns += 1;
                    s.max_active_turns = s.max_active_turns.max(s.active_turns);
                    let fault_now = s
                        .fault
                        .filter(|f| f.on_conn_index == conn_idx && turn_counter >= f.after_turn);
                    (fault_now, s.response_delay)
                };
                turn_counter += 1;
                if !response_delay.is_zero() {
                    thread::sleep(response_delay);
                }
                if fault_now.is_some() {
                    // Mimic the live Codex 1011 keepalive-timeout
                    // drop: send a close frame and bail without
                    // streaming the response body. Client side
                    // sees `Message::Close` → `LlmError(0, "stream
                    // error: ws closed mid-stream ...")`.
                    let _ = ws.send(Message::Close(Some(tungstenite::protocol::CloseFrame {
                        code: tungstenite::protocol::frame::coding::CloseCode::Error,
                        reason: "keepalive ping timeout".into(),
                    })));
                    finish_server_turn(&state);
                    return;
                }
                // Stream a tiny canned event sequence: one
                // visible-text delta, then completed.
                let events = [
                    serde_json::json!({
                        "type": "response.output_text.delta",
                        "delta": "hello",
                    }),
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {
                            "id": format!("resp_{conn_idx}_{turn_counter}"),
                            "usage": {
                                "input_tokens": 1,
                                "output_tokens": 1,
                                "input_tokens_details": { "cached_tokens": 0 },
                            },
                        },
                    }),
                ];
                for ev in events {
                    let txt = serde_json::to_string(&ev).expect("serialize");
                    if ws.send(Message::Text(txt.into())).is_err() {
                        finish_server_turn(&state);
                        return;
                    }
                }
                finish_server_turn(&state);
            }
            Message::Close(_) => return,
            _ => continue,
        }
    }
}

fn finish_server_turn(state: &Arc<Mutex<ServerState>>) {
    let mut s = state.lock().expect("server state lock");
    s.active_turns = s.active_turns.saturating_sub(1);
}

fn user_msg(text: &str) -> tau_proto::ContextItem {
    tau_proto::ContextItem::Message(tau_proto::MessageItem {
        role: tau_proto::ContextRole::User,
        content: vec![tau_proto::ContentPart::Text {
            text: text.to_owned(),
        }],
        phase: None,
    })
}

fn run_turn(
    pool: &mut WsPool,
    config: &ResponsesConfig,
    session: &str,
    on_update: &mut impl FnMut(&str, Option<&str>),
) {
    let session_id = tau_proto::SessionId::new(session);
    let request = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: None,
        originator: &tau_proto::PromptOriginator::User,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    run_turn_through_pool(pool, config, session, "sp-test", &request, on_update).expect("turn ok");
}

fn run_shared_turn(
    pool: &SharedWsPool,
    config: &ResponsesConfig,
    session: &str,
    session_prompt_id: &str,
) {
    let session_id = tau_proto::SessionId::new(session);
    let originator = tau_proto::PromptOriginator::User;
    let request = PromptPayload {
        system_prompt: "sys",
        context_items: &[],
        tools: &[],
        params: tau_proto::ModelParams::default(),
        tool_choice: tau_proto::ToolChoice::default(),
        previous_response: None,
        originator: &originator,
        session_id: &session_id,
        share_user_cache_key: false,
    };
    let mut on_update = |_: &str, _: Option<&str>| {};
    run_turn_through_shared_pool(
        pool,
        config,
        session_prompt_id,
        &request,
        &mut || false,
        &mut on_update,
    )
    .expect("shared turn ok");
}

fn make_config(base_url: &str, account_id: Option<&str>) -> ResponsesConfig {
    ResponsesConfig {
        surface: ResponsesSurface::ChatGpt,
        base_url: base_url.into(),
        api_key: "test".into(),
        model_id: "gpt-5-codex".into(),
        account_id: account_id.map(str::to_owned),
        supports_reasoning_effort: false,
        supports_reasoning_summary: false,
        supports_verbosity: false,
        supports_phase: false,
        supports_websocket: true,
        supports_compaction: false,
        supports_prompt_cache_key: false,
        supports_encrypted_reasoning: false,
    }
}
