//! Discovers locally installed coding-agent CLIs and builds launch argv.
//!
//! Callers only need [`detect`], [`DetectedAgent::plan`], and [`RunHandle`] to
//! stop a run. Binary names, extra PATH entries, per-agent flags, and process
//! signalling stay inside this module.

use std::io::Read;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

/// Kind of coding agent Sillage knows how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    CodexCli,
    CursorCli,
    DeepSeekHarness,
}

impl AgentKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::CodexCli => "Codex CLI",
            Self::CursorCli => "Cursor CLI",
            Self::DeepSeekHarness => "DeepSeek Harness",
        }
    }
}

/// A coding-agent binary found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedAgent {
    pub kind: AgentKind,
    pub program: PathBuf,
}

/// Command line that will be spawned for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// How a finished agent run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Done(String),
    Failed(String),
    Stopped,
}

/// Control point for one in-flight run, shared between the UI and the thread
/// that blocks on the agent process.
///
/// [`RunHandle::cancel`] returns as soon as the signal is sent, so it is safe
/// to call from the main thread while the run is still reading output.
#[derive(Clone, Default)]
pub struct RunHandle {
    slot: Arc<Mutex<Slot>>,
}

#[derive(Default)]
struct Slot {
    child: Option<Child>,
    cancelled: bool,
}

impl RunHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the run. Signals the whole process group so tools the agent
    /// launched stop with it, and takes effect even when the process has not
    /// been spawned yet.
    pub fn cancel(&self) {
        let mut slot = self.slot();
        slot.cancelled = true;
        if let Some(child) = &slot.child {
            terminate_group(child.id());
        }
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Take ownership of the spawned process, killing it right away when a
    /// cancel landed while it was starting up.
    fn attach(&self, child: Child) {
        let mut slot = self.slot();
        if slot.cancelled {
            terminate_group(child.id());
        }
        slot.child = Some(child);
    }

    fn cancelled(&self) -> bool {
        self.slot().cancelled
    }

    /// Reap the process once its pipes are closed. The lock is released before
    /// the wait, so a concurrent [`Self::cancel`] never blocks.
    fn reap(&self) -> Option<ExitStatus> {
        let child = self.slot().child.take();
        child.and_then(|mut child| child.wait().ok())
    }
}

fn terminate_group(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGTERM);
    }
}

impl DetectedAgent {
    /// Build argv for a non-interactive run in `cwd`.
    pub fn plan(&self, prompt: &str, cwd: &Path, full_access: bool) -> LaunchPlan {
        LaunchPlan {
            program: self.program.clone(),
            args: launch_args(self.kind, prompt, full_access),
            cwd: cwd.to_path_buf(),
        }
    }

    /// Run the plan to completion and return combined stdout/stderr.
    ///
    /// Blocks the calling thread; `handle` is the only way to end the run
    /// early.
    pub fn run(
        &self,
        prompt: &str,
        cwd: &Path,
        full_access: bool,
        handle: &RunHandle,
    ) -> RunOutcome {
        let plan = self.plan(prompt, cwd, full_access);
        execute(&plan, self.kind.display_name(), handle)
    }
}

fn execute(plan: &LaunchPlan, label: &str, handle: &RunHandle) -> RunOutcome {
    let mut child = match Command::new(&plan.program)
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group so cancelling reaches the agent's subprocesses.
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return RunOutcome::Failed(format!("failed to start {label}: {err}")),
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    handle.attach(child);
    // Both pipes are drained at once; a full stderr buffer would otherwise
    // stall the agent while we wait on stdout.
    let draining = thread::spawn(move || drain(stderr));
    let mut text = drain(stdout);
    let errors = draining.join().unwrap_or_default();
    let status = handle.reap();

    if handle.cancelled() {
        return RunOutcome::Stopped;
    }
    if !errors.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&errors);
    }
    match status {
        Some(status) if status.success() => RunOutcome::Done(text),
        Some(status) => {
            let code = status
                .code()
                .map(|code| format!("exit {code}"))
                .unwrap_or_else(|| "terminated".into());
            RunOutcome::Failed(format!("{code}\n{text}"))
        }
        None => RunOutcome::Failed(format!("lost track of {label}\n{text}")),
    }
}

fn drain(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut raw = Vec::new();
    if pipe.read_to_end(&mut raw).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&raw).into_owned()
}

fn launch_args(kind: AgentKind, prompt: &str, full_access: bool) -> Vec<String> {
    match kind {
        AgentKind::ClaudeCode => {
            let mut args = vec!["-p".into(), "--output-format".into(), "text".into()];
            if full_access {
                args.push("--dangerously-skip-permissions".into());
            }
            args.push(prompt.into());
            args
        }
        AgentKind::CodexCli => {
            let mut args = vec!["exec".into()];
            if full_access {
                args.push("--full-auto".into());
            }
            args.push(prompt.into());
            args
        }
        AgentKind::CursorCli => {
            let mut args = vec!["-p".into(), "--output-format".into(), "text".into()];
            if full_access {
                args.push("--force".into());
            }
            args.push(prompt.into());
            args
        }
        AgentKind::DeepSeekHarness => {
            vec!["--profile".into(), "headless".into(), prompt.into()]
        }
    }
}

/// Scan PATH (plus common install prefixes) for known agent CLIs.
pub fn detect() -> Vec<DetectedAgent> {
    let mut found = Vec::new();
    for &(kind, names) in CANDIDATES {
        if let Some(program) = names.iter().find_map(|name| lookup(name)) {
            found.push(DetectedAgent { kind, program });
        }
    }
    found
}

const CANDIDATES: &[(AgentKind, &[&str])] = &[
    (AgentKind::ClaudeCode, &["claude"]),
    (AgentKind::CodexCli, &["codex"]),
    (AgentKind::CursorCli, &["cursor-agent", "cursor"]),
    (AgentKind::DeepSeekHarness, &["dsh"]),
];

fn lookup(bin: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(bin) {
        return Some(path);
    }
    extra_bin_dirs()
        .into_iter()
        .map(|dir| dir.join(bin))
        .find(|path| path.is_file())
}

fn extra_bin_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".grok/bin"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_full_access_adds_permission_flag() {
        let args = launch_args(AgentKind::ClaudeCode, "build it", true);
        assert!(args.contains(&"--dangerously-skip-permissions".into()));
        assert_eq!(args.last().unwrap(), "build it");
    }

    #[test]
    fn cursor_print_mode_is_non_interactive() {
        let args = launch_args(AgentKind::CursorCli, "fix tests", false);
        assert_eq!(args[0], "-p");
        assert!(!args.iter().any(|a| a == "--force"));
    }

    #[test]
    fn dsh_uses_headless_profile() {
        let args = launch_args(AgentKind::DeepSeekHarness, "run tests", false);
        assert_eq!(args, vec!["--profile", "headless", "run tests"]);
    }

    #[test]
    fn cancel_before_spawn_still_stops_the_run() {
        let handle = RunHandle::new();
        handle.cancel();
        assert!(handle.cancelled());
    }

    #[test]
    fn cancelling_a_running_process_reports_stopped() {
        let plan = LaunchPlan {
            program: PathBuf::from("/bin/sleep"),
            args: vec!["30".into()],
            cwd: PathBuf::from("/"),
        };
        let handle = RunHandle::new();
        let stopper = handle.clone();
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(100));
            stopper.cancel();
        });
        let started = std::time::Instant::now();
        assert_eq!(execute(&plan, "sleep", &handle), RunOutcome::Stopped);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn output_and_errors_are_combined() {
        let plan = LaunchPlan {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "echo out; echo boom >&2".into()],
            cwd: PathBuf::from("/"),
        };
        let outcome = execute(&plan, "sh", &RunHandle::new());
        assert_eq!(outcome, RunOutcome::Done("out\nboom\n".into()));
    }
}
