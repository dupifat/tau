//! The harness-owned `skill` tool.
//!
//! The `skill` tool is registered against [`HARNESS_CONNECTION_ID`] in
//! [`Harness::register_harness_tools`] and dispatched inline (bypassing the
//! bus) by [`Harness::handle_skill_tool_call`]. It surfaces the skills the
//! harness already discovered at startup (`Harness::discovered_skills`),
//! so search and load don't touch the filesystem walker again.

use tau_proto::{
    AgentToolCall, CborValue, Event, ToolCallId, ToolDisplay, ToolDisplayStats, ToolDisplayStatus,
    ToolName, ToolRequest,
};

use crate::conversation::ConversationId;
use crate::discovery::DiscoveredSkill;
use crate::error::HarnessError;
use crate::harness::{HARNESS_CONNECTION_ID, Harness};
use crate::prompt::cbor_map_bool;

impl Harness {
    /// Register harness-owned tools (e.g. `skill`).
    pub(crate) fn register_harness_tools(&mut self) {
        let _ = self.registry.register(
            HARNESS_CONNECTION_ID,
            tau_proto::ToolSpec {
                name: ToolName::new("skill"),
                model_visible_name: None,
                description: Some(
                    "Discover and load skills — short, focused playbooks for \
                     specific tasks. The user has likely curated skills for \
                     workflows they care about, so reach for this tool early: \
                     before tackling any request that touches a tool, command, \
                     framework, or domain you are not deeply familiar with — or \
                     anything the user might have an opinionated way of doing. \
                     Most skills are NOT pre-advertised in <available_skills>, so \
                     a missing entry there is no reason to skip this tool. Pass \
                     one query string, or an array of plausible terms \
                     (\"commit\", \"git commit\", \"version control\"). If the \
                     search resolves to one skill, or one single-term match has \
                     exactly that skill name, the full skill is loaded; otherwise \
                     matching skill names and descriptions are returned. Query \
                     terms are trimmed and deduplicated."
                        .to_owned(),
                ),
                tool_type: tau_proto::ToolType::Function,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": ["string", "array"],
                            "items": {"type": "string"},
                            "description": "One or more keywords matched case-insensitively against skill names and descriptions. Single string or array of strings. Terms are trimmed and deduplicated."
                        },
                        "search_content": {
                            "type": "boolean",
                            "description": "When true, also search the skill body. Default false."
                        }
                    },
                    "required": ["query"]
                })),
                format: None,
                enabled_by_default: true,
                side_effects: tau_proto::ToolSideEffects::Pure,
            },
        );
    }

    /// Handle the harness-owned `skill` tool call inline.
    ///
    /// Searches by `query`, then auto-loads when the result is unambiguous:
    /// - one total match loads that skill;
    /// - one single-term query with an exact skill-name match loads that skill;
    /// - otherwise returns `{name, description}` matches.
    pub(crate) fn handle_skill_tool_call(
        &mut self,
        cid: &ConversationId,
        call: &AgentToolCall,
    ) -> Result<(), HarnessError> {
        let call_id: ToolCallId = call.id.clone();
        let tool_name = ToolName::new("skill");

        // Track the conversation mapping first so the published
        // request + result both attribute to this conversation's
        // session via `session_id_for_event`.
        self.tool_conversations.insert(call_id.clone(), cid.clone());
        self.pending_tool_names
            .insert(call_id.clone(), tool_name.clone());
        self.bump_tools_started_for(cid);
        self.publish_for_conversation(
            cid,
            Event::ToolRequest(ToolRequest {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                tool_type: call.tool_type,
                arguments: call.arguments.clone(),
                originator: tau_proto::PromptOriginator::User,
            }),
        );

        let result_event = self.handle_skill_query(&call_id, &tool_name, &call.arguments);

        // Publish, then drop the in-flight tracking — order matters:
        // `session_id_for_event` reads `tool_conversations` to
        // attribute the persisted record before we clear it.
        self.publish_for_conversation(cid, result_event);
        self.on_tool_call_complete(&call.id);
        self.clear_tool_call_tracking(call_id.as_str());

        Ok(())
    }

    fn read_skill_by_name(&self, call_id: &ToolCallId, tool_name: &ToolName, name: &str) -> Event {
        let Some(skill) = self.discovered_skills.get(name) else {
            let message = format!("unknown skill: {name}");
            return Event::ToolError(tau_proto::ToolError {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                display: Some(skill_error_display(name, &message)),
                message,
                details: None,
                originator: tau_proto::PromptOriginator::User,
            });
        };
        match std::fs::read_to_string(&skill.file_path) {
            Ok(content) => {
                let body = tau_skills::strip_frontmatter(&content);
                let mut display = skill_ok_display(name);
                display.stats = text_stats_for_skill(body);
                Event::ToolResult(tau_proto::ToolResult {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    result: CborValue::Map(vec![
                        (
                            CborValue::Text("name".to_owned()),
                            CborValue::Text(name.to_owned()),
                        ),
                        (
                            CborValue::Text("content".to_owned()),
                            CborValue::Text(body.to_owned()),
                        ),
                    ]),
                    display: Some(display),
                    originator: tau_proto::PromptOriginator::User,
                })
            }
            Err(e) => {
                let message = format!("failed to read skill file: {e}");
                Event::ToolError(tau_proto::ToolError {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    display: Some(skill_error_display(name, &message)),
                    message,
                    details: None,
                    originator: tau_proto::PromptOriginator::User,
                })
            }
        }
    }

    fn handle_skill_query(
        &self,
        call_id: &ToolCallId,
        tool_name: &ToolName,
        arguments: &CborValue,
    ) -> Event {
        let needles = match extract_skill_search_queries(arguments) {
            Ok(needles) => needles,
            Err(message) => {
                return Event::ToolError(tau_proto::ToolError {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    display: Some(skill_error_display("search:", &message)),
                    message,
                    details: None,
                    originator: tau_proto::PromptOriginator::User,
                });
            }
        };
        let search_content = cbor_map_bool(arguments, "search_content").unwrap_or(false);
        let hits = self.search_discovered_skills(&needles, search_content);

        if let Some(name) = self.skill_name_to_auto_load(&needles, &hits) {
            return self.read_skill_by_name(call_id, tool_name, &name);
        }

        self.skill_search_result(call_id, tool_name, &needles, search_content, hits)
    }

    fn skill_name_to_auto_load(
        &self,
        needles: &[String],
        hits: &[(usize, String, String)],
    ) -> Option<String> {
        if hits.len() == 1 {
            return Some(hits[0].1.clone());
        }
        if needles.len() == 1 {
            let needle = &needles[0];
            if let Some((_, name, _)) = hits.iter().find(|(_, name, _)| name == needle) {
                return Some(name.clone());
            }
        }
        None
    }

    fn skill_search_result(
        &self,
        call_id: &ToolCallId,
        tool_name: &ToolName,
        needles: &[String],
        search_content: bool,
        hits: Vec<(usize, String, String)>,
    ) -> Event {
        let scope_label = if search_content { " [content]" } else { "" };
        let queries_label = needles.join(" ");
        let display_args = format!("{queries_label}{scope_label}");

        let mut display = skill_ok_display(&display_args);
        display.stats = skill_search_stats(&hits);
        if hits.is_empty() {
            display.status_text = "ok: no matches".to_owned();
        }

        let matches = CborValue::Array(
            hits.into_iter()
                .map(|(hit_count, name, description)| {
                    CborValue::Map(vec![
                        (CborValue::Text("name".to_owned()), CborValue::Text(name)),
                        (
                            CborValue::Text("description".to_owned()),
                            CborValue::Text(description),
                        ),
                        (
                            CborValue::Text("hit_count".to_owned()),
                            CborValue::Integer((hit_count as u64).into()),
                        ),
                    ])
                })
                .collect(),
        );
        let queries_echo =
            CborValue::Array(needles.iter().map(|n| CborValue::Text(n.clone())).collect());
        Event::ToolResult(tau_proto::ToolResult {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            result: CborValue::Map(vec![
                (CborValue::Text("queries".to_owned()), queries_echo),
                (
                    CborValue::Text("search_content".to_owned()),
                    CborValue::Bool(search_content),
                ),
                (CborValue::Text("matches".to_owned()), matches),
            ]),
            display: Some(display),
            originator: tau_proto::PromptOriginator::User,
        })
    }

    /// Score each discovered skill by how many of `needles` match its
    /// name, description, and (when `search_content`) body. A skill
    /// that matches more terms is more likely the right answer when
    /// the agent fired several plausible spellings at the same time
    /// ("commit", "git commit", "version control"). Returns
    /// `(hit_count, name, description)` rows sorted by descending
    /// hit count, with ties broken by name for deterministic output.
    ///
    /// Needles are expected to already be lowercased.
    fn search_discovered_skills(
        &self,
        needles: &[String],
        search_content: bool,
    ) -> Vec<(usize, String, String)> {
        let mut hits: Vec<(usize, &tau_proto::SkillName, &DiscoveredSkill)> = self
            .discovered_skills
            .iter()
            .filter_map(|(name, skill)| {
                let lower_name = name.as_str().to_lowercase();
                let lower_desc = skill.description.to_lowercase();
                // Read the body at most once across all needles, and
                // only when at least one needle didn't match in the
                // name or description and the caller opted in.
                let mut body: Option<String> = None;
                let hit_count = needles
                    .iter()
                    .filter(|needle| {
                        if lower_name.contains(needle.as_str())
                            || lower_desc.contains(needle.as_str())
                        {
                            return true;
                        }
                        if !search_content {
                            return false;
                        }
                        let body = body.get_or_insert_with(|| {
                            std::fs::read_to_string(&skill.file_path)
                                .map(|s| s.to_lowercase())
                                .unwrap_or_else(|err| {
                                    tracing::warn!(
                                        skill = %name.as_str(),
                                        path = %skill.file_path.display(),
                                        error = %err,
                                        "skill body unreadable; treating as empty for content search",
                                    );
                                    String::new()
                                })
                        });
                        body.contains(needle.as_str())
                    })
                    .count();
                (hit_count > 0).then_some((hit_count, name, skill))
            })
            .collect();
        hits.sort_by(|(ac, an, _), (bc, bn, _)| {
            bc.cmp(ac).then_with(|| an.as_str().cmp(bn.as_str()))
        });
        hits.into_iter()
            .map(|(hit_count, name, skill)| {
                (
                    hit_count,
                    name.as_str().to_owned(),
                    skill.description.clone(),
                )
            })
            .collect()
    }
}

fn skill_ok_display(args: &str) -> ToolDisplay {
    ToolDisplay {
        args: args.to_owned(),
        status: ToolDisplayStatus::Success,
        status_text: "ok".to_owned(),
        ..Default::default()
    }
}

fn skill_error_display(args: &str, message: &str) -> ToolDisplay {
    let chip = error_chip_text(message);
    ToolDisplay {
        args: args.to_owned(),
        status: ToolDisplayStatus::Error,
        status_text: chip,
        ..Default::default()
    }
}

fn text_stats_for_skill(text: &str) -> ToolDisplayStats {
    if text.is_empty() {
        return ToolDisplayStats::default();
    }
    ToolDisplayStats {
        matches: None,
        lines: Some(text.lines().count() as u64),
        bytes: Some(text.len() as u64),
    }
}

fn skill_search_stats(matches: &[(usize, String, String)]) -> ToolDisplayStats {
    let output = matches
        .iter()
        .map(|(_, name, description)| format!("{name}: {description}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut stats = text_stats_for_skill(&output);
    stats.matches = Some(matches.len() as u64);
    stats
}

fn error_chip_text(message: &str) -> String {
    let first = message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.is_empty() {
        return "err".to_owned();
    }
    const MAX: usize = 64;
    let label = if first.chars().count() <= MAX {
        first.to_owned()
    } else {
        let mut s: String = first.chars().take(MAX.saturating_sub(1)).collect();
        s.push('…');
        s
    };
    format!("err: {label}")
}

/// Parse the `query` argument of a `skill` tool call into one-or-more
/// lowercased search needles. Accepts either a single string (one
/// needle) or an array of strings. Terms are trimmed, lowercased, and
/// deduplicated before matching. Returns a user-facing error message
/// string on missing/empty/malformed input.
fn extract_skill_search_queries(arguments: &CborValue) -> Result<Vec<String>, String> {
    let CborValue::Map(entries) = arguments else {
        return Err("missing required argument: query".to_owned());
    };
    let raw = entries
        .iter()
        .find_map(|(k, v)| match k {
            CborValue::Text(k) if k == "query" => Some(v),
            _ => None,
        })
        .ok_or_else(|| "missing required argument: query".to_owned())?;

    let raw_needles: Vec<String> = match raw {
        CborValue::Text(s) => vec![s.trim().to_lowercase()],
        CborValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    CborValue::Text(s) => out.push(s.trim().to_lowercase()),
                    _ => return Err("query array entries must all be strings".to_owned()),
                }
            }
            out
        }
        _ => {
            return Err("query must be a string or an array of strings".to_owned());
        }
    };

    let mut needles: Vec<String> = Vec::with_capacity(raw_needles.len());
    for needle in raw_needles.into_iter().filter(|n| !n.is_empty()) {
        if !needles.iter().any(|existing| existing == &needle) {
            needles.push(needle);
        }
    }
    if needles.is_empty() {
        return Err("query must include at least one non-empty term".to_owned());
    }
    Ok(needles)
}
