//! Host-wide task storage: one JSON unit file in dsh's `storage-json`
//! single layout (`<root>/schedule.json`: `{unit, global, tables: {tasks}}`,
//! pretty-printed, atomically replaced).
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::record::Record;
use crate::time::parse_iso;

const UNIT: &str = "schedule";
const VERSION: u64 = 1;

/// One saved inbox acknowledgment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub scheduled_at: String,
    pub delivered_at: String,
    pub message_id: String,
}

impl Receipt {
    fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("scheduledAt".into(), self.scheduled_at.clone().into());
        m.insert("deliveredAt".into(), self.delivered_at.clone().into());
        m.insert("messageId".into(), self.message_id.clone().into());
        m
    }

    fn from_json(v: &Value, allow_prompt: bool) -> Result<(Receipt, Option<String>), String> {
        let m = v.as_object().ok_or("delivery receipt must be an object")?;
        let allowed = ["scheduledAt", "deliveredAt", "messageId", "prompt"];
        if m.keys()
            .any(|k| !allowed[..if allow_prompt { 4 } else { 3 }].contains(&k.as_str()))
        {
            return Err("delivery receipt has unknown fields".into());
        }
        let instant = |k: &str| {
            m.get(k)
                .and_then(Value::as_str)
                .filter(|s| parse_iso(s).is_some())
                .map(str::to_owned)
                .ok_or_else(|| format!("delivery receipt {k} must be a canonical UTC instant"))
        };
        let message_id = m
            .get("messageId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.trim() == *s)
            .ok_or("delivery receipt messageId is invalid")?;
        let prompt = match m.get("prompt") {
            None => None,
            Some(p) => Some(
                p.as_str()
                    .ok_or("delivery record prompt must be a string")?
                    .to_owned(),
            ),
        };
        Ok((
            Receipt {
                scheduled_at: instant("scheduledAt")?,
                delivered_at: instant("deliveredAt")?,
                message_id: message_id.to_owned(),
            },
            prompt,
        ))
    }
}

/// Retained deliveries of one task, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct History {
    pub records: Vec<(Receipt, Option<String>)>,
    pub earlier_unavailable: bool,
    pub earlier_pruned: Option<bool>,
}

impl History {
    fn to_json(&self) -> Value {
        let records: Vec<Value> = self
            .records
            .iter()
            .map(|(r, prompt)| {
                let mut m = r.to_json();
                if let Some(p) = prompt {
                    m.insert("prompt".into(), p.clone().into());
                }
                Value::Object(m)
            })
            .collect();
        let mut m = Map::new();
        m.insert("records".into(), Value::Array(records));
        m.insert(
            "earlierRecordsUnavailable".into(),
            self.earlier_unavailable.into(),
        );
        if let Some(p) = self.earlier_pruned {
            m.insert("earlierRecordsPruned".into(), p.into());
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Inactive,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Inactive => "inactive",
        }
    }
}

/// One stored task (dsh `scheduleDomain.tasks` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub session_id: String,
    pub record: Record,
    pub status: Status,
    pub last_delivery: Option<Receipt>,
    pub history: Option<History>,
}

impl Task {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("sessionId".into(), self.session_id.clone().into());
        m.insert("record".into(), self.record.to_json());
        m.insert("status".into(), self.status.as_str().into());
        if let Some(h) = &self.history {
            m.insert("deliveryHistory".into(), h.to_json());
        }
        if let Some(r) = &self.last_delivery {
            m.insert("lastDelivery".into(), Value::Object(r.to_json()));
        }
        Value::Object(m)
    }

    pub fn from_json(v: &Value) -> Result<Task, String> {
        let m = v.as_object().ok_or("task must be an object")?;
        let allowed = [
            "sessionId",
            "record",
            "status",
            "lastDelivery",
            "deliveryHistory",
        ];
        if m.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err("task has unknown fields".into());
        }
        let session_id = m
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("task sessionId must be a non-empty string")?;
        let record = Record::from_json(m.get("record").unwrap_or(&Value::Null))?;
        let status = match m.get("status").and_then(Value::as_str) {
            None | Some("active") => Status::Active,
            Some("inactive") => Status::Inactive,
            Some(_) => return Err("task status must be active or inactive".into()),
        };
        let last_delivery = match m.get("lastDelivery") {
            None => None,
            Some(v) => Some(Receipt::from_json(v, false)?.0),
        };
        let history = match m.get("deliveryHistory") {
            None => None,
            Some(v) => {
                let h = v.as_object().ok_or("deliveryHistory must be an object")?;
                let records = h
                    .get("records")
                    .and_then(Value::as_array)
                    .ok_or("deliveryHistory records must be an array")?
                    .iter()
                    .map(|r| Receipt::from_json(r, true))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut ids: Vec<&str> =
                    records.iter().map(|(r, _)| r.message_id.as_str()).collect();
                ids.sort_unstable();
                ids.dedup();
                if ids.len() != records.len() {
                    return Err(
                        "Delivery history message identities must be unique within a task".into(),
                    );
                }
                Some(History {
                    records,
                    earlier_unavailable: h
                        .get("earlierRecordsUnavailable")
                        .and_then(Value::as_bool)
                        .ok_or("deliveryHistory earlierRecordsUnavailable must be a boolean")?,
                    earlier_pruned: h.get("earlierRecordsPruned").and_then(Value::as_bool),
                })
            }
        };
        if let Some(h) = &history {
            let consistent = match (h.records.last(), &last_delivery) {
                (None, None) => true,
                (Some((latest, _)), Some(last)) => latest == last,
                _ => false,
            };
            if !consistent {
                return Err("Last delivery must match the latest saved delivery receipt".into());
            }
        }
        Ok(Task {
            session_id: session_id.to_owned(),
            record,
            status,
            last_delivery,
            history,
        })
    }

    /// dsh `deliveryHistoryOf`: legacy rows expose only their last receipt.
    pub fn history_or_legacy(&self) -> History {
        self.history.clone().unwrap_or_else(|| History {
            records: self
                .last_delivery
                .clone()
                .map(|r| (r, None))
                .into_iter()
                .collect(),
            earlier_unavailable: true,
            earlier_pruned: None,
        })
    }
}

/// The task table file.
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// `<root>/schedule.json`.
    pub fn new(root: &Path) -> Store {
        Store {
            path: root.join(format!("{UNIT}.json")),
        }
    }

    /// Loads every task; a missing file is an empty table.
    pub fn load(&self) -> anyhow::Result<Vec<Task>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let doc: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("unit '{UNIT}': file is not valid JSON: {e}"))?;
        anyhow::ensure!(
            doc["unit"]["name"] == UNIT && doc["unit"]["version"].is_number(),
            "unit '{UNIT}': missing or foreign unit header"
        );
        anyhow::ensure!(
            doc["unit"]["version"] == VERSION,
            "unit '{UNIT}': stored version {} != expected {VERSION}",
            doc["unit"]["version"]
        );
        let mut out = Vec::new();
        if let Some(tasks) = doc["tables"].get("tasks") {
            let tasks = tasks
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("unit '{UNIT}': table 'tasks' is not an object"))?;
            for (key, value) in tasks {
                let task = Task::from_json(value).map_err(|e| {
                    anyhow::anyhow!("schedule: stored task \"{key}\" is invalid: {e}")
                })?;
                anyhow::ensure!(
                    *key == task.record.id,
                    "schedule: stored task key \"{key}\" differs from record id \"{}\"",
                    task.record.id
                );
                out.push(task);
            }
        }
        Ok(out)
    }

    /// Durably replaces the file with `tasks` (temp file, fsync, rename).
    pub fn save(&self, tasks: &[Task]) -> anyhow::Result<()> {
        let mut table = Map::new();
        for t in tasks {
            table.insert(t.record.id.clone(), t.to_json());
        }
        let doc = json!({
            "unit": {"name": UNIT, "version": VERSION},
            "global": null,
            "tables": {"tasks": Value::Object(table)},
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&doc)?);
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> anyhow::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
            #[cfg(unix)]
            std::fs::File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}
