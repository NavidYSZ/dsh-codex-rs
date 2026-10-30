//! The composer routes of `dsh-codex web` ([`dshx_server::Composer`]):
//! `/` and `@` suggestions, the MCP servers a chat activated, `/compact`,
//! the log export, queued prompts and file uploads.
use std::path::PathBuf;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use dshx_agent::{Preset, QueueAction};
use dshx_core::{ContentBlock, SessionEvent, SessionHeader};
use dshx_server::{NotFound, SuggestQuery};
use serde_json::{Value, json};
use sha2::Digest as _;

use super::WebBackend;
use crate::runtime::{Overrides, RuntimeExtensions, SuggestContext};

pub(super) struct WebComposer(pub Weak<WebBackend>);

/// What the routes need to know about one chat.
struct Chat {
    header: SessionHeader,
    events: Vec<SessionEvent>,
    preset: Preset,
    cwd: PathBuf,
}

impl WebComposer {
    fn backend(&self) -> anyhow::Result<Arc<WebBackend>> {
        self.0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("the web backend stopped"))
    }

    /// The live log of an open chat, else the stored one.
    async fn chat(&self, id: &str) -> anyhow::Result<Chat> {
        let b = self.backend()?;
        if let Some(s) = b.open.lock().await.get(id).cloned() {
            return Ok(Chat {
                header: s.log.header(),
                events: s.log.events(),
                preset: s.agent.config().preset,
                cwd: s.cwd.clone(),
            });
        }
        if b.runtime.store.locate(id)?.is_none() {
            return Err(NotFound(id.to_owned()).into());
        }
        let stored = b.runtime.store.read(id)?;
        let recorded = dshx_session::agent_preset(&stored.header, &stored.events);
        Ok(Chat {
            cwd: PathBuf::from(&stored.header.cwd),
            preset: Preset::of_session(recorded.as_deref()),
            header: stored.header,
            events: stored.events,
        })
    }
}

/// A file name without folders or control characters.
fn file_name(name: &str) -> anyhow::Result<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim().to_owned();
    anyhow::ensure!(
        !clean.is_empty() && clean != "." && clean != "..",
        "invalid file name {name:?}"
    );
    Ok(clean)
}

#[async_trait]
impl dshx_server::Composer for WebComposer {
    async fn suggest(&self, query: SuggestQuery) -> anyhow::Result<Value> {
        let b = self.backend()?;
        let chat = match query.session.as_deref().filter(|s| !s.is_empty()) {
            Some(id) => Some((id.to_owned(), self.chat(id).await?)),
            None => None,
        };
        let cwd = query
            .cwd
            .filter(|c| !c.is_empty())
            .map(PathBuf::from)
            .or_else(|| chat.as_ref().map(|(_, c)| c.cwd.clone()));
        let goals = b.runtime.automation.features()["goals"] == true;
        let preset = match &chat {
            Some((_, c)) => c.preset,
            None => query
                .preset
                .as_deref()
                .and_then(Preset::parse)
                .unwrap_or_else(|| crate::presets::default_of(&b.runtime.config())),
        };
        b.runtime
            .suggest(SuggestContext {
                kind: query.kind,
                query: query.q,
                cwd,
                preset,
                session: chat.map(|(id, c)| (id, c.events)),
                model: query.model.filter(|m| !m.is_empty()),
                goals,
            })
            .await
    }

    async fn session_mcp(&self, id: &str) -> anyhow::Result<Value> {
        let b = self.backend()?;
        let chat = self.chat(id).await?;
        let active = match chat.preset {
            Preset::Standard | Preset::Ptc => b.runtime.active_mcp(id, &chat.events),
            Preset::Minimal => Vec::new(),
        };
        Ok(json!({ "active": active }))
    }

    async fn set_session_mcp(&self, id: &str, server: &str, on: bool) -> anyhow::Result<Value> {
        let b = self.backend()?;
        let chat = self.chat(id).await?;
        anyhow::ensure!(
            chat.preset != Preset::Minimal,
            "MCP servers are not part of Minimal mode"
        );
        if !b.runtime.mcp_server_names().contains(server) {
            return Err(NotFound(format!("MCP server {server}")).into());
        }
        b.runtime.request_mcp(id, server, on);
        self.session_mcp(id).await
    }

    async fn compact(&self, id: &str) -> anyhow::Result<Value> {
        let b = self.backend()?;
        let s = b.session_handle(id, &Overrides::default()).await?;
        anyhow::ensure!(
            s.agent.config().preset != Preset::Minimal,
            "context compaction is not part of Minimal mode"
        );
        Ok(match s.agent.compact_now().await? {
            Some(done) => json!({ "compacted": true, "items": done.items, "tokens": done.tokens }),
            None => json!({ "compacted": false }),
        })
    }

    async fn export(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        let chat = self.chat(id).await?;
        let mut out = serde_json::to_vec(&chat.header)?;
        out.push(b'\n');
        for event in &chat.events {
            serde_json::to_writer(&mut out, event)?;
            out.push(b'\n');
        }
        Ok((out, format!("dsh-codex-{id}.jsonl")))
    }

    async fn update_queue(&self, id: &str, item: &str, body: Value) -> anyhow::Result<()> {
        let b = self.backend()?;
        let Some(s) = b.open.lock().await.get(id).cloned() else {
            anyhow::bail!("queued item is no longer pending");
        };
        let action = match body["action"].as_str() {
            Some("edit") => {
                let text = body["text"].as_str().unwrap_or_default();
                QueueAction::Edit(vec![ContentBlock::text(text)])
            }
            Some("remove") => QueueAction::Remove,
            Some("steer") => QueueAction::Steer,
            _ => anyhow::bail!("action must be \"edit\", \"remove\" or \"steer\""),
        };
        s.agent.update_queue(item, action).await
    }

    async fn save_file(&self, name: &str, data: Vec<u8>) -> anyhow::Result<Value> {
        let b = self.backend()?;
        let name = file_name(name)?;
        let digest: String = sha2::Sha256::digest(&data)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        let dir = b
            .runtime
            .home
            .join("attachments")
            .join("files")
            .join(&digest[..16]);
        let path = dir.join(&name);
        let bytes = data.len();
        let target = path.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if target.is_file() {
                return Ok(());
            }
            std::fs::create_dir_all(&dir)?;
            let tmp = dir.join(format!(".{name}.tmp"));
            std::fs::write(&tmp, &data)?;
            // A verbatim read-only copy, like dsh's stored attachments.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o444))?;
            }
            std::fs::rename(&tmp, &target)?;
            Ok(())
        })
        .await??;
        Ok(json!({
            "path": path.display().to_string(),
            "name": path.file_name().map(|n| n.to_string_lossy().into_owned()),
            "bytes": bytes,
            "sha256": digest,
        }))
    }

    async fn skill_settings(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let b = self.backend()?;
        RuntimeExtensions::new(b.runtime.clone())
            .set_skill_settings(name, body)
            .await
    }

    async fn mcp_settings(&self, name: &str, body: Value) -> anyhow::Result<Value> {
        let b = self.backend()?;
        RuntimeExtensions::new(b.runtime.clone())
            .set_mcp_settings(name, body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_folders() {
        assert_eq!(file_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(file_name("C:\\x\\Report Q3.pdf").unwrap(), "Report Q3.pdf");
        assert!(file_name("..").is_err());
        assert!(file_name("dir/").is_err());
    }
}
