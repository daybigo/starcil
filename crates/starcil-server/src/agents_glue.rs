//! Agent registry: glues the pure lifecycle engine (starcil-agents) to panes.
//! The async layer calls `tick_agents` periodically; handlers below service the
//! agent.* and pane.report_* API families.

use crate::core::ServerCore;
use crate::hosttraits::{ReadFormat, ReadSource, TerminalHost};
use serde_json::{json, Value};
use starcil_agents::{
    AgentDetection, CompiledManifest, DecisionAuthority, DetectionSnapshot, EvaluationInput,
    IntegrationReport, LifecycleEngine, ProcessInfo, ReportedState, SystemClock,
};
use starcil_domain::{AgentStatus, PaneId};
use starcil_protocol::error::{ApiError, ErrorCode};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Default TTL for integration reports that do not carry their own.
const REPORT_TTL: Duration = Duration::from_secs(600);

pub struct PaneAgent {
    engine: LifecycleEngine<SystemClock>,
    /// Wall-clock origin aligned (at creation) with the engine's own clock.
    origin: Instant,
    pub agent_id: Option<String>,
    pub status: AgentStatus,
    pub state_change_seq: u64,
    pub seen: bool,
    pub session_ref: Option<Value>,
    /// Reporting sources seen, with the last sequence used (for auto-seq).
    sources: BTreeMap<String, u64>,
    last_seq: u64,
    last_change: Instant,
    /// First tick that saw an agent in the pane. Exit detection waits
    /// `AgentRegistry::exit_grace` from here, so a CLI that is still starting
    /// (`agent.start` typed the command a moment ago) is not read as gone.
    agent_since: Option<Instant>,
    /// Last known shell state: `Some(true)` at the prompt, `Some(false)` with
    /// a program running under it, `None` when the host cannot tell.
    pub shell_idle: Option<bool>,
}

impl PaneAgent {
    fn new(manifest: CompiledManifest) -> Self {
        let now = Instant::now();
        PaneAgent {
            engine: LifecycleEngine::new(SystemClock::default(), manifest),
            origin: now,
            agent_id: None,
            status: AgentStatus::Unknown,
            state_change_seq: 0,
            seen: true,
            session_ref: None,
            sources: BTreeMap::new(),
            last_seq: 0,
            last_change: now,
            agent_since: None,
            shell_idle: None,
        }
    }
}

/// How long a pane keeps its agent after the shell is first seen idle.
const EXIT_GRACE: Duration = Duration::from_secs(1);

pub struct AgentRegistry {
    pub panes: BTreeMap<PaneId, PaneAgent>,
    /// Shared manifest used for kind detection (engines carry their own copy).
    pub detector: CompiledManifest,
    /// Grace before an idle shell ends its agent (tests shorten it).
    pub exit_grace: Duration,
}

impl AgentRegistry {
    pub fn new() -> Self {
        AgentRegistry {
            panes: BTreeMap::new(),
            detector: CompiledManifest::bundled().expect("bundled agent manifest is valid"),
            exit_grace: EXIT_GRACE,
        }
    }

    fn entry(&mut self, pane: PaneId) -> &mut PaneAgent {
        self.panes.entry(pane).or_insert_with(|| {
            PaneAgent::new(CompiledManifest::bundled().expect("bundled agent manifest is valid"))
        })
    }

    pub fn mark_seen(&mut self, pane: PaneId) {
        if let Some(a) = self.panes.get_mut(&pane) {
            a.seen = true;
        }
    }

    pub fn drop_pane(&mut self, pane: PaneId) {
        self.panes.remove(&pane);
    }
}

fn to_status(s: starcil_agents::LifecycleState) -> AgentStatus {
    match s {
        starcil_agents::LifecycleState::Idle => AgentStatus::Idle,
        starcil_agents::LifecycleState::Working => AgentStatus::Working,
        starcil_agents::LifecycleState::Blocked => AgentStatus::Blocked,
        starcil_agents::LifecycleState::Done => AgentStatus::Done,
        starcil_agents::LifecycleState::Unknown => AgentStatus::Unknown,
    }
}

/// Launch program per agent kind for `agent start`.
pub fn launch_program(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "pi" => "pi",
        "claude" => "claude",
        "codex" => "codex",
        "gemini" => "gemini",
        "cursor" => "cursor-agent",
        "devin" => "devin",
        "agy" => "agy",
        "cline" => "cline",
        "omp" => "omp",
        "mastracode" => "mastracode",
        "opencode" => "opencode",
        "copilot" => "copilot",
        "kimi" => "kimi",
        "kiro" => "kiro",
        "droid" => "droid",
        "amp" => "amp",
        "grok" => "grok",
        "hermes" => "hermes",
        "kilo" => "kilo",
        "qodercli" => "qodercli",
        "maki" => "maki",
        _ => return None,
    })
}

/// What `agent.start`'s startup wait reads on every poll (`startup_probe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupProbe {
    /// An agent entry still claims the pane.
    pub present: bool,
    pub status: AgentStatus,
    /// `Some(true)` = the shell is back at its prompt, nothing runs under it.
    pub shell_idle: Option<bool>,
    /// The status comes from positive recognition (a screen rule or a hook
    /// report), or the kind has no screen rules so stability is the best
    /// evidence there is.
    pub recognized: bool,
}

impl StartupProbe {
    fn absent() -> Self {
        StartupProbe {
            present: false,
            status: AgentStatus::Unknown,
            shell_idle: None,
            recognized: false,
        }
    }
}

impl<H: TerminalHost> ServerCore<H> {
    /// Grace before an idle shell ends its agent (tests shorten it to zero).
    pub fn set_agent_exit_grace(&mut self, grace: Duration) {
        self.agents.exit_grace = grace;
    }

    /// One-lock snapshot for `agent.start`'s startup wait. The stability
    /// fallback (`screen_stability_window` is zero) calls any quiet screen
    /// `idle` the moment a known agent is assigned — including the bare shell
    /// that just echoed the launch command — so a kind that HAS screen rules
    /// only counts as recognized once a rule or a report classified it.
    pub fn startup_probe(&self, pane: PaneId) -> StartupProbe {
        let Some(a) = self.agents.panes.get(&pane) else {
            return StartupProbe::absent();
        };
        let Some(kind) = a.agent_id.as_deref() else {
            return StartupProbe::absent();
        };
        let recognized = match a.engine.explain() {
            Some(e) => {
                e.authority != DecisionAuthority::ConservativeFallback
                    || !self.agents.detector.has_screen_rules(kind)
            }
            None => false,
        };
        StartupProbe {
            present: true,
            status: a.status,
            shell_idle: a.shell_idle,
            recognized,
        }
    }

    /// Periodic evaluation of every pane's agent state. The async layer calls
    /// this every few hundred milliseconds and after reports.
    pub fn tick_agents(&mut self) {
        let pane_ids: Vec<PaneId> = self.model.panes.keys().copied().collect();
        let focused = self.focused_pane();
        for pane in pane_ids {
            let (terminal_id, alive, title, seq, text, processes, shell_cwd) = {
                let meta = match self.model.pane(pane) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let tid = meta.terminal_id.clone();
                let alive = self.host.is_alive(&tid);
                let title = self.host.terminal_title(&tid);
                let seq = self.host.change_seq(&tid);
                let text = self
                    .host
                    .read(&tid, ReadSource::Detection, 16, ReadFormat::Text)
                    .map(|r| r.text)
                    .unwrap_or_default();
                // What runs under the shell right now (None = host can't tell).
                let processes = self.host.descendant_process_names(&tid);
                // Where the shell is right now (None = host can't tell).
                let shell_cwd = self.host.shell_cwd(&tid);
                (tid, alive, title, seq, text, processes, shell_cwd)
            };
            let _ = terminal_id;
            // The pane's cwd follows the shell: the TUI's composer completes
            // paths against it and a restored session reopens there. Part of
            // the snapshot, so the revision moves; clients apply the event.
            if let Some(cwd) = shell_cwd {
                if let Ok(meta) = self.model.pane_mut(pane) {
                    if meta.cwd != cwd {
                        meta.cwd = cwd.clone();
                        self.model.bump();
                        self.pending_events.push((
                            "pane.cwd_changed".to_string(),
                            json!({"pane_id": pane.to_string(), "cwd": cwd}),
                        ));
                    }
                }
            }
            let is_focused_and_seen = self.has_foreground_client && pane == focused;
            // A shell with nothing running under it hosts no agent, whatever
            // its title still says (cmd keeps the last program's title): no
            // detection there, and a pane that had one saw its agent exit.
            let idle_shell = processes.as_ref().is_some_and(Vec::is_empty);

            // Kind detection happens against the shared detector before the
            // per-pane entry is mutably borrowed: OSC title first (Claude
            // Code announces itself), then the process names under the shell
            // (CLIs that never set a title).
            let needs_detection = self
                .agents
                .panes
                .get(&pane)
                .map(|a| a.agent_id.is_none())
                .unwrap_or(true);
            let detection: Option<AgentDetection> = if needs_detection && !idle_shell {
                let detector = &self.agents.detector;
                detector
                    .detect_agent(None, None, title.as_deref())
                    .or_else(|| {
                        processes
                            .iter()
                            .flatten()
                            .find_map(|name| detector.detect_agent(Some(name), None, None))
                    })
            } else {
                None
            };
            let exit_grace = self.agents.exit_grace;

            let entry = self.agents.entry(pane);
            if seq != entry.last_seq {
                entry.last_seq = seq;
                entry.last_change = Instant::now();
            }
            if is_focused_and_seen {
                entry.seen = true;
            }
            // Prompt vs. program: the TUI's composer yields the keyboard while
            // something runs in the pane. A plain event, no snapshot churn.
            let shell_idle = processes.as_ref().map(Vec::is_empty);
            if entry.shell_idle != shell_idle {
                entry.shell_idle = shell_idle;
                self.pending_events.push((
                    "pane.shell_idle_changed".to_string(),
                    json!({"pane_id": pane.to_string(), "idle": shell_idle}),
                ));
            }
            if entry.agent_id.is_none() {
                if let Some(d) = detection {
                    entry.agent_id = Some(d.agent_id.clone());
                    entry.agent_since = Some(Instant::now());
                    // The kind is part of every pane's snapshot, and TUI
                    // clients only refetch the snapshot when the revision
                    // moves: bump it so the in-pane composer hides as soon as
                    // the CLI is recognized, not on the next unrelated click.
                    self.model.bump();
                    self.pending_events.push((
                        "pane.agent_detected".to_string(),
                        json!({"pane_id": pane.to_string(), "agent": d.agent_id}),
                    ));
                }
            } else {
                // Agents that arrived through `agent.start` or a report get
                // their grace period from the first tick that sees them.
                let since = *entry.agent_since.get_or_insert_with(Instant::now);
                if idle_shell && since.elapsed() >= exit_grace {
                    // The CLI is gone: the pane is a plain shell again (the
                    // TUI brings its composer back on the next snapshot).
                    let kind = entry.agent_id.take();
                    entry.agent_since = None;
                    entry.session_ref = None;
                    if let Ok(meta) = self.model.pane_mut(pane) {
                        meta.agent_name = None;
                    }
                    self.model.bump();
                    self.pending_events.push((
                        "pane.agent_exited".to_string(),
                        json!({"pane_id": pane.to_string(), "agent": kind}),
                    ));
                }
            }
            let last_change_at = entry.last_change.duration_since(entry.origin);
            let agent_id = entry.agent_id.clone();
            let decision = entry.engine.evaluate(EvaluationInput {
                process: ProcessInfo { alive, foreground: true },
                agent_id: agent_id.as_deref(),
                screen: DetectionSnapshot { text: &text, change_seq: seq, last_change_at },
                seen: entry.seen,
            });
            let new_status = to_status(decision.state);
            if new_status != entry.status {
                entry.status = new_status;
                entry.state_change_seq += 1;
                if new_status == AgentStatus::Working {
                    entry.seen = is_focused_and_seen;
                }
                let seq_now = entry.state_change_seq;
                self.pending_events.push((
                    "pane.agent_status_changed".to_string(),
                    json!({
                        "pane_id": pane.to_string(),
                        "agent_status": new_status,
                        "state_change_seq": seq_now,
                    }),
                ));
            }
        }
    }

    /// Drop registry entries for panes that no longer exist.
    pub fn gc_agents(&mut self) {
        let model = &self.model;
        self.agents.panes.retain(|p, _| model.panes.contains_key(p));
    }

    /// Resolve an agent target: unique live agent name, or a pane id that
    /// currently hosts an agent.
    pub fn resolve_agent(&self, target: &str) -> Result<PaneId, ApiError> {
        if let Some(p) = self.model.resolve_agent_name(target) {
            return Ok(p);
        }
        if let Ok(p) = target.parse::<PaneId>() {
            self.model.pane(p)?;
            let hosts_agent = self
                .agents
                .panes
                .get(&p)
                .map(|a| a.agent_id.is_some())
                .unwrap_or(false);
            if hosts_agent {
                return Ok(p);
            }
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                format!("pane {p} does not currently host a recognized agent"),
            ));
        }
        Err(ApiError::not_found(format!("agent `{target}`")))
    }

    // ---- agent.* handlers ----

    pub fn agent_list_handler(&self) -> Result<Value, ApiError> {
        let mut agents = Vec::new();
        for (pane, a) in &self.agents.panes {
            if a.agent_id.is_some() && self.model.pane(*pane).is_ok() {
                agents.push(self.agent_info(*pane)?);
            }
        }
        Ok(json!({"type": "agent_list", "agents": agents}))
    }

    pub fn agent_get_handler(&self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        Ok(json!({"type": "agent_info", "agent": self.agent_info(pane)?}))
    }

    fn agent_target(&self, params: &Value) -> Result<PaneId, ApiError> {
        let target = params
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `target`"))?;
        self.resolve_agent(target)
    }

    pub fn agent_read_handler(&self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        let mut p = params.clone();
        p["pane_id"] = json!(pane.to_string());
        self.handle_readonly_pane_read(&p)
    }

    fn handle_readonly_pane_read(&self, params: &Value) -> Result<Value, ApiError> {
        // Same semantics as pane.read; kept separate so agent.read cannot
        // accidentally mutate focus/seen state.
        let source = match params.get("source").and_then(Value::as_str).unwrap_or("recent-unwrapped") {
            "visible" => ReadSource::Visible,
            "recent" => ReadSource::Recent,
            "recent-unwrapped" => ReadSource::RecentUnwrapped,
            "detection" => ReadSource::Detection,
            other => return Err(ApiError::invalid_params(format!("invalid source `{other}`"))),
        };
        let format = match params.get("format").and_then(Value::as_str).unwrap_or("text") {
            "text" => ReadFormat::Text,
            "ansi" => ReadFormat::Ansi,
            other => return Err(ApiError::invalid_params(format!("invalid format `{other}`"))),
        };
        let lines = params.get("lines").and_then(Value::as_u64).unwrap_or(0) as usize;
        let pane: PaneId = params["pane_id"].as_str().unwrap().parse().unwrap();
        self.read_terminal(pane, source, lines, format)
    }

    pub fn agent_send_keys_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        let mut p = params.clone();
        p["pane_id"] = json!(pane.to_string());
        self.handle("pane.send_keys", &p)
    }

    pub fn agent_prompt_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `text`"))?;
        let term = self.model.pane(pane)?.terminal_id.clone();
        // Paste honoring bracketed-paste mode, then Enter as a separate write.
        self.host
            .paste_text(&term, text)
            .and_then(|_| self.host.write_enter(&term))
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(json!({"type": "agent_prompted", "agent": self.agent_info(pane)?}))
    }

    pub fn agent_rename_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        if params.get("clear").and_then(Value::as_bool).unwrap_or(false) {
            self.model.pane_mut(pane)?.agent_name = None;
        } else {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::invalid_params("missing `name` (or pass clear:true)"))?;
            if !starcil_domain::ids::valid_agent_name(name) {
                return Err(ApiError::invalid_params(
                    "agent names must match [a-z][a-z0-9_-]{0,31}",
                ));
            }
            if let Some(existing) = self.model.resolve_agent_name(name) {
                if existing != pane {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        format!("agent name `{name}` is already used by {existing}"),
                    ));
                }
            }
            self.model.pane_mut(pane)?.agent_name = Some(name.to_string());
        }
        self.model.bump();
        Ok(json!({"type": "agent_info", "agent": self.agent_info(pane)?}))
    }

    pub fn agent_focus_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        let tid = self.model.tab_of_pane(pane)?;
        self.model.tab_mut(tid)?.focused_pane = pane;
        self.model
            .workspace_mut(starcil_domain::WorkspaceId(tid.workspace))?
            .focused_tab = tid;
        self.model.focused_workspace = starcil_domain::WorkspaceId(tid.workspace);
        self.model.bump();
        self.agents.mark_seen(pane);
        self.emit("pane.focused", json!({"pane_id": pane.to_string()}));
        Ok(json!({"type": "agent_info", "agent": self.agent_info(pane)?}))
    }

    pub fn agent_explain_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self.agent_target(params)?;
        self.tick_agents();
        let a = self
            .agents
            .panes
            .get(&pane)
            .ok_or_else(|| ApiError::not_found(format!("agent in pane {pane}")))?;
        let explanation = a
            .engine
            .explain()
            .map(|e| serde_json::to_value(e).unwrap())
            .unwrap_or(Value::Null);
        Ok(json!({
            "type": "agent_explain",
            "pane_id": pane.to_string(),
            "agent": a.agent_id,
            "explanation": explanation,
        }))
    }

    pub fn agent_start_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `name`"))?;
        if !starcil_domain::ids::valid_agent_name(name) {
            return Err(ApiError::invalid_params("agent names must match [a-z][a-z0-9_-]{0,31}"));
        }
        if self.model.resolve_agent_name(name).is_some() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                format!("agent name `{name}` is already in use"),
            ));
        }
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `kind`"))?;
        let program = launch_program(kind)
            .ok_or_else(|| ApiError::invalid_params(format!("unknown agent kind `{kind}`")))?;
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("agent start requires --pane"))?;
        self.model.pane(pane)?;
        if self
            .agents
            .panes
            .get(&pane)
            .map(|a| a.agent_id.is_some())
            .unwrap_or(false)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                format!("pane {pane} already hosts an agent"),
            ));
        }
        let args: Vec<String> = params
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let mut command = program.to_string();
        for a in &args {
            command.push(' ');
            // Quote args containing spaces so the shell keeps them together.
            if a.contains(' ') {
                command.push('"');
                command.push_str(a);
                command.push('"');
            } else {
                command.push_str(a);
            }
        }
        let term = self.model.pane(pane)?.terminal_id.clone();
        self.host
            .write_text(&term, &command)
            .and_then(|_| self.host.write_enter(&term))
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        self.model.pane_mut(pane)?.agent_name = Some(name.to_string());
        let entry = self.agents.entry(pane);
        entry.agent_id = Some(kind.to_string());
        self.model.bump();
        self.emit("pane.agent_detected", json!({"pane_id": pane.to_string(), "agent": kind}));
        Ok(json!({"type": "agent_started", "agent": self.agent_info(pane)?}))
    }

    // ---- pane.report_* handlers ----

    pub fn report_agent_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        self.model.pane(pane)?;
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `source`"))?
            .to_string();
        crate::metadata::validate_source(&source)?;
        let state = params
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `state`"))?;
        let agent_label = params.get("agent").and_then(Value::as_str).map(str::to_string);
        let message = params.get("message").and_then(Value::as_str).map(str::to_string);
        let screen_change_seq = {
            let term = self.model.pane(pane)?.terminal_id.clone();
            self.host.change_seq(&term)
        };
        let entry = self.agents.entry(pane);
        if let Some(label) = &agent_label {
            if entry.agent_id.is_none() {
                entry.agent_id = Some(label.clone());
            }
        }
        let accepted = match state {
            "unknown" => {
                entry.engine.release_source(&source);
                entry.sources.remove(&source);
                true
            }
            s => {
                let reported = match s {
                    "idle" => ReportedState::Idle,
                    "working" => ReportedState::Working,
                    "blocked" => ReportedState::Blocked,
                    other => {
                        return Err(ApiError::invalid_params(format!(
                            "state must be idle|working|blocked|unknown, got `{other}`"
                        )))
                    }
                };
                let seq = params.get("seq").and_then(Value::as_u64).unwrap_or_else(|| {
                    entry.sources.get(&source).copied().unwrap_or(0) + 1
                });
                entry.sources.insert(source.clone(), seq);
                let acceptance = entry.engine.accept_report(
                    IntegrationReport {
                        source: source.clone(),
                        state: reported,
                        seq,
                        ttl: REPORT_TTL,
                        message,
                    },
                    screen_change_seq,
                );
                matches!(acceptance, starcil_agents::ReportAcceptance::Accepted)
            }
        };
        self.store_session_ref(pane, params, &source, agent_label.as_deref());
        self.tick_agents();
        Ok(json!({"type": "agent_reported", "accepted": accepted}))
    }

    pub fn report_agent_session_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        self.model.pane(pane)?;
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `source`"))?
            .to_string();
        crate::metadata::validate_source(&source)?;
        let agent_label = params.get("agent").and_then(Value::as_str).map(str::to_string);
        self.store_session_ref(pane, params, &source, agent_label.as_deref());
        Ok(json!({"type": "agent_session_reported"}))
    }

    fn store_session_ref(&mut self, pane: PaneId, params: &Value, source: &str, agent: Option<&str>) {
        let (kind, value) = if let Some(id) = params.get("agent_session_id").and_then(Value::as_str) {
            ("id", id.to_string())
        } else if let Some(path) = params.get("agent_session_path").and_then(Value::as_str) {
            ("path", path.to_string())
        } else {
            return;
        };
        let entry = self.agents.entry(pane);
        entry.session_ref = Some(json!({
            "source": source,
            "agent": agent.or(entry.agent_id.as_deref()).unwrap_or("unknown"),
            "kind": kind,
            "value": value,
        }));
    }

    pub fn release_agent_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `source`"))?;
        if let Some(entry) = self.agents.panes.get_mut(&pane) {
            entry.engine.release_source(source);
            entry.sources.remove(source);
        }
        self.tick_agents();
        Ok(json!({"type": "ok"}))
    }

    pub fn clear_agent_authority_handler(&mut self, params: &Value) -> Result<Value, ApiError> {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        if let Some(entry) = self.agents.panes.get_mut(&pane) {
            let sources: Vec<String> = entry.sources.keys().cloned().collect();
            for s in sources {
                entry.engine.release_source(&s);
            }
            entry.sources.clear();
        }
        self.tick_agents();
        Ok(json!({"type": "ok"}))
    }

    pub fn agent_info(&self, pane: PaneId) -> Result<Value, ApiError> {
        let a = self
            .agents
            .panes
            .get(&pane)
            .ok_or_else(|| ApiError::not_found(format!("agent in pane {pane}")))?;
        let mut info = serde_json::to_value(self.pane_info(pane)?).unwrap();
        info["agent"] = json!(a.agent_id);
        info["agent_status"] = json!(a.status);
        info["state_change_seq"] = json!(a.state_change_seq);
        if let Some(name) = &self.model.pane(pane)?.agent_name {
            info["name"] = json!(name);
        }
        if let Some(r) = &a.session_ref {
            info["agent_session"] = r.clone();
        }
        Ok(info)
    }
}
