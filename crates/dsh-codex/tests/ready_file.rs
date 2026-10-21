//! `dsh-codex web --ready-file`: the handshake the macOS desktop shell uses to
//! learn the UI's URL and token.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_dsh-codex");

fn get(port: u16, path: &str, token: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn ready_file_reports_url_and_token() {
    let home = tempfile::tempdir().unwrap();
    let ready = home.path().join("ready.json");
    let mut child = Command::new(BIN)
        .args(["web", "--no-open", "--port", "0", "--ready-file"])
        .arg(&ready)
        .current_dir(home.path())
        .env("DSHX_HOME", home.path())
        .env("DEEPSEEK_API_KEY", "sk-mock")
        .env_remove("DSH_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "no ready file");
        assert!(child.try_wait().unwrap().is_none(), "dsh-codex exited");
        std::thread::sleep(Duration::from_millis(50));
    }
    let info: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    let token = info["token"].as_str().unwrap();
    let port = u16::try_from(info["port"].as_u64().unwrap()).unwrap();
    assert_eq!(
        info["url"],
        format!("http://127.0.0.1:{port}/?token={token}").as_str()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&ready).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    assert!(get(port, "/api/info", token).starts_with("HTTP/1.1 200"));
    assert!(get(port, "/api/info", "wrong").starts_with("HTTP/1.1 401"));

    // SIGTERM shuts down cleanly and removes the file.
    #[cfg(unix)]
    {
        let status = Command::new("kill")
            .arg(child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        let exit = loop {
            if let Some(exit) = child.try_wait().unwrap() {
                break exit;
            }
            assert!(Instant::now() < deadline, "dsh-codex ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(exit.success(), "{exit:?}");
        assert!(!ready.exists());
    }
    #[cfg(not(unix))]
    child.kill().unwrap();
}
