//! Skills and MCP servers on demand: nothing of them reaches the model
//! until the user asks for it.
//!
//! - **Skills**: with `[skills] catalog = "manual"` (the default) no skill is
//!   model-invocable, so neither the `<available_skills>` catalog nor the
//!   `skill` tool is offered; `/name` (or `$name`) still loads a skill's full
//!   instructions into that turn.
//! - **MCP servers**: a chat offers the tools of the servers it activated
//!   (`@server`, `@plugin`, the active-server chips) plus `always` servers,
//!   minus `disabled_tools`. Activation is recorded in the session log only:
//!   the tools join through the agent's `tool-addition` developer message
//!   (deferred on DeepSeek's addition-only route, so the cached prefix stays
//!   valid), followed by a `user/message` with source `{kind:
//!   "mcp-activation", server}` that names the tools and carries the
//!   server's instructions. Deactivation writes `{kind: "mcp-deactivation"}`;
//!   on the addition-only route the declared tools stay (calls fail with a
//!   clear error), elsewhere they are removed.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Weak};

use dshx_agent::{PreStep, PreStepHook, ToolUpdate};
use dshx_core::config::Config;
use dshx_core::{
    ContentBlock, Message, MessageSource, Role, SessionEvent, Tool, ToolContext, ToolError,
    ToolOutput, ToolRegistry, ToolSchema, kinds,
};
use serde_json::{Value, json};

use super::Runtime;

/// dsh `FILE_REFERENCE_PROMPT` (dsh-file-reference), the system prompt
/// section `context:file-reference` at order 900 while `read` is offered.
pub const FILE_REFERENCE_PROMPT: &str = "Tokens prefixed with @ are paths the user explicitly referenced. Relative paths resolve from the workspace root; absolute paths identify files or directories on the host. A trailing slash marks a directory: list it when its contents matter. Anything else is a file: use the read tool when its contents are needed, and do not claim to have inspected it before reading. @\"...\" quotes a path containing spaces.";

/// Source kinds of the activation notes.
pub const ACTIVATION: &str = "mcp-activation";
pub const DEACTIVATION: &str = "mcp-deactivation";

/// A chat's explicit choice for one server; servers without one follow
/// their `always` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    On,
    Off,
}

/// What a session log says about MCP servers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpHistory {
    /// Explicit per-chat choices, newest wins.
    pub choices: BTreeMap<String, Choice>,
    /// Every tool name a request of this session declared.
    pub declared: HashSet<String>,
    /// `seq` of each server's latest activation note.
    pub notes: BTreeMap<String, u64>,
}

fn header_tools(event: &SessionEvent) -> Vec<String> {
    event.data["header"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl McpHistory {
    /// Folds the log: a server is on once tools of it reached a request
    /// (the first request header, later `tool-addition` developer
    /// messages) or an activation note says so, and off after a
    /// deactivation note. Removals alone (a server that disconnected, a
    /// switched-off tool) change nothing. `server_of` maps a public tool
    /// name to its server.
    pub fn from_events(
        events: &[SessionEvent],
        server_of: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let mut history = McpHistory::default();
        let mut first_header = true;
        for event in events {
            match event.kind.as_str() {
                kinds::REQUEST_HEADER => {
                    let tools = header_tools(event);
                    if std::mem::take(&mut first_header) {
                        for server in tools.iter().filter_map(|t| server_of(t)) {
                            history.choices.insert(server, Choice::On);
                        }
                    }
                    history.declared.extend(tools);
                }
                kinds::DEVELOPER_MESSAGE => {
                    let content = event.data["message"]["content"].as_array();
                    for block in content.into_iter().flatten() {
                        if block["type"] == "tool-addition"
                            && let Some(server) = block["toolName"].as_str().and_then(&server_of)
                        {
                            history.choices.insert(server, Choice::On);
                        }
                    }
                }
                kinds::USER_MESSAGE => {
                    let source = &event.data["source"];
                    let choice = match source["kind"].as_str() {
                        Some(ACTIVATION) => Choice::On,
                        Some(DEACTIVATION) => Choice::Off,
                        _ => continue,
                    };
                    if let Some(server) = source["server"].as_str() {
                        history.choices.insert(server.to_owned(), choice);
                        if choice == Choice::On {
                            history.notes.insert(server.to_owned(), event.seq);
                        }
                    }
                }
                _ => {}
            }
        }
        history
    }
}

/// The server a public tool name belongs to, among `servers` (the longest
/// matching name wins, since server names may contain `__`).
pub fn server_of<'a>(tool: &str, servers: impl IntoIterator<Item = &'a String>) -> Option<String> {
    let rest = tool.strip_prefix("mcp__")?;
    servers
        .into_iter()
        .filter(|s| {
            rest.strip_prefix(s.as_str())
                .is_some_and(|r| r.starts_with("__"))
        })
        .max_by_key(|s| s.len())
        .cloned()
}

/// Which characters of `chars` are code: inside a fenced block (``` or ~~~
/// lines) or an inline code span (a backtick run up to the next run of the
/// same length, as in CommonMark).
fn code_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut fence: Option<(char, usize)> = None;
    let mut start = 0;
    while start < chars.len() {
        let end = chars[start..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(chars.len(), |i| start + i + 1);
        let line = &chars[start..end];
        let trimmed: Vec<char> = line.iter().copied().skip_while(|c| *c == ' ').collect();
        let marker = trimmed.first().copied().filter(|c| *c == '`' || *c == '~');
        let run = marker.map_or(0, |m| trimmed.iter().take_while(|c| **c == m).count());
        match fence {
            Some((m, n)) => {
                mask[start..end].iter_mut().for_each(|x| *x = true);
                if marker == Some(m) && run >= n {
                    fence = None;
                }
            }
            None if run >= 3 => {
                mask[start..end].iter_mut().for_each(|x| *x = true);
                fence = marker.map(|m| (m, run));
            }
            None => {
                let mut i = start;
                while i < end {
                    if chars[i] != '`' {
                        i += 1;
                        continue;
                    }
                    let n = chars[i..end].iter().take_while(|c| **c == '`').count();
                    let mut j = i + n;
                    let close = loop {
                        if j >= end {
                            break None;
                        }
                        if chars[j] == '`' {
                            let m = chars[j..end].iter().take_while(|c| **c == '`').count();
                            if m == n {
                                break Some(j + m);
                            }
                            j += m;
                        } else {
                            j += 1;
                        }
                    };
                    match close {
                        Some(stop) => {
                            mask[i..stop].iter_mut().for_each(|x| *x = true);
                            i = stop;
                        }
                        None => i += n,
                    }
                }
            }
        }
        start = end;
    }
    mask
}

/// `@name` tokens of the user's own text in `claimed`, in first-seen order:
/// `@` at the start or after whitespace or punctuation (never inside an
/// e-mail address or code), followed by a server-name token that does not
/// continue as a path (`@src/x`, `@main.rs`).
pub fn mentions(claimed: &[Message]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for message in claimed.iter().filter(|m| m.source.is_user()) {
        for block in &message.content {
            let ContentBlock::Text { text } = block else {
                continue;
            };
            let chars: Vec<char> = text.chars().collect();
            let code = code_mask(&chars);
            for (i, c) in chars.iter().enumerate() {
                if *c != '@' || code[i] {
                    continue;
                }
                let boundary = i == 0
                    || chars[i - 1].is_whitespace()
                    || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
                if !boundary {
                    continue;
                }
                let token: String = chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '-')
                    .collect();
                if token.is_empty() || token.len() > 64 {
                    continue;
                }
                let after = chars.get(i + 1 + token.chars().count()).copied();
                let next = chars.get(i + 2 + token.chars().count()).copied();
                let path = match after {
                    Some('/') | Some('\\') => true,
                    Some('.') => next.is_some_and(|n| n.is_alphanumeric()),
                    _ => false,
                };
                if !path && !out.iter().any(|t| t.eq_ignore_ascii_case(&token)) {
                    out.push(token);
                }
            }
        }
    }
    out
}

fn note(text: String, source: Value) -> Message {
    Message {
        id: dshx_core::new_id(),
        role: Role::User,
        content: vec![ContentBlock::text(text)],
        source: MessageSource::Other(source),
        tool_call_id: None,
        is_error: None,
        extra: Default::default(),
    }
}

/// The note after the user switched `server` on: its tools from here on,
/// its resources, then its instructions.
pub fn activation_message(
    server: &str,
    tools: &[String],
    resources: bool,
    instructions: &str,
) -> Message {
    let mut lines = vec![
        "<system-reminder>".to_owned(),
        if tools.is_empty() {
            format!(
                "The user turned on the MCP server \"{server}\" (@{server}) for this chat. It has no tools yet (it is still connecting or needs a login); they become available once it is connected."
            )
        } else {
            format!(
                "The user turned on the MCP server \"{server}\" (@{server}) for this chat. Its tools are available from here on: {}.",
                tools.join(", ")
            )
        },
    ];
    if resources {
        lines.push(format!(
            "Its resources are available through list_mcp_resources, list_mcp_resource_templates and read_mcp_resource with server \"{server}\"."
        ));
    }
    if !instructions.trim().is_empty() {
        lines.push(String::new());
        lines.push(instructions.trim_end().to_owned());
    }
    lines.push("</system-reminder>".into());
    note(
        lines.join("\n"),
        json!({"kind": ACTIVATION, "server": server}),
    )
}

/// The note after the user switched `server` off.
pub fn deactivation_message(server: &str) -> Message {
    note(
        format!(
            "<system-reminder>\nThe user turned off the MCP server \"{server}\" for this chat. Do not call its tools (mcp__{server}__*) any more; they fail until the user turns the server on again.\n</system-reminder>"
        ),
        json!({"kind": DEACTIVATION, "server": server}),
    )
}

/// A tool that stays declared (DeepSeek's addition-only route cannot take
/// a tool back without breaking the cached prefix) but no longer runs.
struct Unavailable {
    schema: ToolSchema,
    why: String,
}

#[async_trait::async_trait]
impl Tool for Unavailable {
    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(&self, _: Value, _: ToolContext) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Failed(self.why.clone()))
    }
}

/// The MCP part of one session's tool set.
pub struct McpSelection<'a> {
    pub config: &'a Config,
    pub choices: &'a BTreeMap<String, Choice>,
    pub declared: &'a HashSet<String>,
    pub tool_update: Option<ToolUpdate>,
}

impl McpSelection<'_> {
    /// Whether the chat offers `server`'s tools: its choice, else `always`.
    pub fn offered(&self, server: &str) -> bool {
        match self.choices.get(server) {
            Some(Choice::On) => true,
            Some(Choice::Off) => false,
            None => self.config.mcp_availability(server).always,
        }
    }
}

impl Runtime {
    /// Names of the configured and running MCP servers.
    pub(crate) fn mcp_server_names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self.mcp.servers().into_iter().collect();
        names.extend(self.config().mcp_servers.keys().cloned());
        names
    }

    /// The MCP history of a session log.
    pub(crate) fn mcp_history(&self, events: &[SessionEvent]) -> McpHistory {
        let names = self.mcp_server_names();
        McpHistory::from_events(events, |tool| server_of(tool, &names))
    }

    /// A session's choices: the log plus changes requested through the API
    /// that no step recorded yet.
    pub(crate) fn mcp_choices(&self, session: &str, events: &[SessionEvent]) -> McpHistory {
        let mut history = self.mcp_history(events);
        if let Some(pending) = self.mcp_pending.lock().unwrap().get(session) {
            for (server, on) in pending {
                history
                    .choices
                    .insert(server.clone(), if *on { Choice::On } else { Choice::Off });
            }
        }
        history
    }

    /// Servers a session offers, sorted (for the active-server chips).
    pub(crate) fn active_mcp(&self, session: &str, events: &[SessionEvent]) -> Vec<String> {
        let config = self.config();
        let history = self.mcp_choices(session, events);
        let selection = McpSelection {
            config: &config,
            choices: &history.choices,
            declared: &history.declared,
            tool_update: None,
        };
        self.mcp_server_names()
            .into_iter()
            .filter(|s| config.mcp_servers.get(s).is_none_or(|c| c.enabled))
            .filter(|s| selection.offered(s))
            .collect()
    }

    /// Switches `server` on or off for a session from its next step on.
    pub(crate) fn request_mcp(&self, session: &str, server: &str, on: bool) {
        self.mcp_pending
            .lock()
            .unwrap()
            .entry(session.to_owned())
            .or_default()
            .insert(server.to_owned(), on);
    }

    /// The bridged tools a session offers: tools of offered servers minus
    /// `disabled_tools`, the resource tools while any server is offered.
    /// On the addition-only route a tool the session declared before stays
    /// declared, answering with why it is unavailable.
    pub(crate) fn mcp_tools(&self, selection: &McpSelection<'_>) -> Vec<Arc<dyn Tool>> {
        let keep = selection.tool_update == Some(ToolUpdate::AdditionOnly);
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        let mut any = false;
        for entry in self.mcp.server_tools() {
            let schema = entry.tool.schema();
            let disabled = selection
                .config
                .mcp_availability(&entry.server)
                .disabled_tools
                .contains(&entry.raw_name);
            let offered = selection.offered(&entry.server);
            if offered && !disabled {
                any = true;
                out.push(entry.tool);
            } else if keep && selection.declared.contains(&schema.name) {
                let why = if disabled {
                    format!(
                        "The user switched off the tool \"{}\" of the MCP server \"{}\".",
                        entry.raw_name, entry.server
                    )
                } else {
                    format!(
                        "The user turned off the MCP server \"{}\" for this chat; its tools are unavailable until the user turns it on again.",
                        entry.server
                    )
                };
                out.push(Arc::new(Unavailable { schema, why }));
            }
        }
        let resources = self.mcp.resource_tools();
        if any
            || (keep
                && resources
                    .iter()
                    .any(|t| selection.declared.contains(&t.schema().name)))
        {
            out.extend(resources);
        }
        out
    }

    /// Replaces the bridged tools in `tools` with `mcp`.
    pub(crate) fn swap_mcp_tools(tools: &mut ToolRegistry, mcp: Vec<Arc<dyn Tool>>) {
        let mut next = ToolRegistry::new();
        for tool in tools.iter() {
            if !dshx_mcp::is_bridged_tool(&tool.schema().name) {
                next.register(tool.clone());
            }
        }
        for tool in mcp {
            next.register(tool);
        }
        *tools = next;
    }

    /// Servers `token` names: a server (case-insensitive), else every server
    /// of the enabled plugin with that name.
    async fn resolve_mention(&self, token: &str, servers: &BTreeSet<String>) -> Vec<String> {
        if let Some(server) = servers.iter().find(|s| s.eq_ignore_ascii_case(token)) {
            return vec![server.clone()];
        }
        self.plugin_servers()
            .await
            .into_iter()
            .find(|(id, _)| {
                id.split('@')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(token))
            })
            .map(|(_, servers)| servers)
            .unwrap_or_default()
    }
}

/// Per-step MCP selection of one root agent: mentions and API requests
/// switch servers on or off, the tool set follows, notes record it.
pub(crate) struct McpHook {
    pub runtime: Weak<Runtime>,
    pub session: String,
}

#[async_trait::async_trait]
impl PreStepHook for McpHook {
    async fn pre_step(
        &self,
        step: &PreStep<'_>,
        entering: &mut Vec<Message>,
    ) -> anyhow::Result<()> {
        let Some(rt) = self.runtime.upgrade() else {
            return Ok(());
        };
        let names = rt.mcp_server_names();
        let (logged, compacted) = step.read_history(|events, visible| {
            let history = McpHistory::from_events(events, |t| server_of(t, &names));
            // Activation notes a compaction folded away: their instructions are gone.
            let compacted: BTreeSet<String> = history
                .notes
                .iter()
                .filter(|(_, seq)| !visible(**seq))
                .map(|(server, _)| server.clone())
                .collect();
            (history, compacted)
        });
        let mut choices = logged.choices.clone();
        let pending = rt.mcp_pending.lock().unwrap().remove(&self.session);
        for (server, on) in pending.into_iter().flatten() {
            choices.insert(server, if on { Choice::On } else { Choice::Off });
        }
        for token in mentions(step.claimed) {
            for server in rt.resolve_mention(&token, &names).await {
                choices.insert(server, Choice::On);
            }
        }
        let config = rt.config();
        let before = McpSelection {
            config: &config,
            choices: &logged.choices,
            declared: &logged.declared,
            tool_update: step.tool_update,
        };
        let after = McpSelection {
            choices: &choices,
            ..before
        };
        let tools = rt.mcp_tools(&after);
        let wanted: BTreeSet<String> = tools.iter().map(|t| t.schema().name).collect();
        let current: BTreeSet<String> = step
            .tools
            .iter()
            .filter(|n| dshx_mcp::is_bridged_tool(n))
            .cloned()
            .collect();
        // Same names can still mean other tools (a switched-off server's
        // declared tools turn into stubs), so a changed choice swaps too.
        if wanted != current || choices != logged.choices {
            step.update_tools(|registry| Runtime::swap_mcp_tools(registry, tools));
        }
        for server in &names {
            // After a compaction an active server's note comes back once.
            let renew =
                compacted.contains(server) && logged.choices.get(server) == Some(&Choice::On);
            let (was, now) = (before.offered(server), after.offered(server));
            if now && (!was || renew) {
                let tool_names: Vec<String> = wanted
                    .iter()
                    .filter(|t| server_of(t, &names).as_deref() == Some(server))
                    .cloned()
                    .collect();
                entering.push(activation_message(
                    server,
                    &tool_names,
                    rt.mcp.has_resources(server),
                    &rt.mcp.instructions(server),
                ));
            } else if was && !now {
                entering.push(deactivation_message(server));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, data: Value) -> SessionEvent {
        SessionEvent {
            kind: kind.into(),
            seq,
            time: 0,
            data,
            extra: Default::default(),
        }
    }

    #[test]
    fn mentions_skip_mails_and_paths() {
        let m =
            Message::user_text("@linear and (@GitHub), a@b.com @src/main.rs @x.rs @end. @linear");
        assert_eq!(mentions(&[m]), ["linear", "GitHub", "end"]);
        let code = Message::user_text(
            "run `npm i @linear/sdk` or `@github`\n```\n@notion\n```\n``a `@x` b`` then @sentry",
        );
        assert_eq!(mentions(&[code]), ["sentry"]);
        let mut injected = Message::user_text("@linear");
        injected.source = MessageSource::Other(json!({"kind": "runtime-context"}));
        assert!(mentions(&[injected]).is_empty());
    }

    #[test]
    fn server_names_take_the_longest_match() {
        let names = ["a".to_owned(), "a__b".to_owned()];
        assert_eq!(server_of("mcp__a__b__c", &names).as_deref(), Some("a__b"));
        assert_eq!(server_of("mcp__a__c", &names).as_deref(), Some("a"));
        assert_eq!(server_of("read", &names), None);
    }

    #[test]
    fn the_log_decides_which_servers_are_on() {
        let names = ["t".to_owned(), "u".to_owned(), "v".to_owned()];
        let header = |tools: &[&str]| json!({"header": {"tools": tools.iter().map(|t| json!({"name": t})).collect::<Vec<_>>()}});
        let events = vec![
            ev(0, kinds::REQUEST_HEADER, header(&["read", "mcp__t__echo"])),
            ev(
                1,
                kinds::DEVELOPER_MESSAGE,
                json!({"message": {"content": [
                {"type": "tool-addition", "toolName": "mcp__u__x"},
                {"type": "tool-removal", "toolName": "mcp__t__echo"}]}}),
            ),
            ev(2, kinds::REQUEST_HEADER, header(&["read", "mcp__u__x"])),
            ev(
                3,
                kinds::USER_MESSAGE,
                json!({"source": {"kind": DEACTIVATION, "server": "u"}}),
            ),
            ev(
                4,
                kinds::USER_MESSAGE,
                json!({"source": {"kind": ACTIVATION, "server": "v"}}),
            ),
        ];
        let h = McpHistory::from_events(&events, |t| server_of(t, &names));
        assert_eq!(h.choices["t"], Choice::On);
        assert_eq!(h.choices["u"], Choice::Off);
        assert_eq!(h.choices["v"], Choice::On);
        assert!(h.declared.contains("mcp__t__echo") && h.declared.contains("mcp__u__x"));
    }
}
