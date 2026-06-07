//! Rhai scripting extension for trusted local Tau event hooks.
//!
//! The extension keeps Tau protocol handling in Rust and exposes delivered
//! events to Rhai scripts as JSON-shaped maps matching Serde's JSON form.

use std::error::Error;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;

use rhai::{Array, Dynamic, Engine, ImmutableString, Map, Scope};
use serde::Deserialize;
use tau_proto::{
    Ack, ClientKind, ConfigError, Configure, Event, EventLogSeq, EventSelector, HarnessInfo,
    HarnessInfoLevel, HarnessInputMessage, HarnessOutputMessage, Hello, Intercept, InterceptAction,
    InterceptReply, InterceptionPriority, PROTOCOL_VERSION, PeerInputReader, PeerOutputWriter,
    Ready, Subscribe, UnixMicros,
};

/// `tracing` target for events emitted from this extension.
pub const LOG_TARGET: &str = "rhai";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ExtConfig {
    /// Rhai script path. Absolute paths are preferred; relative paths are
    /// resolved by the extension process current working directory.
    script: Option<PathBuf>,
    /// JSON-compatible user variables passed to `init(config)`.
    vars: serde_json::Value,
    /// Script execution limits.
    limits: Limits,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    /// Maximum Rhai operations before aborting a callback.
    max_operations: Option<u64>,
    /// Maximum nested Rhai function calls.
    max_call_levels: Option<usize>,
    /// Maximum expression nesting depth during parsing. Zero disables the
    /// limit.
    max_expr_depth: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct InitOutput {
    subscribe: Vec<EventSelector>,
    intercept: Vec<InitIntercept>,
    ready_message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitIntercept {
    selectors: Vec<EventSelector>,
    priority: InterceptionPriority,
}

struct ScriptRuntime {
    engine: Engine,
    ast: rhai::AST,
    scope: Scope<'static>,
}

/// Run the extension over stdio.
pub fn run_stdio() -> Result<(), Box<dyn Error>> {
    tau_extension::init_logging_for(LOG_TARGET);
    run(std::io::stdin(), std::io::stdout())
}

/// Run the extension over the supplied reader/writer pair.
pub fn run<R, W>(reader: R, writer: W) -> Result<(), Box<dyn Error>>
where
    R: Read,
    W: Write + Send + 'static,
{
    let mut reader = PeerInputReader::new(BufReader::new(reader));
    let mut writer = PeerOutputWriter::new(BufWriter::new(writer));

    writer.write_message(&HarnessInputMessage::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client_name: tau_proto::ExtensionName::new("tau-ext-rhai"),
        client_kind: ClientKind::Tool,
    }))?;
    writer.flush()?;

    let Some(configure) = read_initial_config(&mut reader)? else {
        return Ok(());
    };

    let (tx, rx) = mpsc::channel::<HarnessInputMessage>();
    let writer_handle = std::thread::spawn(move || -> Result<(), Box<dyn Error + Send>> {
        for message in rx {
            writer
                .write_message(&message)
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
            writer
                .flush()
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
        }
        Ok(())
    });

    let mut runtime = match load_runtime(&configure, tx.clone()) {
        Ok((mut runtime, init, config_json)) => {
            send_init_messages(&tx, init)?;
            runtime.start(config_json, &tx);
            Some(runtime)
        }
        Err(message) => {
            tracing::warn!(target: LOG_TARGET, error = %message, "rhai disabled");
            send_config_error_ready(&tx, message)?;
            None
        }
    };

    while let Some(message) = reader.read_message()? {
        match message {
            HarnessOutputMessage::Deliver(delivery) => {
                let (event, seq, recorded_at) = delivery.into_parts();
                if let Some(runtime) = runtime.as_mut() {
                    runtime.on_event(event, seq, recorded_at, &tx);
                }
                if let Some(up_to) = seq {
                    let _ = tx.send(HarnessInputMessage::Ack(Ack { up_to }));
                }
            }
            HarnessOutputMessage::InterceptRequest(req) => {
                let action = runtime
                    .as_mut()
                    .map(|runtime| runtime.on_intercept(*req.event, req.transient, &tx))
                    .unwrap_or_else(|| InterceptAction::Pass(None));
                let _ = tx.send(HarnessInputMessage::InterceptReply(InterceptReply {
                    action,
                }));
            }
            HarnessOutputMessage::Disconnect(_) => break,
            HarnessOutputMessage::Configure(_) => {}
            _ => {}
        }
    }

    drop(runtime);
    drop(tx);
    writer_handle
        .join()
        .map_err(|e| -> Box<dyn Error> { format!("writer thread panicked: {e:?}").into() })?
        .map_err(|e| -> Box<dyn Error> { e })?;
    Ok(())
}

fn read_initial_config<R: Read>(
    reader: &mut PeerInputReader<BufReader<R>>,
) -> Result<Option<Configure>, Box<dyn Error>> {
    while let Some(message) = reader.read_message()? {
        match message {
            HarnessOutputMessage::Configure(configure) => return Ok(Some(configure)),
            HarnessOutputMessage::Disconnect(_) => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

fn load_runtime(
    configure: &Configure,
    tx: mpsc::Sender<HarnessInputMessage>,
) -> Result<(ScriptRuntime, InitOutput, serde_json::Value), String> {
    let cfg = tau_extension::parse_config::<ExtConfig>(&configure.config)?;
    let script = cfg
        .script
        .ok_or_else(|| "rhai script config field is required".to_owned())?;
    let source = fs::read_to_string(&script)
        .map_err(|e| format!("reading Rhai script {}: {e}", script.display()))?;

    let mut engine = Engine::new();
    let max_expr_depth = cfg.limits.max_expr_depth.unwrap_or(64);
    engine.set_max_expr_depths(max_expr_depth, max_expr_depth);
    if let Some(max) = cfg.limits.max_operations {
        engine.set_max_operations(max);
    }
    if let Some(max) = cfg.limits.max_call_levels {
        engine.set_max_call_levels(max);
    }
    let ast = engine
        .compile(&source)
        .map_err(|e| format!("compiling Rhai script {}: {e}", script.display()))?;
    let mut runtime = ScriptRuntime {
        engine,
        ast,
        scope: Scope::new(),
    };
    let config_json = init_config_json(&cfg.vars, configure.state_dir.as_ref());
    let init = runtime.init(config_json.clone())?;
    register_host_functions(&mut runtime.engine, tx);
    Ok((runtime, init, config_json))
}

fn init_config_json(vars: &serde_json::Value, state_dir: Option<&PathBuf>) -> serde_json::Value {
    serde_json::json!({
        "vars": vars,
        "state_dir": state_dir.map(|p| p.display().to_string()),
    })
}

fn send_init_messages(
    tx: &mpsc::Sender<HarnessInputMessage>,
    init: InitOutput,
) -> Result<(), Box<mpsc::SendError<HarnessInputMessage>>> {
    if !init.subscribe.is_empty() {
        tx.send(HarnessInputMessage::Subscribe(Subscribe {
            selectors: init.subscribe,
        }))
        .map_err(Box::new)?;
    }
    for intercept in init.intercept {
        tx.send(HarnessInputMessage::Intercept(Intercept {
            selectors: intercept.selectors,
            priority: intercept.priority,
        }))
        .map_err(Box::new)?;
    }
    tx.send(HarnessInputMessage::Ready(Ready {
        message: Some(
            init.ready_message
                .unwrap_or_else(|| "rhai ready".to_owned()),
        ),
    }))
    .map_err(Box::new)?;
    Ok(())
}

fn normalize_init_output(mut init: InitOutput) -> Result<InitOutput, String> {
    let Some(first) = init.intercept.first() else {
        return Ok(init);
    };
    let priority = first.priority;
    let mut selectors = Vec::new();
    for intercept in std::mem::take(&mut init.intercept) {
        if intercept.priority != priority {
            return Err("init intercept entries must all use the same priority".to_owned());
        }
        selectors.extend(intercept.selectors);
    }
    init.intercept = vec![InitIntercept {
        selectors,
        priority,
    }];
    Ok(init)
}
fn send_config_error_ready(
    tx: &mpsc::Sender<HarnessInputMessage>,
    message: String,
) -> Result<(), Box<mpsc::SendError<HarnessInputMessage>>> {
    tx.send(HarnessInputMessage::ConfigError(ConfigError {
        message: message.clone(),
    }))
    .map_err(Box::new)?;
    tx.send(HarnessInputMessage::Ready(Ready {
        message: Some(format!("rhai disabled: {message}")),
    }))
    .map_err(Box::new)?;
    Ok(())
}

fn register_host_functions(engine: &mut Engine, tx: mpsc::Sender<HarnessInputMessage>) {
    let emit_tx = tx.clone();
    engine.register_fn("tau_emit", move |event: Dynamic| {
        enqueue_event(&emit_tx, event, false);
    });

    let emit_tx = tx.clone();
    engine.register_fn("tau_emit_transient", move |event: Dynamic| {
        enqueue_event(&emit_tx, event, true);
    });

    let info_tx = tx.clone();
    engine.register_fn("tau_info", move |message: ImmutableString| {
        enqueue_info(&info_tx, message.as_str(), HarnessInfoLevel::Normal, true);
    });

    let info_tx = tx.clone();
    engine.register_fn(
        "tau_info",
        move |message: ImmutableString, level: ImmutableString| {
            enqueue_info(
                &info_tx,
                message.as_str(),
                parse_info_level(level.as_str()),
                true,
            );
        },
    );

    engine.register_fn(
        "tau_log",
        move |level: ImmutableString, message: ImmutableString| match level.as_str() {
            "trace" => tracing::trace!(target: LOG_TARGET, message = %message, "rhai script log"),
            "debug" => tracing::debug!(target: LOG_TARGET, message = %message, "rhai script log"),
            "warn" => tracing::warn!(target: LOG_TARGET, message = %message, "rhai script log"),
            "error" => tracing::error!(target: LOG_TARGET, message = %message, "rhai script log"),
            _ => tracing::info!(target: LOG_TARGET, message = %message, "rhai script log"),
        },
    );
}

fn enqueue_event(tx: &mpsc::Sender<HarnessInputMessage>, event: Dynamic, transient: bool) {
    match dynamic_to_json(&event)
        .and_then(|value| serde_json::from_value::<Event>(value).map_err(|e| e.to_string()))
    {
        Ok(event) => {
            let _ = tx.send(HarnessInputMessage::emit_with_transient(event, transient));
        }
        Err(message) => {
            tracing::warn!(target: LOG_TARGET, error = %message, "script emitted invalid event");
            enqueue_info(
                tx,
                &format!("rhai invalid event: {message}"),
                HarnessInfoLevel::Important,
                true,
            );
        }
    }
}

fn enqueue_info(
    tx: &mpsc::Sender<HarnessInputMessage>,
    message: &str,
    level: HarnessInfoLevel,
    transient: bool,
) {
    let _ = tx.send(HarnessInputMessage::emit_with_transient(
        Event::HarnessInfo(HarnessInfo {
            message: message.to_owned(),
            level,
        }),
        transient,
    ));
}

fn parse_info_level(level: &str) -> HarnessInfoLevel {
    match level {
        "important" => HarnessInfoLevel::Important,
        _ => HarnessInfoLevel::Normal,
    }
}

impl ScriptRuntime {
    fn init(&mut self, config: serde_json::Value) -> Result<InitOutput, String> {
        if !self.has_function("init", 1) {
            return Ok(InitOutput::default());
        }
        match self.engine.call_fn::<Dynamic>(
            &mut self.scope,
            &self.ast,
            "init",
            (json_to_dynamic(&config)?,),
        ) {
            Ok(value) if value.is_unit() => Ok(InitOutput::default()),
            Ok(value) => dynamic_to_json(&value)
                .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string()))
                .and_then(normalize_init_output),
            Err(err) => Err(format!("running init: {err}")),
        }
    }

    fn start(&mut self, config: serde_json::Value, tx: &mpsc::Sender<HarnessInputMessage>) {
        if self.has_function("start", 1) {
            let config = match json_to_dynamic(&config) {
                Ok(config) => config,
                Err(message) => {
                    report_callback_error(tx, format!("preparing start config: {message}"));
                    return;
                }
            };
            match self
                .engine
                .call_fn::<Dynamic>(&mut self.scope, &self.ast, "start", (config,))
            {
                Ok(_) => {}
                Err(err) => report_callback_error(tx, format!("rhai start failed: {err}")),
            }
            return;
        }

        if !self.has_function("start", 0) {
            return;
        }
        match self
            .engine
            .call_fn::<Dynamic>(&mut self.scope, &self.ast, "start", ())
        {
            Ok(_) => {}
            Err(err) => report_callback_error(tx, format!("rhai start failed: {err}")),
        }
    }

    fn on_event(
        &mut self,
        event: Event,
        seq: Option<EventLogSeq>,
        recorded_at: Option<UnixMicros>,
        tx: &mpsc::Sender<HarnessInputMessage>,
    ) {
        let event = match serde_json::to_value(event)
            .map_err(|e| e.to_string())
            .and_then(|v| json_to_dynamic(&v))
        {
            Ok(event) => event,
            Err(message) => {
                report_callback_error(tx, format!("preparing on_event: {message}"));
                return;
            }
        };
        let meta = match json_to_dynamic(&meta_json(seq, recorded_at)) {
            Ok(meta) => meta,
            Err(message) => {
                report_callback_error(tx, format!("preparing on_event metadata: {message}"));
                return;
            }
        };
        if !self.has_function("on_event", 2) {
            return;
        }
        match self
            .engine
            .call_fn::<Dynamic>(&mut self.scope, &self.ast, "on_event", (event, meta))
        {
            Ok(_) => {}
            Err(err) => report_callback_error(tx, format!("rhai on_event failed: {err}")),
        }
    }

    fn on_intercept(
        &mut self,
        event: Event,
        transient: bool,
        tx: &mpsc::Sender<HarnessInputMessage>,
    ) -> InterceptAction {
        let event = match serde_json::to_value(event)
            .map_err(|e| e.to_string())
            .and_then(|v| json_to_dynamic(&v))
        {
            Ok(event) => event,
            Err(message) => {
                report_callback_error(tx, format!("preparing on_intercept: {message}"));
                return InterceptAction::Pass(None);
            }
        };
        if !self.has_function("on_intercept", 2) {
            return InterceptAction::Pass(None);
        }
        match self.engine.call_fn::<Dynamic>(
            &mut self.scope,
            &self.ast,
            "on_intercept",
            (event, transient),
        ) {
            Ok(value) => parse_intercept_action(value).unwrap_or_else(|message| {
                report_callback_error(tx, format!("invalid on_intercept result: {message}"));
                InterceptAction::Pass(None)
            }),
            Err(err) => {
                report_callback_error(tx, format!("rhai on_intercept failed: {err}"));
                InterceptAction::Pass(None)
            }
        }
    }

    fn has_function(&self, name: &str, params: usize) -> bool {
        self.ast
            .iter_functions()
            .any(|f| f.name == name && f.params.len() == params)
    }
}

fn report_callback_error(tx: &mpsc::Sender<HarnessInputMessage>, message: String) {
    tracing::warn!(target: LOG_TARGET, error = %message, "rhai callback failed");
    enqueue_info(tx, &message, HarnessInfoLevel::Important, true);
}

fn meta_json(seq: Option<EventLogSeq>, recorded_at: Option<UnixMicros>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    if let Some(seq) = seq {
        map.insert("seq".to_owned(), u64_meta_value(seq.get()));
    }
    if let Some(recorded_at) = recorded_at {
        map.insert("recorded_at".to_owned(), u64_meta_value(recorded_at.get()));
    }
    serde_json::Value::Object(map)
}

fn u64_meta_value(value: u64) -> serde_json::Value {
    if let Ok(value) = i64::try_from(value) {
        serde_json::Value::Number(value.into())
    } else {
        serde_json::Value::String(value.to_string())
    }
}
fn parse_intercept_action(value: Dynamic) -> Result<InterceptAction, String> {
    if value.is_unit() {
        return Ok(InterceptAction::Pass(None));
    }
    if let Some(s) = value.clone().try_cast::<ImmutableString>() {
        return match s.as_str() {
            "pass" => Ok(InterceptAction::Pass(None)),
            "drop" => Ok(InterceptAction::Drop),
            other => Err(format!("unknown action string `{other}`")),
        };
    }
    let json = dynamic_to_json(&value)?;
    let obj = json
        .as_object()
        .ok_or_else(|| "result must be (), string, or map".to_owned())?;
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "result map needs string `kind`".to_owned())?;
    match kind {
        "pass" => {
            let event = obj
                .get("event")
                .cloned()
                .map(serde_json::from_value::<Event>)
                .transpose()
                .map_err(|e| e.to_string())?;
            Ok(InterceptAction::Pass(event.map(Box::new)))
        }
        "drop" => Ok(InterceptAction::Drop),
        other => Err(format!("unknown action kind `{other}`")),
    }
}

fn json_to_dynamic(value: &serde_json::Value) -> Result<Dynamic, String> {
    match value {
        serde_json::Value::Null => Ok(Dynamic::UNIT),
        serde_json::Value::Bool(v) => Ok(Dynamic::from_bool(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Dynamic::from_int(i as rhai::INT))
            } else if let Some(u) = n.as_u64() {
                if let Ok(i) = rhai::INT::try_from(u) {
                    Ok(Dynamic::from_int(i))
                } else {
                    Ok(Dynamic::from(u.to_string()))
                }
            } else if let Some(f) = n.as_f64() {
                Ok(Dynamic::from_float(f as rhai::FLOAT))
            } else {
                Err("unsupported JSON number".to_owned())
            }
        }
        serde_json::Value::String(v) => Ok(Dynamic::from(v.clone())),
        serde_json::Value::Array(values) => values
            .iter()
            .map(json_to_dynamic)
            .collect::<Result<Array, _>>()
            .map(Dynamic::from_array),
        serde_json::Value::Object(values) => {
            let mut map = Map::new();
            for (key, value) in values {
                map.insert(key.as_str().into(), json_to_dynamic(value)?);
            }
            Ok(Dynamic::from_map(map))
        }
    }
}

fn dynamic_to_json(value: &Dynamic) -> Result<serde_json::Value, String> {
    if value.is_unit() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(v) = value.clone().try_cast::<bool>() {
        return Ok(serde_json::Value::Bool(v));
    }
    if let Some(v) = value.clone().try_cast::<rhai::INT>() {
        return Ok(serde_json::Value::Number(v.into()));
    }
    if let Some(v) = value.clone().try_cast::<rhai::FLOAT>() {
        return serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| "float must be finite".to_owned());
    }
    if let Some(v) = value.clone().try_cast::<ImmutableString>() {
        return Ok(serde_json::Value::String(v.to_string()));
    }
    if let Some(values) = value.clone().try_cast::<Array>() {
        return values
            .iter()
            .map(dynamic_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array);
    }
    if let Some(values) = value.clone().try_cast::<Map>() {
        let mut map = serde_json::Map::new();
        for (key, value) in values {
            map.insert(key.to_string(), dynamic_to_json(&value)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(format!(
        "unsupported Rhai value type `{}`",
        value.type_name()
    ))
}

#[cfg(test)]
mod tests;
