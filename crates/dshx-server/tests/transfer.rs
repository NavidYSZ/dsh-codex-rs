//! `/api/transfer/…`: folder transfer between instances against a fake
//! backend whose home and project folders live in a temporary folder.
use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use dshx_core::{ImageAttachmentRef, SessionEvent};
use dshx_server::{Backend, NotFound, PromptRequest, StreamMessage, TransferHost, serve};
use serde_json::{Value, json};
use tokio::sync::broadcast;

struct Host {
    home: PathBuf,
    workspaces: Mutex<Vec<PathBuf>>,
}

#[async_trait]
impl TransferHost for Host {
    fn home(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }
    async fn workspaces(&self) -> anyhow::Result<Vec<PathBuf>> {
        Ok(self.workspaces.lock().unwrap().clone())
    }
    async fn remove_workspace(&self, path: &Path) -> anyhow::Result<()> {
        self.workspaces.lock().unwrap().retain(|w| w != path);
        Ok(())
    }
}

struct Fake {
    host: Arc<Host>,
    tx: broadcast::Sender<StreamMessage>,
}

#[async_trait]
impl Backend for Fake {
    async fn info(&self) -> anyhow::Result<Value> {
        Ok(json!({}))
    }
    async fn update_settings(&self, patch: Value) -> anyhow::Result<Value> {
        Ok(patch)
    }
    async fn workspaces(&self) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn add_workspace(&self, path: String) -> anyhow::Result<Value> {
        Ok(json!({ "path": path }))
    }
    async fn list_sessions(&self, _cwd: Option<String>) -> anyhow::Result<Value> {
        Ok(json!([]))
    }
    async fn create_session(&self, _cwd: String) -> anyhow::Result<String> {
        anyhow::bail!("no sessions")
    }
    async fn session(&self, id: &str) -> anyhow::Result<Value> {
        Err(NotFound(id.into()).into())
    }
    async fn events_after(
        &self,
        _id: &str,
        _after: Option<u64>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(vec![])
    }
    async fn prompt(&self, _id: &str, _request: PromptRequest) -> anyhow::Result<()> {
        Ok(())
    }
    async fn cancel(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete_session(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn approve(&self, _call_id: &str, _decision: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn save_attachment(
        &self,
        _data: Vec<u8>,
        _media_type: String,
    ) -> anyhow::Result<ImageAttachmentRef> {
        anyhow::bail!("no attachments")
    }
    async fn load_attachment(&self, id: &str) -> anyhow::Result<(Vec<u8>, String)> {
        Err(NotFound(id.into()).into())
    }
    fn stream(&self) -> broadcast::Receiver<StreamMessage> {
        self.tx.subscribe()
    }
    fn transfer(&self) -> Option<Arc<dyn TransferHost>> {
        Some(self.host.clone())
    }
}

/// A temporary world: `home/`, a project parent `srv/` with `srv/app`, and
/// `outside/`, which no root covers.
struct World {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    srv: PathBuf,
    outside: PathBuf,
    host: Arc<Host>,
    base: String,
}

async fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let srv = root.join("srv");
    let outside = root.join("outside");
    for d in [&home, &srv.join("app"), &outside] {
        fs::create_dir_all(d).unwrap();
    }
    let host = Arc::new(Host {
        home: home.clone(),
        workspaces: Mutex::new(vec![srv.join("app")]),
    });
    let (tx, _) = broadcast::channel(16);
    let backend = Fake {
        host: host.clone(),
        tx,
    };
    let (addr, _server) = serve(Arc::new(backend), "tok".into(), 0).await.unwrap();
    World {
        _tmp: tmp,
        home,
        srv,
        outside,
        host,
        base: format!("http://{addr}"),
    }
}

impl World {
    async fn post(&self, route: &str, body: Value) -> (u16, Value) {
        let res = reqwest::Client::new()
            .post(format!("{}/api/transfer/{route}", self.base))
            .bearer_auth("tok")
            .json(&body)
            .send()
            .await
            .unwrap();
        (
            res.status().as_u16(),
            res.json().await.unwrap_or(Value::Null),
        )
    }

    async fn export(&self, body: Value) -> (u16, Vec<u8>) {
        let res = reqwest::Client::new()
            .post(format!("{}/api/transfer/export", self.base))
            .bearer_auth("tok")
            .json(&body)
            .send()
            .await
            .unwrap();
        (res.status().as_u16(), res.bytes().await.unwrap().to_vec())
    }

    async fn import(&self, target: &Path, mode: &str, archive: Vec<u8>) -> (u16, Value) {
        let res = reqwest::Client::new()
            .post(format!("{}/api/transfer/import", self.base))
            .query(&[
                ("path", target.display().to_string().as_str()),
                ("mode", mode),
            ])
            .bearer_auth("tok")
            .body(archive)
            .send()
            .await
            .unwrap();
        (
            res.status().as_u16(),
            res.json().await.unwrap_or(Value::Null),
        )
    }
}

fn set_mtime(path: &Path, secs: u64) {
    let t = UNIX_EPOCH + Duration::from_secs(secs);
    File::options()
        .write(true)
        .open(path)
        .or_else(|_| File::open(path))
        .unwrap()
        .set_times(FileTimes::new().set_modified(t).set_accessed(t))
        .unwrap();
}

/// Manifest rows without the absolute root, mtimes in whole seconds.
fn comparable(manifest: &Value) -> Vec<Value> {
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let mut f = f.clone();
            f["mtime"] = json!(f["mtime"].as_i64().unwrap() / 1000);
            f
        })
        .collect()
}

fn zstd_tar(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    build(&mut b);
    zstd::encode_all(&b.into_inner().unwrap()[..], 3).unwrap()
}

/// An entry with a raw name, past the tar crate's own path checks.
fn raw_entry(
    b: &mut tar::Builder<Vec<u8>>,
    name: &str,
    kind: tar::EntryType,
    link: Option<&str>,
    data: &[u8],
) {
    let mut h = tar::Header::new_gnu();
    {
        let gnu = h.as_gnu_mut().unwrap();
        gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        if let Some(l) = link {
            gnu.linkname[..l.len()].copy_from_slice(l.as_bytes());
        }
    }
    h.set_entry_type(kind);
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(1_600_000_000);
    h.set_cksum();
    b.append(&h, data).unwrap();
}

#[tokio::test]
async fn roundtrip_keeps_symlinks_empty_folders_mtimes_and_modes() {
    let w = world().await;
    let src = w.home.join("project");
    fs::create_dir_all(src.join("src/deep")).unwrap();
    fs::create_dir_all(src.join("empty")).unwrap();
    fs::create_dir_all(src.join("web/node_modules/pkg")).unwrap();
    fs::create_dir_all(src.join(".git")).unwrap();
    fs::write(src.join("README.md"), "hello\n").unwrap();
    fs::write(src.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(src.join(".env"), "SECRET=1\n").unwrap();
    let big: Vec<u8> = (0..700_000u32).map(|i| (i * 31 % 251) as u8).collect();
    fs::write(src.join("src/deep/data.bin"), &big).unwrap();
    fs::write(src.join("web/node_modules/pkg/index.js"), "x").unwrap();
    fs::write(src.join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(src.join("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(src.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
    symlink("../README.md", src.join("src/readme")).unwrap();
    set_mtime(&src.join("README.md"), 1_600_000_000);
    set_mtime(&src.join("src/deep/data.bin"), 1_650_000_123);
    set_mtime(&src.join("empty"), 1_500_000_000);
    let t = filetime::FileTime::from_unix_time(1_550_000_000, 0);
    filetime::set_symlink_file_times(src.join("src/readme"), t, t).unwrap();

    let excludes = json!(["node_modules", "target"]);
    let (status, manifest) = w
        .post("manifest", json!({ "path": src, "excludes": excludes }))
        .await;
    assert_eq!(status, 200, "{manifest}");
    let paths: Vec<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            ".env",
            ".git/HEAD",
            "README.md",
            "empty",
            "run.sh",
            "src/deep/data.bin",
            "src/readme",
            "web",
        ],
        "sorted; hidden files and .git included; `web` holds only excluded entries"
    );
    assert_eq!(manifest["excluded"], 1);
    assert_eq!(manifest["name"], "project");
    assert_eq!(manifest["count"], 8);
    let readme = &manifest["files"][6];
    assert_eq!(readme["kind"], "symlink");
    assert_eq!(readme["target"], "../README.md");
    assert_eq!(manifest["files"][3]["kind"], "dir");
    assert_eq!(manifest["files"][4]["mode"], 0o750);
    assert_eq!(manifest["files"][2]["mtime"], 1_600_000_000_000u64);

    let (status, archive) = w.export(json!({ "path": src, "excludes": excludes })).await;
    assert_eq!(status, 200);
    let dst = w.srv.join("copy");
    let (status, result) = w.import(&dst, "fresh", archive.clone()).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["written"], 8);
    assert_eq!(
        result["bytes"].as_u64().unwrap(),
        manifest["totalBytes"].as_u64().unwrap()
    );

    let (_, copied) = w
        .post("manifest", json!({ "path": dst, "excludes": excludes }))
        .await;
    assert_eq!(comparable(&copied), comparable(&manifest));
    assert_eq!(fs::read(dst.join("src/deep/data.bin")).unwrap(), big);
    assert_eq!(
        fs::read_link(dst.join("src/readme")).unwrap(),
        PathBuf::from("../README.md")
    );
    assert!(dst.join("empty").is_dir());
    assert!(!dst.join("web/node_modules").exists());

    // fresh refuses a non-empty folder; merge overwrites and keeps the rest.
    let (status, body) = w.import(&dst, "fresh", archive.clone()).await;
    assert_eq!(status, 409, "{body}");
    fs::write(dst.join("README.md"), "changed").unwrap();
    fs::write(dst.join("local.txt"), "kept").unwrap();
    let (status, body) = w.import(&dst, "merge", archive).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fs::read_to_string(dst.join("README.md")).unwrap(),
        "hello\n"
    );
    assert_eq!(fs::read_to_string(dst.join("local.txt")).unwrap(), "kept");

    // A subset export (sync) carries only the listed entries.
    let (status, subset) = w
        .export(json!({ "path": src, "excludes": excludes, "files": ["README.md", "empty"] }))
        .await;
    assert_eq!(status, 200);
    let only = w.srv.join("subset");
    let (status, body) = w.import(&only, "fresh", subset).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["written"], 2);
    assert!(only.join("README.md").is_file() && only.join("empty").is_dir());
    assert!(!only.join("src").exists());
}

#[tokio::test]
async fn roots_and_paths_outside_them() {
    let w = world().await;
    let (status, roots) = reqwest::Client::new()
        .get(format!("{}/api/transfer/roots", w.base))
        .bearer_auth("tok")
        .send()
        .await
        .map(|r| (r.status().as_u16(), r))
        .unwrap();
    assert_eq!(status, 200);
    let roots: Value = roots.json().await.unwrap();
    assert_eq!(roots["home"], json!(w.home));
    assert_eq!(roots["defaultParent"], json!(w.srv));
    assert_eq!(roots["roots"], json!([w.home, w.srv, w.srv.join("app")]));

    symlink(&w.outside, w.home.join("escape")).unwrap();
    for (path, code) in [
        (json!("/"), "invalid"),
        (json!(w.outside), "outside-roots"),
        (json!("relative/path"), "invalid"),
        (json!(format!("{}/../outside", w.home.display())), "invalid"),
        (json!(w.home.join("escape")), "outside-roots"),
    ] {
        let (status, body) = w.post("manifest", json!({ "path": path })).await;
        assert_eq!(status, 400, "{path}: {body}");
        assert_eq!(body["code"], code, "{path}: {body}");
    }
    let (status, _) = w
        .post("manifest", json!({ "path": w.home.join("missing") }))
        .await;
    assert_eq!(status, 404);

    let empty = zstd_tar(|_| {});
    let (status, body) = w.import(&w.outside.join("x"), "fresh", empty.clone()).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("outside-roots"))
    );
    let (status, body) = w
        .import(&w.home.join("escape"), "merge", empty.clone())
        .await;
    assert_eq!(status, 400, "a symlinked target is refused: {body}");
    let (status, body) = w.import(&w.home.join("escape/x"), "fresh", empty).await;
    assert_eq!(status, 400, "the canonical parent decides: {body}");
    assert!(!w.outside.join("x").exists());
}

#[tokio::test]
async fn import_rejects_escaping_entries() {
    let w = world().await;
    let dst = w.home.join("dst");

    let dotdot = zstd_tar(|b| raw_entry(b, "../evil", tar::EntryType::Regular, None, b"x"));
    let (status, body) = w.import(&dst, "fresh", dotdot).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("rejected")),
        "{body}"
    );
    assert!(!w.home.join("evil").exists());

    let abs = zstd_tar(|b| {
        raw_entry(b, "/dshx-transfer-abs", tar::EntryType::Regular, None, b"x");
    });
    let (status, body) = w.import(&dst, "merge", abs).await;
    assert_eq!(status, 400, "{body}");
    assert!(!Path::new("/dshx-transfer-abs").exists());

    // A symlink from the archive is created, but never written through.
    let target = w.outside.display().to_string();
    let through = zstd_tar(|b| {
        raw_entry(b, "ok.txt", tar::EntryType::Regular, None, b"fine");
        raw_entry(b, "link", tar::EntryType::Symlink, Some(&target), b"");
        raw_entry(b, "link/pwned", tar::EntryType::Regular, None, b"x");
        raw_entry(b, "later.txt", tar::EntryType::Regular, None, b"never");
    });
    let (status, body) = w.import(&dst, "merge", through).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("rejected")),
        "{body}"
    );
    assert_eq!(body["written"], 2, "{body}");
    assert!(!w.outside.join("pwned").exists());
    assert!(!dst.join("later.txt").exists());
    assert!(
        fs::symlink_metadata(dst.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // An existing symlink in the target is refused on the way, too.
    symlink(&w.outside, dst.join("old")).unwrap();
    let nested = zstd_tar(|b| raw_entry(b, "old/x", tar::EntryType::Regular, None, b"x"));
    let (status, body) = w.import(&dst, "merge", nested).await;
    assert_eq!(status, 400, "{body}");
    assert!(!w.outside.join("x").exists());

    // As the leaf it is replaced, not followed.
    fs::write(w.outside.join("victim"), "safe").unwrap();
    symlink(w.outside.join("victim"), dst.join("leaf")).unwrap();
    let leaf = zstd_tar(|b| raw_entry(b, "leaf", tar::EntryType::Regular, None, b"new"));
    let (status, body) = w.import(&dst, "merge", leaf).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        fs::read_to_string(w.outside.join("victim")).unwrap(),
        "safe"
    );
    assert_eq!(fs::read_to_string(dst.join("leaf")).unwrap(), "new");

    // A truncated upload fails.
    let mut cut = zstd_tar(|b| raw_entry(b, "big", tar::EntryType::Regular, None, &[7; 100_000]));
    cut.truncate(cut.len() / 2);
    let (status, body) = w.import(&w.home.join("cut"), "fresh", cut).await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test]
async fn delete_and_remove_stay_inside() {
    let w = world().await;
    let app = w.srv.join("app");
    fs::create_dir_all(app.join("gone/sub")).unwrap();
    fs::create_dir_all(app.join("emptied")).unwrap();
    fs::write(app.join("old.txt"), "x").unwrap();
    fs::write(app.join("gone/sub/f"), "x").unwrap();
    fs::write(w.outside.join("keep"), "x").unwrap();
    symlink(&w.outside, app.join("out")).unwrap();

    for bad in ["../srv", "/etc/passwd", "out/keep", ""] {
        let (status, body) = w
            .post("delete", json!({ "path": app, "files": [bad] }))
            .await;
        assert_eq!(status, 400, "{bad}: {body}");
    }
    assert!(w.outside.join("keep").exists());
    let (status, body) = w
        .post(
            "delete",
            json!({ "path": app, "files": ["old.txt", "gone/", "emptied", "missing", "out"] }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["deleted"], 4);
    assert!(!app.join("gone").exists() && !app.join("out").exists());
    assert!(w.outside.join("keep").exists(), "the symlink target stays");

    for refused in [
        &w.home,
        &w.srv,
        &w.home.parent().unwrap().to_path_buf(),
        &w.outside,
    ] {
        let (status, body) = w.post("remove", json!({ "path": refused })).await;
        assert_eq!(status, 400, "{}: {body}", refused.display());
        assert!(refused.exists());
    }
    symlink(&app, w.home.join("alias")).unwrap();
    let (status, _) = w
        .post("remove", json!({ "path": w.home.join("alias") }))
        .await;
    assert_eq!(status, 400, "a symlinked path is refused");
    assert!(app.exists());

    let (status, body) = w.post("remove", json!({ "path": app })).await;
    assert_eq!(status, 200, "{body}");
    assert!(!app.exists());
    assert!(w.host.workspaces.lock().unwrap().is_empty());
}
