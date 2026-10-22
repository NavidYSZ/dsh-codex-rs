//! Command confinement per `SandboxMode` (dsh `sandbox-local`, `bash-sandbox`)
//! and the shared escalation vocabulary (dsh `sandbox/escalation.ts`).
//!
//! Linux uses bubblewrap with dsh's exact profile; macOS uses `sandbox-exec`
//! with dsh's SBPL profile. Unlike dsh, a host without a usable runner does not
//! fail closed: commands run unconfined and a warning is logged once
//! (SPEC §5.3). File tools enforce the policy in-process either way.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use dshx_core::{ApprovalDecision, SandboxMode, ToolContext, ToolError};

/// The escalation targets the schemas advertise (dsh `ESCALATION_TARGETS`).
pub const ESCALATION_TARGETS: [SandboxMode; 2] =
    [SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess];

pub fn mode_name(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
        SandboxMode::DangerFullAccess => "danger-full-access",
    }
}

pub fn parse_mode(value: &str) -> Option<SandboxMode> {
    match value {
        "read-only" => Some(SandboxMode::ReadOnly),
        "workspace-write" => Some(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Some(SandboxMode::DangerFullAccess),
        _ => None,
    }
}

fn wider_modes(mode: SandboxMode) -> &'static [SandboxMode] {
    match mode {
        SandboxMode::ReadOnly => &[SandboxMode::WorkspaceWrite, SandboxMode::DangerFullAccess],
        SandboxMode::WorkspaceWrite => &[SandboxMode::DangerFullAccess],
        SandboxMode::DangerFullAccess => &[],
    }
}

/// `[sandbox: file access denied under <mode> mode]`
pub fn denial_marker(mode: SandboxMode) -> String {
    format!(
        "[sandbox: file access denied under {} mode]",
        mode_name(mode)
    )
}

/// The same-turn escalation hint after a denial (`subject`: `command`/`operation`).
pub fn escalation_hint(subject: &str) -> String {
    format!(
        "[sandbox: escalation available — retry this exact {subject} once with sandbox_permissions (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]"
    )
}

/// Note for a confinement runner that failed before the command ran.
pub fn runner_failed_marker(mode: SandboxMode) -> String {
    format!(
        "[sandbox: the sandbox runner itself failed under {} mode — the command did not run; this is a sandbox problem, not a command failure]",
        mode_name(mode)
    )
}

/// dsh `validateEscalationArgs`.
pub fn validate_escalation_args(
    permissions: Option<&str>,
    justification: Option<&str>,
) -> Result<(), ToolError> {
    let fail = |m: &str| Err(ToolError::Failed(m.to_owned()));
    if permissions.is_some() && justification.is_none() {
        return fail("invalid escalation: sandbox_permissions requires a justification");
    }
    if justification.is_some() && permissions.is_none() {
        return fail(
            "invalid escalation: justification is only valid together with sandbox_permissions",
        );
    }
    if justification.is_some_and(|j| j.trim().is_empty()) {
        return fail("invalid justification: expected a non-empty sentence");
    }
    Ok(())
}

/// dsh `approveEscalation`: strict widening, then the user's decision through
/// `ctx.approver`. `subject` is `command` (bash) or `operation` (write/edit);
/// `summary` is the one-line description shown to the user.
pub async fn approve_escalation(
    ctx: &ToolContext,
    tool: &str,
    subject: &str,
    requested: &str,
    justification: &str,
    effective: SandboxMode,
    summary: &str,
) -> Result<SandboxMode, ToolError> {
    if requested == mode_name(effective) {
        return Ok(effective);
    }
    let Some(mode) = parse_mode(requested).filter(|m| wider_modes(effective).contains(m)) else {
        return Err(ToolError::Failed(format!(
            "sandbox escalation to \"{requested}\" is not strictly wider than this call's current \"{}\" mode",
            mode_name(effective)
        )));
    };
    let summary = format!("escalate sandbox to {requested}: {summary}");
    let decision = tokio::select! {
        decision = ctx.approver.approve(&ctx.call_id, tool, &summary, Some(justification)) => decision,
        _ = ctx.cancel.cancelled() => ApprovalDecision::Cancelled,
    };
    match decision {
        ApprovalDecision::AllowedOnce => Ok(mode),
        ApprovalDecision::Rejected => Err(ToolError::Failed(format!(
            "the user rejected escalating this {subject} to \"{requested}\"; it stays denied, so stop and explain instead of working around it"
        ))),
        ApprovalDecision::Cancelled => Err(ToolError::Failed(format!(
            "approval for escalating to \"{requested}\" was cancelled"
        ))),
        ApprovalDecision::Unavailable => Err(ToolError::Failed(format!(
            "sandbox escalation to \"{requested}\" requires approval, but no approval channel is available"
        ))),
    }
}

/// `realpath`, or the path itself when it cannot be resolved (dsh `canonicalPath`).
pub fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Roots a `workspace-write` call may modify (dsh `writableRoots`).
pub fn writable_roots(mode: SandboxMode, workspace_root: &Path) -> Vec<PathBuf> {
    if mode != SandboxMode::WorkspaceWrite {
        return Vec::new();
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in [
        workspace_root.to_path_buf(),
        PathBuf::from("/tmp"),
        std::env::temp_dir(),
    ] {
        let canonical = canonical_path(&root);
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    roots
}

/// The confinement mechanism commands run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// Linux bubblewrap with dsh's mount profile.
    Bwrap,
    /// macOS `sandbox-exec` with dsh's SBPL profile.
    Seatbelt,
    /// An operator-supplied runner taking bwrap-compatible profile arguments
    /// (dsh `runnerCommand` + `runnerFailureSignatures`); also the test seam.
    Command {
        argv: Vec<String>,
        failure_signatures: Vec<String>,
    },
    /// No usable runner: commands run unconfined.
    Unconfined,
}

/// bwrap profile for one policy (dsh `bwrapProfileArgs`).
pub fn bwrap_profile_args(mode: SandboxMode, workspace_root: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if mode == SandboxMode::WorkspaceWrite {
        let root = workspace_root.to_string_lossy().into_owned();
        args.extend([
            "--tmpfs".into(),
            "/tmp".into(),
            "--bind".into(),
            root.clone(),
            root,
        ]);
    }
    args
}

fn sbpl_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `sandbox-exec` arguments for one policy (dsh `seatbeltProfileArgs`).
pub fn seatbelt_profile_args(mode: SandboxMode, workspace_root: &Path) -> Vec<String> {
    let mut forms = vec![
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        "(deny file-write*)".to_owned(),
        format!(
            "(allow file-write* (literal {}))",
            sbpl_string(Path::new("/dev/null"))
        ),
    ];
    let roots = writable_roots(mode, workspace_root);
    if !roots.is_empty() {
        let subpaths: Vec<String> = roots
            .iter()
            .map(|r| format!("(subpath {})", sbpl_string(r)))
            .collect();
        forms.push(format!("(allow file-write* {})", subpaths.join(" ")));
    }
    vec!["-p".to_owned(), forms.join(" ")]
}

fn probe(program: &str, args: &[String]) -> bool {
    let mut child = match std::process::Command::new(program)
        .args(args)
        .args(["--", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The host's runner, probed once per process.
pub fn host_runner() -> Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER
        .get_or_init(|| {
            let runner = if cfg!(target_os = "macos") {
                let exec = "/usr/bin/sandbox-exec";
                if probe(
                    exec,
                    &seatbelt_profile_args(SandboxMode::ReadOnly, Path::new("/")),
                ) {
                    Runner::Seatbelt
                } else {
                    Runner::Unconfined
                }
            } else if cfg!(target_os = "linux")
                && probe(
                    "bwrap",
                    &bwrap_profile_args(SandboxMode::ReadOnly, Path::new("/")),
                )
            {
                Runner::Bwrap
            } else {
                Runner::Unconfined
            };
            if runner == Runner::Unconfined {
                tracing::warn!(
                    "no usable sandbox runner (bubblewrap on Linux, sandbox-exec on macOS); \
                     commands run without file confinement"
                );
            }
            runner
        })
        .clone()
}

/// A command wrapped for one policy, plus how to classify its stderr.
#[derive(Debug, Clone)]
pub struct Confined {
    pub argv: Vec<String>,
    /// Set when a runner actually confines the command.
    pub enforcement: Option<&'static str>,
    pub denial_signatures: Vec<String>,
    pub failure_signatures: Vec<String>,
}

/// Wraps `bash -c <command>` for `mode` (confined modes only make sense for
/// `read-only` and `workspace-write`).
pub fn confine(
    runner: &Runner,
    command: &str,
    mode: SandboxMode,
    workspace_root: &Path,
) -> Confined {
    let inner = vec!["bash".to_owned(), "-c".to_owned(), command.to_owned()];
    confine_argv(runner, inner, mode, workspace_root)
}

/// Wraps any argv for `mode` (the persistent shell confines a whole `bash`).
pub fn confine_argv(
    runner: &Runner,
    inner: Vec<String>,
    mode: SandboxMode,
    workspace_root: &Path,
) -> Confined {
    let root = canonical_path(workspace_root);
    let wrap = |prefix: Vec<String>| -> Vec<String> {
        let mut argv = prefix;
        argv.push("--".into());
        argv.extend(inner.iter().cloned());
        argv
    };
    match runner {
        _ if mode == SandboxMode::DangerFullAccess => Confined {
            argv: inner.clone(),
            enforcement: None,
            denial_signatures: Vec::new(),
            failure_signatures: Vec::new(),
        },
        Runner::Bwrap => {
            let mut prefix = vec!["bwrap".to_owned()];
            prefix.extend(bwrap_profile_args(mode, &root));
            Confined {
                argv: wrap(prefix),
                enforcement: Some("full"),
                denial_signatures: vec!["read-only file system".into()],
                failure_signatures: vec!["bwrap: ".into()],
            }
        }
        Runner::Seatbelt => {
            let mut prefix = vec!["/usr/bin/sandbox-exec".to_owned()];
            prefix.extend(seatbelt_profile_args(mode, &root));
            Confined {
                argv: wrap(prefix),
                enforcement: Some("full"),
                denial_signatures: vec!["operation not permitted".into()],
                failure_signatures: vec!["sandbox-exec: ".into()],
            }
        }
        Runner::Command {
            argv,
            failure_signatures,
        } => {
            let mut prefix = argv.clone();
            prefix.extend(bwrap_profile_args(mode, &root));
            Confined {
                argv: wrap(prefix),
                enforcement: Some("full"),
                denial_signatures: vec!["read-only file system".into(), "permission denied".into()],
                failure_signatures: failure_signatures.clone(),
            }
        }
        Runner::Unconfined => Confined {
            argv: inner.clone(),
            enforcement: None,
            denial_signatures: Vec::new(),
            failure_signatures: Vec::new(),
        },
    }
}

/// dsh `matchesSignature`: a non-zero exit whose stderr names a denial.
pub fn matches_signature(exit_code: Option<i32>, stderr: &str, signatures: &[String]) -> bool {
    if matches!(exit_code, None | Some(0)) {
        return false;
    }
    let lowered = stderr.to_lowercase();
    signatures
        .iter()
        .any(|s| lowered.contains(&s.to_lowercase()))
}

/// dsh `classifyRunnerFailure`: the runner's own fatal line, if any.
pub fn runner_failure(
    exit_code: Option<i32>,
    stderr: &str,
    signatures: &[String],
) -> Option<String> {
    if matches!(exit_code, None | Some(0)) {
        return None;
    }
    let signatures: Vec<String> = signatures
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    stderr
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .find(|line| {
            let lowered = line.to_lowercase();
            signatures.iter().any(|s| lowered.contains(s))
        })
        .map(str::to_owned)
}

/// dsh `SandboxUnavailableError` text.
pub fn unavailable_message(mode: SandboxMode, detail: Option<&str>) -> String {
    let mut text = format!(
        "sandbox mode \"{}\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access.",
        mode_name(mode)
    );
    if let Some(detail) = detail {
        text.push_str(&format!(" Runner failure: {detail}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_profile_matches_dsh() {
        let args = bwrap_profile_args(SandboxMode::WorkspaceWrite, Path::new("/w"));
        assert_eq!(
            args,
            [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent",
                "--tmpfs",
                "/tmp",
                "--bind",
                "/w",
                "/w"
            ]
        );
        assert_eq!(
            bwrap_profile_args(SandboxMode::ReadOnly, Path::new("/w")).len(),
            9
        );
    }

    #[test]
    fn seatbelt_profile_matches_dsh() {
        let args = seatbelt_profile_args(SandboxMode::ReadOnly, Path::new("/w"));
        assert_eq!(
            args[1],
            "(version 1) (allow default) (deny file-write*) (allow file-write* (literal \"/dev/null\"))"
        );
        let args =
            seatbelt_profile_args(SandboxMode::WorkspaceWrite, Path::new("/nonexistent-root"));
        assert!(args[1].contains("(allow file-write* (subpath \"/nonexistent-root\")"));
    }

    #[test]
    fn classification() {
        let sigs = vec!["read-only file system".to_owned()];
        assert!(matches_signature(
            Some(1),
            "touch: x: Read-only file system",
            &sigs
        ));
        assert!(!matches_signature(Some(0), "Read-only file system", &sigs));
        let fail = vec!["bwrap: ".to_owned()];
        assert_eq!(
            runner_failure(Some(1), "info\nbwrap: Can't mount proc\n", &fail).as_deref(),
            Some("bwrap: Can't mount proc")
        );
        assert_eq!(runner_failure(Some(0), "bwrap: x", &fail), None);
    }

    #[test]
    fn escalation_validation() {
        assert!(validate_escalation_args(None, None).is_ok());
        let e = validate_escalation_args(Some("danger-full-access"), None).unwrap_err();
        assert_eq!(
            e.to_string(),
            "invalid escalation: sandbox_permissions requires a justification"
        );
        let e = validate_escalation_args(None, Some("x")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "invalid escalation: justification is only valid together with sandbox_permissions"
        );
        let e = validate_escalation_args(Some("danger-full-access"), Some(" ")).unwrap_err();
        assert_eq!(
            e.to_string(),
            "invalid justification: expected a non-empty sentence"
        );
    }
}
