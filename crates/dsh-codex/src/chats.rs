//! Sidebar state the session log does not hold: pinned, archived, renamed and
//! read markers, plus the folders of chats started without a project, and the
//! display preferences of Settings → Appearance. Kept in
//! `$DSHX_HOME/ui-state.json`, so every browser and the desktop app see the
//! same sidebar and the same view.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatMeta {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pinned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    archived: bool,
    /// Marked unread by hand; cleared when the chat is seen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seen_at: Option<u64>,
}

/// Display preferences (dsh `ui-chat` settings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiPrefs {
    /// How much of the agent's work the transcript shows: `compact`,
    /// `standard`, `detailed` or `verbose` (dsh "Work details").
    pub work_details: String,
    /// `compact` (context ring and cost) or `detailed` (dsh "Performance & usage").
    pub performance_usage: String,
    /// Sidebar view (dsh workspace view options): `projects` or `flat`.
    pub sidebar_group: String,
    /// `activity` (last activity first) or `created` (newest chat first).
    pub sidebar_order: String,
    /// Archived chats: `hide`, `show` (in their places) or `only`.
    pub sidebar_archived: String,
    /// Ask before switching to "Full access" (dsh permission-presets `confirm`).
    pub confirm_full_access: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            work_details: "standard".into(),
            performance_usage: "detailed".into(),
            sidebar_group: "projects".into(),
            sidebar_order: "activity".into(),
            sidebar_archived: "hide".into(),
            confirm_full_access: true,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    sessions: BTreeMap<String, ChatMeta>,
    #[serde(default)]
    ui: UiPrefs,
}

pub struct ChatState {
    path: PathBuf,
    chats_dir: PathBuf,
    file: Mutex<StateFile>,
}

/// Activity this close to the last "seen" still counts as seen (the final
/// events of a turn the user watched can land just after the marker).
const SEEN_SLACK_MS: u64 = 1500;

impl ChatState {
    pub fn load(home: &Path) -> Self {
        let path = home.join("ui-state.json");
        let file = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // Session cwds are canonical; compare against the canonical home.
        let base = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        ChatState {
            path,
            chats_dir: base.join("chats"),
            file: Mutex::new(file),
        }
    }

    /// Folder of a chat that belongs to no project.
    pub fn is_scratch(&self, cwd: &str) -> bool {
        Path::new(cwd).starts_with(&self.chats_dir)
    }

    /// Creates the private folder for a chat without a project.
    pub fn new_chat_dir(&self) -> anyhow::Result<PathBuf> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.chats_dir.join(format!("chat-{}", &id[..12]));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Adds `scratch`, `pinned`, `archived`, `unread` and a renamed title to a
    /// session list row.
    pub fn decorate(&self, row: &mut Value) {
        let scratch = row["cwd"].as_str().is_some_and(|c| self.is_scratch(c));
        let file = self.file.lock().unwrap();
        let meta = row["id"]
            .as_str()
            .and_then(|id| file.sessions.get(id))
            .cloned()
            .unwrap_or_default();
        let updated = row["updatedAt"].as_u64().unwrap_or(0);
        // Never seen here (started elsewhere, or before this file existed): read.
        let unread = meta.unread
            || meta
                .seen_at
                .is_some_and(|seen| updated > seen + SEEN_SLACK_MS);
        if let Some(title) = &meta.title {
            row["title"] = json!(title);
        }
        row["scratch"] = json!(scratch);
        row["pinned"] = json!(meta.pinned);
        row["archived"] = json!(meta.archived);
        row["unread"] = json!(unread);
    }

    /// Applies `{title?, pinned?, archived?, seen?, unread?}`.
    pub fn update(&self, id: &str, patch: &Value, now: u64) -> anyhow::Result<()> {
        let mut file = self.file.lock().unwrap();
        let meta = file.sessions.entry(id.to_owned()).or_default();
        if let Some(title) = patch.get("title") {
            let title = title.as_str().unwrap_or("").trim();
            anyhow::ensure!(title.chars().count() <= 200, "title too long");
            meta.title = (!title.is_empty()).then(|| title.to_owned());
        }
        if let Some(v) = patch.get("pinned").and_then(Value::as_bool) {
            meta.pinned = v;
        }
        if let Some(v) = patch.get("archived").and_then(Value::as_bool) {
            meta.archived = v;
        }
        if patch.get("seen").and_then(Value::as_bool) == Some(true) {
            meta.seen_at = Some(now);
            meta.unread = false;
        }
        if let Some(v) = patch.get("unread").and_then(Value::as_bool) {
            meta.unread = v;
            if !v {
                meta.seen_at = Some(now);
            }
        }
        self.save(&file)
    }

    pub fn ui(&self) -> UiPrefs {
        self.file.lock().unwrap().ui.clone()
    }

    /// Applies `{workDetails?, performanceUsage?, sidebarGroup?, sidebarOrder?,
    /// sidebarArchived?, confirmFullAccess?}`.
    pub fn update_ui(&self, patch: &Value) -> anyhow::Result<UiPrefs> {
        let pick = |key: &str, allowed: &[&str]| -> anyhow::Result<Option<String>> {
            match patch.get(key) {
                None => Ok(None),
                Some(v) => match v.as_str().filter(|v| allowed.contains(v)) {
                    Some(v) => Ok(Some(v.to_owned())),
                    None => anyhow::bail!("{key} must be one of {}", allowed.join(", ")),
                },
            }
        };
        let work = pick(
            "workDetails",
            &["compact", "standard", "detailed", "verbose"],
        )?;
        let usage = pick("performanceUsage", &["compact", "detailed"])?;
        let group = pick("sidebarGroup", &["projects", "flat"])?;
        let order = pick("sidebarOrder", &["activity", "created"])?;
        let archived = pick("sidebarArchived", &["hide", "show", "only"])?;
        let confirm = match patch.get("confirmFullAccess") {
            None => None,
            Some(v) => Some(
                v.as_bool()
                    .ok_or_else(|| anyhow::anyhow!("confirmFullAccess must be true or false"))?,
            ),
        };
        let mut file = self.file.lock().unwrap();
        if let Some(v) = work {
            file.ui.work_details = v;
        }
        if let Some(v) = usage {
            file.ui.performance_usage = v;
        }
        if let Some(v) = group {
            file.ui.sidebar_group = v;
        }
        if let Some(v) = order {
            file.ui.sidebar_order = v;
        }
        if let Some(v) = archived {
            file.ui.sidebar_archived = v;
        }
        if let Some(v) = confirm {
            file.ui.confirm_full_access = v;
        }
        self.save(&file)?;
        Ok(file.ui.clone())
    }

    pub fn remove(&self, id: &str) {
        let mut file = self.file.lock().unwrap();
        if file.sessions.remove(id).is_some() {
            let _ = self.save(&file);
        }
    }

    fn save(&self, file: &StateFile) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, cwd: &str, updated: u64) -> Value {
        json!({ "id": id, "cwd": cwd, "title": "Auto", "updatedAt": updated })
    }

    #[test]
    fn marks_survive_a_reload_and_drive_the_row() {
        let home = tempfile::tempdir().unwrap();
        let state = ChatState::load(home.path());
        state
            .update(
                "s1",
                &json!({"pinned": true, "title": " Mine ", "seen": true}),
                1_000,
            )
            .unwrap();
        let state = ChatState::load(home.path());
        let mut r = row("s1", "/x", 2_000);
        state.decorate(&mut r);
        assert_eq!(r["pinned"], true);
        assert_eq!(r["title"], "Mine");
        assert_eq!(r["unread"], false, "within the slack after seen");
        let mut r = row("s1", "/x", 9_000);
        state.decorate(&mut r);
        assert_eq!(r["unread"], true, "activity after seen");
    }

    #[test]
    fn unknown_chats_read_and_manual_unread_sticks_until_seen() {
        let home = tempfile::tempdir().unwrap();
        let state = ChatState::load(home.path());
        let mut r = row("s2", "/x", 5_000);
        state.decorate(&mut r);
        assert_eq!(r["unread"], false);
        state.update("s2", &json!({"unread": true}), 6_000).unwrap();
        let mut r = row("s2", "/x", 5_000);
        state.decorate(&mut r);
        assert_eq!(r["unread"], true);
        state.update("s2", &json!({"seen": true}), 7_000).unwrap();
        let mut r = row("s2", "/x", 5_000);
        state.decorate(&mut r);
        assert_eq!(r["unread"], false);
        state.update("s2", &json!({"title": ""}), 7_000).unwrap();
        let mut r = row("s2", "/x", 5_000);
        state.decorate(&mut r);
        assert_eq!(
            r["title"], "Auto",
            "an empty title restores the generated one"
        );
    }

    #[test]
    fn display_preferences_persist_and_are_checked() {
        let home = tempfile::tempdir().unwrap();
        let state = ChatState::load(home.path());
        assert_eq!(state.ui(), UiPrefs::default());
        state.update_ui(&json!({"workDetails": "verbose"})).unwrap();
        assert!(
            state
                .update_ui(&json!({"performanceUsage": "loud"}))
                .is_err()
        );
        let state = ChatState::load(home.path());
        assert_eq!(state.ui().work_details, "verbose");
        assert_eq!(state.ui().performance_usage, "detailed");
    }

    #[test]
    fn sidebar_view_and_full_access_question_persist() {
        let home = tempfile::tempdir().unwrap();
        let state = ChatState::load(home.path());
        assert!(state.ui().confirm_full_access);
        state
            .update_ui(&json!({"sidebarGroup": "flat", "sidebarOrder": "created", "sidebarArchived": "only", "confirmFullAccess": false}))
            .unwrap();
        assert!(state.update_ui(&json!({"sidebarGroup": "tree"})).is_err());
        assert!(
            state
                .update_ui(&json!({"confirmFullAccess": "no"}))
                .is_err()
        );
        let ui = ChatState::load(home.path()).ui();
        assert_eq!(
            (
                ui.sidebar_group.as_str(),
                ui.sidebar_order.as_str(),
                ui.sidebar_archived.as_str()
            ),
            ("flat", "created", "only")
        );
        assert!(!ui.confirm_full_access);
        // A file from before these settings reads with their defaults.
        std::fs::write(
            home.path().join("ui-state.json"),
            r#"{"ui":{"workDetails":"compact"}}"#,
        )
        .unwrap();
        let ui = ChatState::load(home.path()).ui();
        assert_eq!(
            (ui.work_details.as_str(), ui.sidebar_group.as_str()),
            ("compact", "projects")
        );
        assert!(ui.confirm_full_access);
    }

    #[test]
    fn chat_folders_are_scratch() {
        let home = tempfile::tempdir().unwrap();
        let state = ChatState::load(home.path());
        let dir = state.new_chat_dir().unwrap();
        assert!(dir.is_dir());
        assert!(state.is_scratch(&dir.display().to_string()));
        assert!(!state.is_scratch(&home.path().join("project").display().to_string()));
    }
}
