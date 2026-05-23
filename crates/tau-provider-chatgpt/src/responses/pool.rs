//! WebSocket connection pool for the Codex Responses backend.
//!
//! See `TODO-codex-websocket.md` §2 for the design rationale. Recap:
//!
//! - The provider processes prompts concurrently, so it can alternate between
//!   conversations (different sessions, sub-agent delegations interleaved with
//!   the parent). The OpenAI WS endpoint only caches the *most recent*
//!   `previous_response_id` per socket, so routing A → B → A on one shared
//!   socket would flush each chain's warmth on every switch. Keep one
//!   connection per `(account, session, conversation)` so warmth survives
//!   context-switches.
//! - Connection-in-flight exclusivity is enforced by ownership plus the shared
//!   wrapper's per-key busy set: checkout removes the connection from the map
//!   before a worker runs the turn, and same-key workers wait until release or
//!   drop before retrying. Different keys do not wait on each other's network
//!   turns.
//! - Bounded by a soft cap (env-tunable `TAU_WS_POOL_MAX`,
//!   [`DEFAULT_POOL_MAX`]). LRU eviction when full.
//! - Connections age out near the server's 60-minute hard cap so a call doesn't
//!   fail mid-turn from the server slamming the door.
//! - Bearer-mismatch on checkout means OAuth refreshed; drop the stale socket
//!   and open a new one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

const CHECKOUT_ABORT_POLL: Duration = Duration::from_millis(50);

use super::ResponsesConfig;
use super::ws::WsConn;
use crate::common::LlmError;

/// Default soft cap on simultaneously-cached WS connections.
///
/// One per `(account, session, conversation)`. A typical interactive workload
/// runs 1–3 active sessions/conversations (the user's main + any in-flight
/// sub-agent delegation). The cap exists to bound pathological growth (a
/// long-lived agent process where the user reopens many old
/// sessions), not because the normal path needs many slots.
pub const DEFAULT_POOL_MAX: usize = 10;

/// Environment variable that overrides [`DEFAULT_POOL_MAX`] at
/// `WsPool::new()` time.
pub const POOL_MAX_ENV: &str = "TAU_WS_POOL_MAX";

/// Margin under the server's 60-minute hard cap before we
/// pre-emptively reopen a connection on checkout. Five minutes is
/// safer than cutting it close — a 59-minute-old connection that
/// dies *after* we send `response.create` surfaces as a mid-stream
/// `stream error` to the user, which a `<55min ? reuse : reopen`
/// check avoids entirely.
pub const MAX_CONNECTION_AGE: Duration = Duration::from_secs(55 * 60);

/// Pool key. A connection caches the previous_response of one
/// conversation chain; different chains get different sockets so
/// alternating between them preserves each chain's warm cache.
///
/// - `base_url` + `account_id` form a "socket realm" — same bearer, same
///   server-side state. Cross-realm reuse is impossible.
/// - `session_id` scopes the harness session. Side conversations may share a
///   session id with their parent, so `conversation_id` further separates the
///   user turn from extension-originated query chains.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub base_url: String,
    pub account_id: Option<String>,
    pub session_id: String,
    pub conversation_id: String,
}

impl PoolKey {
    pub fn for_request(
        config: &ResponsesConfig,
        session_id: &str,
        originator: &tau_proto::PromptOriginator,
    ) -> Self {
        Self {
            base_url: config.base_url.clone(),
            account_id: config.account_id.clone(),
            session_id: session_id.to_owned(),
            conversation_id: conversation_id(originator),
        }
    }
}

fn conversation_id(originator: &tau_proto::PromptOriginator) -> String {
    match originator {
        tau_proto::PromptOriginator::User => "user".to_owned(),
        tau_proto::PromptOriginator::Extension { name, query_id } => {
            format!("extension/{name}/{query_id}")
        }
    }
}

/// Single-threaded pool of WS connections.
///
/// Hot path (turn N+1 on a known session): `checkout` returns the
/// existing `WsConn` (removed from the map); the caller runs the
/// turn; on success it calls `release` to put the conn back at the
/// head of the LRU queue. On error (mid-stream close, IO break),
/// the caller drops the connection — the entry is already removed
/// from the map and the LRU list resyncs lazily.
pub struct WsPool {
    conns: HashMap<PoolKey, WsConn>,
    /// Front = most recent. Pruned of stale keys on `release` /
    /// `checkout` rather than eagerly — a key in the queue without
    /// a matching map entry just means that connection died and was
    /// dropped, so we skip it next time we walk the queue.
    lru: VecDeque<PoolKey>,
    max: usize,
    stats: WsPoolStats,
}

/// Lifetime counters for the WS pool. Bumped on each interesting
/// path so an operator can grep provider tracing output and see
/// how often the silent-reconnect machinery kicked in (or, more
/// importantly, *kept* kicking in for a session — a runaway count
/// is the signature of an upstream regression).
#[derive(Clone, Copy, Debug, Default)]
pub struct WsPoolStats {
    /// Fresh sockets opened (pool miss, age-out, bearer-rotate, or
    /// the silent-reconnect path below).
    pub upgrades: u64,
    /// Cached sockets that died mid-turn and triggered the silent
    /// reopen-and-replay-without-chain-id recovery.
    pub silent_reconnects: u64,
    /// Times the fresh-socket path stripped a `previous_response_id`
    /// from the outgoing request because the new socket's chain
    /// cache was empty by definition.
    pub chain_strips_on_fresh: u64,
}

impl WsPool {
    pub fn new() -> Self {
        let max = std::env::var(POOL_MAX_ENV)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_POOL_MAX);
        Self {
            conns: HashMap::new(),
            lru: VecDeque::new(),
            max,
            stats: WsPoolStats::default(),
        }
    }

    /// Snapshot the running counters. Cheap (`Copy`); intended for
    /// tracing emission and tests.
    pub fn stats(&self) -> WsPoolStats {
        self.stats
    }

    /// Look up an existing connection for `key`, validating its
    /// bearer/age against the current request. Returns:
    ///
    /// - `Some(conn)` — caller owns it for the turn, must call
    ///   [`Self::release`] on success or drop on failure.
    /// - `None` — pool miss. Caller should `connect()` a fresh `WsConn` and
    ///   insert it via [`Self::release`] after the turn.
    ///
    /// Drops the entry if its bearer has rotated (OAuth refresh) or
    /// the connection is approaching the server-side age limit.
    pub fn checkout(&mut self, key: &PoolKey, current_bearer: &str) -> Option<WsConn> {
        let conn = self.conns.remove(key)?;
        // Bearer rotation: refreshed access token means upstream
        // would reject the existing socket on the next message
        // anyway. Drop and let caller reopen with the new token.
        if conn.bearer != current_bearer {
            self.purge_key(key);
            return None;
        }
        // Age-out: a 59-minute-old socket would die mid-stream.
        // Reopen here instead, before sending anything.
        if MAX_CONNECTION_AGE <= conn.opened_at.elapsed() {
            self.purge_key(key);
            return None;
        }
        // LRU bookkeeping: take the key out — caller will put it
        // back at the front on `release`.
        self.lru.retain(|k| k != key);
        Some(conn)
    }

    /// Put a connection (newly opened or just-used) back into the
    /// pool. Inserts at the LRU front. Evicts the LRU tail when the
    /// pool was already at capacity.
    pub fn release(&mut self, key: PoolKey, conn: WsConn) {
        if self.conns.len() >= self.max && !self.conns.contains_key(&key) {
            self.evict_lru();
        }
        // Lazy-prune: if a stale copy of this key is somewhere in
        // the queue (e.g. it was age-purged earlier), drop it so we
        // don't double-count.
        self.lru.retain(|k| k != &key);
        self.lru.push_front(key.clone());
        self.conns.insert(key, conn);
    }

    /// Number of cached connections currently retained by the pool.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.conns.len()
    }

    fn purge_key(&mut self, key: &PoolKey) {
        self.conns.remove(key);
        self.lru.retain(|k| k != key);
    }

    fn evict_lru(&mut self) {
        // Walk the LRU tail forward until we find a key still
        // backed by the map. Stale keys (entry removed earlier
        // without queue update) are silently skipped.
        while let Some(stale) = self.lru.pop_back() {
            if self.conns.remove(&stale).is_some() {
                return;
            }
        }
    }
}

impl Default for WsPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe WS pool wrapper used by prompt workers.
///
/// The inner mutex protects only pool bookkeeping. A per-key busy set reserves
/// a conversation chain while its network turn is in flight, so concurrent
/// same-key callers wait for that turn to release/drop the socket instead of
/// opening a second socket for the same chain. Different keys can still run
/// their network turns concurrently.
pub struct SharedWsPool {
    inner: Mutex<SharedWsPoolInner>,
    changed: Condvar,
}

struct SharedWsPoolInner {
    pool: WsPool,
    busy: HashSet<PoolKey>,
}

impl SharedWsPool {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SharedWsPoolInner {
                pool: WsPool::new(),
                busy: HashSet::new(),
            }),
            changed: Condvar::new(),
        }
    }

    pub fn stats(&self) -> Option<WsPoolStats> {
        self.inner.lock().ok().map(|inner| inner.pool.stats())
    }

    /// Try to reserve `key` without waiting for an active same-key turn. This
    /// is used by best-effort prewarm requests that run on the provider
    /// main loop: if a real turn already owns the reservation, prewarm
    /// should skip rather than delaying cancellation, worker output,
    /// PromptDone, or ACK handling.
    fn try_checkout(
        &self,
        key: &PoolKey,
        current_bearer: &str,
    ) -> Result<TryCheckout, WsTurnError> {
        let mut inner = self.lock_inner()?;
        if inner.busy.contains(key) {
            return Ok(TryCheckout::Busy);
        }
        inner.busy.insert(key.clone());
        Ok(TryCheckout::Reserved(
            inner.pool.checkout(key, current_bearer),
        ))
    }

    /// Reserve `key`, aborting promptly if `should_abort` becomes true while a
    /// same-key worker owns the reservation. This is used by prompt turns so a
    /// targeted cancel cannot leave a worker blocked in the pool and then later
    /// send a stale network request after the canceled turn releases.
    fn checkout_until(
        &self,
        key: &PoolKey,
        current_bearer: &str,
        should_abort: &mut impl FnMut() -> bool,
    ) -> Result<Option<WsConn>, WsTurnError> {
        let mut inner = self.lock_inner()?;
        while inner.busy.contains(key) {
            if should_abort() {
                return Err(WsTurnError::Canceled);
            }
            let (guard, _) = self
                .changed
                .wait_timeout(inner, CHECKOUT_ABORT_POLL)
                .map_err(pool_poisoned)?;
            inner = guard;
        }
        if should_abort() {
            return Err(WsTurnError::Canceled);
        }
        inner.busy.insert(key.clone());
        Ok(inner.pool.checkout(key, current_bearer))
    }

    fn release(&self, key: PoolKey, conn: WsConn) -> Result<(), WsTurnError> {
        let mut inner = self.lock_inner()?;
        inner.pool.release(key.clone(), conn);
        inner.busy.remove(&key);
        self.changed.notify_all();
        Ok(())
    }

    fn abandon(&self, key: &PoolKey) -> Result<(), WsTurnError> {
        let mut inner = self.lock_inner()?;
        inner.busy.remove(key);
        self.changed.notify_all();
        Ok(())
    }

    fn bump_silent_reconnects(&self) -> Result<u64, WsTurnError> {
        let mut inner = self.lock_inner()?;
        inner.pool.stats.silent_reconnects += 1;
        Ok(inner.pool.stats.silent_reconnects)
    }

    fn record_fresh_open(&self, previous_response: bool) -> Result<WsPoolStats, WsTurnError> {
        let mut inner = self.lock_inner()?;
        inner.pool.stats.upgrades += 1;
        if previous_response {
            inner.pool.stats.chain_strips_on_fresh += 1;
        }
        Ok(inner.pool.stats)
    }

    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, SharedWsPoolInner>, WsTurnError> {
        self.inner.lock().map_err(pool_poisoned)
    }
}

impl Default for SharedWsPool {
    fn default() -> Self {
        Self::new()
    }
}

fn pool_poisoned<T>(error: std::sync::PoisonError<T>) -> WsTurnError {
    WsTurnError::Other(LlmError::HttpStatus(
        0,
        format!("WS pool poisoned: {error}"),
    ))
}

enum TryCheckout {
    Reserved(Option<WsConn>),
    Busy,
}

/// WS dispatch failed in a way the caller can classify.
#[derive(Debug)]
pub enum WsTurnError {
    Canceled,
    Other(LlmError),
}

impl WsTurnError {
    pub fn into_llm_error(self) -> LlmError {
        match self {
            Self::Canceled => LlmError::HttpStatus(499, "cancelled by harness".to_owned()),
            Self::Other(error) => error,
        }
    }
}

/// Test-only convenience wrapper that wires `checkout` → `WsConn::run_turn` →
/// `release` together with reopen-on-miss semantics without the production
/// mutex wrapper.
///
/// Transparent reconnect: the Codex WS endpoint's
/// `previous_response_id` cache is **connection-local** (per the
/// OpenAI deployment-checklist WS guide). A fresh socket from
/// `WsConn::connect` has an empty chain cache, so a `previous_response_id`
/// carried in `request` would 404 on the server. The recovery path strips the
/// chain id, replays the full prompt once over the new WS, and releases that
/// socket back into the pool so the following turn is warm again.
#[cfg(test)]
pub fn run_turn_through_pool(
    pool: &mut WsPool,
    config: &ResponsesConfig,
    session_id: &str,
    session_prompt_id: &str,
    request: &crate::common::PromptPayload<'_>,
    on_update: &mut impl FnMut(&str, Option<&str>),
) -> Result<crate::common::StreamState, WsTurnError> {
    let key = PoolKey::for_request(config, session_id, request.originator);

    // First attempt: prefer a warm cached connection so the
    // connection-local chain cache stays useful.
    if let Some(mut conn) = pool.checkout(&key, &config.api_key) {
        match conn.run_turn(config, session_prompt_id, request, on_update) {
            Ok(state) => {
                pool.release(key, conn);
                return Ok(state);
            }
            Err(err) if is_recoverable_ws_error(&err) => {
                pool.stats.silent_reconnects += 1;
                tracing::info!(
                    target: crate::LOG_TARGET,
                    session_id,
                    error = %err,
                    silent_reconnects = pool.stats.silent_reconnects,
                    "Codex WS connection lost mid-turn",
                );
                drop(conn);
                // Fall through to the fresh-open path below. If this was a
                // chained turn, the fresh request will strip the stale
                // connection-local id and rebuild WS warmth with one full
                // replay.
            }
            Err(other) => {
                drop(conn);
                return Err(WsTurnError::Other(other));
            }
        }
    }

    // Fresh socket path. The chain cache here is empty by definition, so strip
    // any prior `previous_response_id` and pay one cold full replay on WS. That
    // is cheaper over the next turns than switching to HTTP and staying cold.
    let mut conn = WsConn::connect(config).map_err(WsTurnError::Other)?;
    pool.stats.upgrades += 1;
    let fresh_request = without_previous_response(request);
    if request.previous_response.is_some() {
        pool.stats.chain_strips_on_fresh += 1;
        tracing::debug!(
            target: crate::LOG_TARGET,
            session_id,
            upgrades = pool.stats.upgrades,
            chain_strips_on_fresh = pool.stats.chain_strips_on_fresh,
            "fresh Codex WS socket; stripping previous_response_id from outgoing request",
        );
    }
    match conn.run_turn(config, session_prompt_id, &fresh_request, on_update) {
        Ok(state) => {
            pool.release(key, conn);
            Ok(state)
        }
        Err(err) => {
            drop(conn);
            Err(WsTurnError::Other(err))
        }
    }
}

/// Thread-safe prompt-worker entry point. Shared-pool bookkeeping is locked
/// only while checking out/reserving a key, updating stats, or releasing a
/// successful connection. The network turn runs without the lock, so unrelated
/// prompt workers can use their own pooled sockets concurrently; same-key
/// callers wait on the reservation to preserve one chain per socket.
pub fn run_turn_through_shared_pool(
    pool: &SharedWsPool,
    config: &ResponsesConfig,
    session_prompt_id: &str,
    request: &crate::common::PromptPayload<'_>,
    should_abort: &mut impl FnMut() -> bool,
    on_update: &mut impl FnMut(&str, Option<&str>),
) -> Result<crate::common::StreamState, WsTurnError> {
    let session_id = request.session_id.as_str();
    let key = PoolKey::for_request(config, session_id, request.originator);

    if let Some(mut conn) = pool.checkout_until(&key, &config.api_key, should_abort)? {
        if should_abort() {
            pool.release(key, conn)?;
            return Err(WsTurnError::Canceled);
        }
        match conn.run_turn(config, session_prompt_id, request, on_update) {
            Ok(state) => {
                pool.release(key, conn)?;
                return Ok(state);
            }
            Err(err) if is_recoverable_ws_error(&err) => {
                let silent_reconnects = pool.bump_silent_reconnects()?;
                tracing::info!(
                    target: crate::LOG_TARGET,
                    session_id,
                    error = %err,
                    silent_reconnects,
                    "Codex WS connection lost mid-turn",
                );
                drop(conn);
            }
            Err(other) => {
                drop(conn);
                pool.abandon(&key)?;
                return Err(WsTurnError::Other(other));
            }
        }
    }

    if should_abort() {
        pool.abandon(&key)?;
        return Err(WsTurnError::Canceled);
    }
    let mut conn = match WsConn::connect(config) {
        Ok(conn) => conn,
        Err(error) => {
            pool.abandon(&key)?;
            return Err(WsTurnError::Other(error));
        }
    };
    if should_abort() {
        drop(conn);
        pool.abandon(&key)?;
        return Err(WsTurnError::Canceled);
    }
    let stats = pool.record_fresh_open(request.previous_response.is_some())?;
    if request.previous_response.is_some() {
        tracing::debug!(
            target: crate::LOG_TARGET,
            session_id,
            upgrades = stats.upgrades,
            chain_strips_on_fresh = stats.chain_strips_on_fresh,
            "fresh Codex WS socket; stripping previous_response_id from outgoing request",
        );
    }
    let fresh_request = without_previous_response(request);
    match conn.run_turn(config, session_prompt_id, &fresh_request, on_update) {
        Ok(state) => {
            pool.release(key, conn)?;
            Ok(state)
        }
        Err(err) => {
            drop(conn);
            pool.abandon(&key)?;
            Err(WsTurnError::Other(err))
        }
    }
}

/// Send a best-effort non-generating prewarm over the same pooled WS
/// connection a later real turn for this session will use. Unlike
/// real turns, a failed cached socket is simply dropped and retried
/// once on a fresh socket; no stateful chain id exists on prewarm.
#[cfg(test)]
pub fn run_prewarm_through_pool(
    pool: &mut WsPool,
    config: &ResponsesConfig,
    session_id: &str,
    request: &crate::common::PromptPayload<'_>,
) -> Result<crate::common::StreamState, LlmError> {
    let key = PoolKey::for_request(config, session_id, request.originator);

    if let Some(mut conn) = pool.checkout(&key, &config.api_key) {
        match conn.run_prewarm(config, request) {
            Ok(state) => {
                pool.release(key, conn);
                return Ok(state);
            }
            Err(err) if is_recoverable_ws_error(&err) => {
                pool.stats.silent_reconnects += 1;
                tracing::info!(
                    target: crate::LOG_TARGET,
                    session_id,
                    error = %err,
                    "Codex WS connection lost during prewarm; reopening",
                );
                drop(conn);
            }
            Err(other) => {
                drop(conn);
                return Err(other);
            }
        }
    }

    let mut conn = WsConn::connect(config)?;
    pool.stats.upgrades += 1;
    match conn.run_prewarm(config, request) {
        Ok(state) => {
            pool.release(key, conn);
            Ok(state)
        }
        Err(err) => {
            drop(conn);
            Err(err)
        }
    }
}

/// Thread-safe prewarm entry point. It reserves only the matching key while the
/// network prewarm is in flight, so prompt workers on other keys can continue
/// to check out/release pooled sockets concurrently. If that key is already
/// reserved, prewarm is skipped because it is best-effort main-loop work.
pub fn run_prewarm_through_shared_pool(
    pool: &SharedWsPool,
    config: &ResponsesConfig,
    session_id: &str,
    request: &crate::common::PromptPayload<'_>,
) -> Result<crate::common::StreamState, LlmError> {
    let key = PoolKey::for_request(config, session_id, request.originator);

    if let TryCheckout::Reserved(cached) = pool
        .try_checkout(&key, &config.api_key)
        .map_err(WsTurnError::into_llm_error)?
    {
        if let Some(mut conn) = cached {
            match conn.run_prewarm(config, request) {
                Ok(state) => {
                    pool.release(key, conn)
                        .map_err(WsTurnError::into_llm_error)?;
                    return Ok(state);
                }
                Err(err) if is_recoverable_ws_error(&err) => {
                    pool.bump_silent_reconnects()
                        .map_err(WsTurnError::into_llm_error)?;
                    tracing::info!(
                        target: crate::LOG_TARGET,
                        session_id,
                        error = %err,
                        "Codex WS connection lost during prewarm; reopening",
                    );
                    drop(conn);
                }
                Err(other) => {
                    drop(conn);
                    pool.abandon(&key).map_err(WsTurnError::into_llm_error)?;
                    return Err(other);
                }
            }
        }
    } else {
        tracing::debug!(
            target: crate::LOG_TARGET,
            session_id,
            "skipping prompt prewarm: websocket pool key is busy",
        );
        return Ok(crate::common::StreamState::new());
    }

    let mut conn = match WsConn::connect(config) {
        Ok(conn) => conn,
        Err(error) => {
            pool.abandon(&key).map_err(WsTurnError::into_llm_error)?;
            return Err(error);
        }
    };
    pool.record_fresh_open(false)
        .map_err(WsTurnError::into_llm_error)?;
    match conn.run_prewarm(config, request) {
        Ok(state) => {
            pool.release(key, conn)
                .map_err(WsTurnError::into_llm_error)?;
            Ok(state)
        }
        Err(err) => {
            drop(conn);
            pool.abandon(&key).map_err(WsTurnError::into_llm_error)?;
            Err(err)
        }
    }
}

/// Errors from `WsConn::run_turn` that mean "this socket is dead,
/// but the *next* socket can probably serve the turn." Caller's job
/// is to reopen and retry once silently rather than letting the outer
/// retry loop burn a backoff on the same broken state.
///
/// Every `run_turn` error path lands here as `LlmError::HttpStatus(0,
/// "stream error: ...")` — and by construction every one of them
/// indicates "this socket can't serve another turn":
///
/// - Transport-level closes: tungstenite raised `ConnectionClosed`,
///   `AlreadyClosed`, or an IO break; the server sent a close frame mid-stream;
///   keepalive ping or turn-send failed write-side.
/// - Task-supervision failures: the per-conn reader or writer task exited or
///   got aborted — the socket they owned is gone.
/// - Server-level stale-chain: an `error` event whose message says the
///   `previous_response_id` we just sent doesn't exist on this socket. Same
///   root cause as a transport close — the previous socket carrying that chain
///   id is gone — just surfaced through the JSON event stream instead of a TCP
///   close.
///
/// The matcher is therefore deliberately broad: anything with the
/// `"stream error:"` prefix from `run_turn` is recoverable. The
/// alternative — a narrow allow-list — silently leaks any new
/// failure mode (e.g. the previous `"ws writer task gone"`) to the
/// outer retry loop, which burns a backoff sleep on the same dead
/// socket. Other `LlmError` variants and non-stream-prefixed bodies
/// fall through unchanged.
///
/// The one carve-out: account-level caps (usage_limit_reached, rate
/// limit, quota) reach us with the same prefix because they ride the
/// same `error` event, but the connection is fine — reopening just
/// burns a fresh upgrade against an upstream that's about to reject
/// every request the same way. Defer those to the outer classifier
/// (`LlmError::retry_after`), which returns `None` and surfaces the
/// error immediately.
fn is_recoverable_ws_error(err: &LlmError) -> bool {
    let LlmError::HttpStatus(0, body) = err else {
        return false;
    };
    if !body.starts_with("stream error:") {
        return false;
    }
    !crate::common::is_account_limit_body(body)
}

/// Borrow `request` but blank out its `previous_response`. Used on
/// the fresh-socket path where the chain id from a prior connection
/// is guaranteed invalid (connection-local cache).
fn without_previous_response<'a>(
    request: &crate::common::PromptPayload<'a>,
) -> crate::common::PromptPayload<'a> {
    crate::common::PromptPayload {
        previous_response: None,
        system_prompt: request.system_prompt,
        context_items: request.context_items,
        tools: request.tools,
        params: request.params,
        tool_choice: request.tool_choice,
        originator: request.originator,
        session_id: request.session_id,
        share_user_cache_key: request.share_user_cache_key,
    }
}

#[cfg(test)]
mod tests;
