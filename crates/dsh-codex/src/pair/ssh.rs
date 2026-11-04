//! `dsh-codex pair --ssh <host>`: pairs the phone with a server that is only
//! reachable over SSH. The phone gets its own ed25519 key, installed
//! restricted in the login user's `authorized_keys`; the client token comes
//! from the server's own `pair` when its build has one, else the app reads
//! the start token from the journal (a forced command on the key).
use std::io::Write as _;
use std::process::{Command, Stdio};

use dshx_server::mobile::{ClientRef, Payload, ServerInfo, Transport, pairing};
use sha2::Digest as _;

use super::Args;

/// What `ssh -G` resolves for the host.
struct Target {
    hostname: String,
    port: u16,
    user: String,
    known_hosts: Vec<String>,
}

/// A client the remote `pair` made.
struct Remote {
    server: ServerInfo,
    client_id: String,
    token: String,
}

pub(super) fn pair(args: &Args, host: &str) -> anyhow::Result<Payload> {
    let target = resolve(args, host)?;
    eprintln!(
        "dsh-codex pair: pairing {}@{}:{} …",
        target.user, target.hostname, target.port
    );
    let (mut server, client_id, token, token_command) = match remote_pair(args, host) {
        Ok(remote) => {
            eprintln!("dsh-codex pair: client token made on the server");
            (remote.server, remote.client_id, Some(remote.token), None)
        }
        Err(e) => {
            eprintln!(
                "dsh-codex pair: no client token from the server ({e}); the app will read the \
                 start token from the journal of {}",
                args.unit
            );
            (
                fallback_server(&target),
                format!("cli_{}", uuid::Uuid::new_v4().simple()),
                None,
                Some(token_command(&args.unit)),
            )
        }
    };
    let (private_key, public_line) = make_key(&format!("aether-{client_id}"))?;
    let mut options = format!(
        "restrict,port-forwarding,permitopen=\"127.0.0.1:{}\"",
        args.remote_port
    );
    if let Some(command) = &token_command {
        options.push_str(&format!(",command=\"{command}\""));
    }
    install_key(args, host, &client_id, &format!("{options} {public_line}"))?;
    eprintln!(
        "dsh-codex pair: key installed in ~/.ssh/authorized_keys of {}@{}",
        target.user, target.hostname
    );
    let host_key_sha256 = host_key_fingerprint(&target)?;
    if let Some(name) = &args.name {
        server.name = name.clone();
    }
    Ok(Payload {
        v: pairing::VERSION,
        server,
        client: ClientRef {
            id: client_id,
            name: args.client_name.clone(),
        },
        token,
        transports: vec![Transport::Ssh {
            host: args
                .public_host
                .clone()
                .unwrap_or_else(|| target.hostname.clone()),
            port: target.port,
            user: target.user,
            host_key_sha256,
            private_key,
            remote_host: "127.0.0.1".into(),
            remote_port: args.remote_port,
            token_command,
        }],
    })
}

/// `ssh -F <config> -o BatchMode=yes [-l user]`.
fn ssh(args: &Args) -> Command {
    let mut cmd = Command::new("ssh");
    if let Some(config) = &args.ssh_config {
        cmd.arg("-F").arg(config);
    }
    cmd.args(["-o", "BatchMode=yes"]);
    if let Some(user) = &args.user {
        cmd.arg("-l").arg(user);
    }
    cmd
}

fn stderr_of(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_owned()
}

/// `ssh -G <host>`: host name, port, user and known_hosts files.
fn resolve(args: &Args, host: &str) -> anyhow::Result<Target> {
    let out = ssh(args).arg("-G").arg(host).output()?;
    anyhow::ensure!(out.status.success(), "ssh -G {host}: {}", stderr_of(&out));
    let mut target = Target {
        hostname: host.to_owned(),
        port: 22,
        user: String::new(),
        known_hosts: Vec::new(),
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "hostname" => target.hostname = value.trim().to_owned(),
            "port" => target.port = value.trim().parse().unwrap_or(22),
            "user" => target.user = value.trim().to_owned(),
            "userknownhostsfile" => {
                target.known_hosts = value.split_whitespace().map(expand_home).collect();
            }
            _ => {}
        }
    }
    anyhow::ensure!(
        !target.user.is_empty(),
        "ssh -G {host} names no user; pass --user"
    );
    Ok(target)
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_owned(),
    }
}

/// Single-quotes a word for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Runs the server's own `pair` as the remote user and reads its payload.
fn remote_pair(args: &Args, host: &str) -> anyhow::Result<Remote> {
    let script = format!(
        "sudo -n -u {} env {} {} pair --format json --no-https --name {}",
        quote(&args.remote_user),
        quote(&format!("DSHX_HOME={}", args.remote_home)),
        quote(&args.remote_bin),
        quote(&args.client_name),
    );
    let out = ssh(args)
        .arg(host)
        .arg(script)
        .stdin(Stdio::null())
        .output()?;
    anyhow::ensure!(out.status.success(), "{}", stderr_of(&out));
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("the server's pair printed no payload: {e}"))?;
    let server: ServerInfo = serde_json::from_value(value["server"].clone())
        .map_err(|e| anyhow::anyhow!("payload without server: {e}"))?;
    let client_id = value["client"]["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("payload without client id"))?
        .to_owned();
    let token = value["token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("payload without token"))?
        .to_owned();
    Ok(Remote {
        server,
        client_id,
        token,
    })
}

/// A server without `pair`: a stable id from host and port, so pairing
/// again replaces the device in the app.
fn fallback_server(target: &Target) -> ServerInfo {
    let digest = sha2::Sha256::digest(format!("ssh:{}:{}", target.hostname, target.port));
    let id: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    ServerInfo {
        id: format!("srv_{id}"),
        name: target.hostname.clone(),
        hostname: target.hostname.clone(),
        platform: "linux".into(),
        version: "unknown".into(),
    }
}

/// The start token from the unit's journal, as the Mac app reads it.
fn token_command(unit: &str) -> String {
    format!(
        "sudo -n journalctl -u {unit} -b -o cat --no-pager | grep -oE 'token=[0-9a-f]+' | tail -n 1 | cut -d= -f2"
    )
}

/// An ed25519 key for the phone: the OpenSSH private key and the public line
/// (`ssh-ed25519 <key> <comment>`).
fn make_key(comment: &str) -> anyhow::Result<(String, String)> {
    use ssh_key::rand_core::OsRng;
    let mut key = ssh_key::PrivateKey::random(&mut OsRng, ssh_key::Algorithm::Ed25519)?;
    key.set_comment(comment);
    let private = key.to_openssh(ssh_key::LineEnding::LF)?.to_string();
    let public = key.public_key().to_openssh()?;
    Ok((private, public))
}

/// Replaces the line with this client's comment in the remote
/// `~/.ssh/authorized_keys` (or appends it); the line travels on stdin so
/// no quoting touches it.
fn install_key(args: &Args, host: &str, client_id: &str, line: &str) -> anyhow::Result<()> {
    let script = format!(
        "umask 077; d=\"$HOME/.ssh\"; f=\"$d/authorized_keys\"; mkdir -p \"$d\"; chmod 700 \"$d\"; \
         [ -f \"$f\" ] || : > \"$f\"; line=$(cat); \
         {{ grep -v -F -e \" aether-{client_id}\" \"$f\"; printf \"%s\\n\" \"$line\"; }} > \"$f.aether\" \
         && cat \"$f.aether\" > \"$f\" && rm -f \"$f.aether\""
    );
    let mut child = ssh(args)
        .arg(host)
        .arg(format!("sh -c '{script}'"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin.write_all(format!("{line}\n").as_bytes())?;
    drop(stdin);
    let out = child.wait_with_output()?;
    anyhow::ensure!(
        out.status.success(),
        "installing the key on {host}: {}",
        stderr_of(&out)
    );
    Ok(())
}

/// `SHA256:…` of the host key: from the known_hosts files of the SSH config,
/// else from `ssh-keyscan`. Ed25519 first, then ECDSA, then RSA.
fn host_key_fingerprint(target: &Target) -> anyhow::Result<String> {
    let name = if target.port == 22 {
        target.hostname.clone()
    } else {
        format!("[{}]:{}", target.hostname, target.port)
    };
    let mut found: Vec<(String, String)> = Vec::new();
    for file in &target.known_hosts {
        if !std::path::Path::new(file).is_file() {
            continue;
        }
        let out = Command::new("ssh-keygen")
            .args(["-l", "-F", &name, "-f", file])
            .output()?;
        // `# Host x found: line 3` then `x ED25519 SHA256:…`.
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let [_, kind, fp, ..] = parts.as_slice()
                && fp.starts_with("SHA256:")
            {
                found.push((kind.to_uppercase(), (*fp).to_owned()));
            }
        }
    }
    if found.is_empty() {
        let scan = Command::new("ssh-keyscan")
            .args([
                "-p",
                &target.port.to_string(),
                "-T",
                "5",
                "-t",
                "ed25519,ecdsa,rsa",
            ])
            .arg(&target.hostname)
            .stderr(Stdio::null())
            .output()?;
        let mut keygen = Command::new("ssh-keygen")
            .args(["-l", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        keygen
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(&scan.stdout)?;
        let out = keygen.wait_with_output()?;
        // `256 SHA256:… host (ED25519)`.
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let fp = line.split_whitespace().find(|w| w.starts_with("SHA256:"));
            let kind = line.rsplit('(').next().and_then(|k| k.strip_suffix(')'));
            if let (Some(fp), Some(kind)) = (fp, kind) {
                found.push((kind.to_uppercase(), fp.to_owned()));
            }
        }
    }
    let rank = |kind: &str| match kind {
        "ED25519" => 0,
        k if k.starts_with("ECDSA") => 1,
        "RSA" => 2,
        _ => 3,
    };
    found.sort_by_key(|(kind, _)| rank(kind));
    found
        .into_iter()
        .next()
        .map(|(_, fp)| fp)
        .ok_or_else(|| anyhow::anyhow!("no host key found for {name}: connect once with ssh first"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_ed25519_with_the_comment() {
        let (private, public) = make_key("aether-cli_1").unwrap();
        assert!(private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
        assert!(private.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
        assert!(public.starts_with("ssh-ed25519 AAAA"));
        assert!(public.ends_with(" aether-cli_1"));
        assert_eq!(public.lines().count(), 1);
    }

    #[test]
    fn fallback_id_is_stable_and_the_command_matches_the_mac_app() {
        let target = Target {
            hostname: "130.162.241.44".into(),
            port: 22,
            user: "ubuntu".into(),
            known_hosts: vec![],
        };
        let a = fallback_server(&target);
        assert_eq!(a, fallback_server(&target));
        assert!(a.id.starts_with("srv_") && a.id.len() == 36);
        assert_eq!(
            token_command("deepseek-harness"),
            "sudo -n journalctl -u deepseek-harness -b -o cat --no-pager | grep -oE 'token=[0-9a-f]+' | tail -n 1 | cut -d= -f2"
        );
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
