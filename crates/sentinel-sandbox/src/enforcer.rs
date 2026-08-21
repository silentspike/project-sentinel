//! SandboxEnforcer — zentrale Facade fuer Landlock + cgroups + bwrap.
//!
//! Orchestriert alle drei Isolationsmechanismen:
//! - Landlock LSM (Kernel-Level Filesystem-Restriktion)
//! - cgroups v2 (CPU, Memory, PID Limits)
//! - bwrap (Namespace-Isolation: PID, Mount, UTS)
//!
//! Lifecycle:
//! 1. `detect()` — prueft verfuegbare Kernel-Features, setzt OOM-Score
//! 2. `setup_agent()` — erstellt cgroup + Agent-Home
//! 3. `start_agent_process()` — startet bwrap (spaeter: mit Landlock im Child)
//! 4. `teardown_agent()` — beendet bwrap-Reste + entfernt cgroup

use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::bwrap::{BwrapConfig, SpawnedSandbox};
use crate::cgroups::{self, CgroupLimits, PsiMetrics};
use crate::landlock;

const PROTOCOL_LINE_LIMIT_BYTES: usize = 1024 * 1024;
const PROTOCOL_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
const PROTOCOL_QUEUE_DEPTH: usize = 64;
const PROTOCOL_CANCEL_GRACE_MS: u64 = 1_000;
const PROTOCOL_WRITE_TIMEOUT_MS: u64 = 250;

const PROTOCOL_TERMINAL_SETTLE_MS: u64 = 25;
const WORKBENCH_ATTESTATION_SCHEMA_VERSION: u16 = 1;
const WORKBENCH_ATTESTATION_TIMEOUT_MS: u64 = 2_000;
const WORKBENCH_ATTESTATION_MAX_BYTES: u64 = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkbenchIsolationAttestation {
    pub(crate) child_pid: u32,
    pub(crate) landlock_abi: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkbenchStartupAttestation {
    schema_version: u16,
    nonce: String,
    wrapper_version: String,
    runtime_version: String,
    landlock_abi: u8,
    host_pid: u32,
}

enum ProtocolFrame {
    Line(String),
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolSupervisionFailure {
    InvalidFrame,
    ProtocolViolation,
    UnsupportedVersion,
    InvocationConflict,
    OutputLimitExceeded,
    ChannelDisconnected,
    DeadlineExceeded,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolCancelOwner {
    Explicit,
    Deadline,
}

#[derive(Debug, Clone, Copy)]
struct ProtocolCancelClaim {
    owner: ProtocolCancelOwner,
    claimed_at: std::time::Instant,
    requested_at_unix_ms: u64,
    send_started: bool,
    #[cfg(test)]
    sent: bool,
}

#[derive(Debug, Clone, Copy)]
enum ProtocolOutcome {
    Running,
    ProcessExitedPending { observed_at: std::time::Instant },
    TerminalPending { observed_at: std::time::Instant },
    FailurePending(ProtocolSupervisionFailure),
    FinalTerminal,
    FinalFailure(ProtocolSupervisionFailure),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProtocolSupervisionSnapshot {
    pub(crate) failure: Option<ProtocolSupervisionFailure>,
    pub(crate) cancel_requested_at_ms: Option<u64>,
    pub(crate) cancel_owner: Option<ProtocolCancelOwner>,
    #[cfg(test)]
    pub(crate) cancel_sent: bool,
    pub(crate) terminal_finalized: bool,
    #[cfg(test)]
    pub(crate) process_reaped: bool,
    #[cfg(test)]
    pub(crate) reader_closed: bool,
    #[cfg(test)]
    pub(crate) stdin_closed: bool,
    pub(crate) cgroup_quiesced: bool,
}

#[derive(Debug, Clone, Default)]
struct ProtocolReaderState {
    invocation_id: Option<String>,
    retained_bytes: usize,
    result_seen: bool,
    terminal: bool,
}

struct ProtocolSupervisionState {
    reader: ProtocolReaderState,
    deadline_unix_ms: u64,
    execute_sent: bool,
    cancel: Option<ProtocolCancelClaim>,
    outcome: ProtocolOutcome,
    reader_closed: bool,
    stop: bool,
    cgroup_quiesced: bool,
}

impl Default for ProtocolSupervisionState {
    fn default() -> Self {
        Self {
            reader: ProtocolReaderState::default(),
            deadline_unix_ms: 0,
            execute_sent: false,
            cancel: None,
            outcome: ProtocolOutcome::Running,
            reader_closed: false,
            stop: false,
            cgroup_quiesced: false,
        }
    }
}

struct ProtocolSupervision {
    state: Mutex<ProtocolSupervisionState>,
    #[cfg(test)]
    deadline_send_barrier: Mutex<Option<ProtocolDeadlineSendBarrier>>,
    #[cfg(test)]
    post_terminal_barrier: Mutex<Option<ProtocolPostTerminalBarrier>>,
    #[cfg(test)]
    pre_quiescence_barrier: Mutex<Option<ProtocolPreQuiescenceBarrier>>,
}

#[cfg(test)]
#[derive(Clone)]
struct ProtocolDeadlineSendBarrier {
    claimed: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
#[derive(Clone)]
struct ProtocolPostTerminalBarrier {
    observed: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

#[cfg(test)]
#[derive(Clone)]
struct ProtocolPreQuiescenceBarrier {
    observed: Arc<std::sync::Barrier>,
    release: Arc<std::sync::Barrier>,
}

impl Default for ProtocolSupervision {
    fn default() -> Self {
        Self {
            state: Mutex::new(ProtocolSupervisionState::default()),
            #[cfg(test)]
            deadline_send_barrier: Mutex::new(None),
            #[cfg(test)]
            post_terminal_barrier: Mutex::new(None),
            #[cfg(test)]
            pre_quiescence_barrier: Mutex::new(None),
        }
    }
}

impl ProtocolSupervision {
    fn lock(&self) -> std::sync::MutexGuard<'_, ProtocolSupervisionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn snapshot(&self, _process_reaped: bool, _stdin_closed: bool) -> ProtocolSupervisionSnapshot {
        let state = self.lock();
        let (failure, terminal_finalized) = match state.outcome {
            ProtocolOutcome::FinalFailure(failure) => (Some(failure), false),
            ProtocolOutcome::FinalTerminal => (None, true),
            _ => (None, false),
        };
        ProtocolSupervisionSnapshot {
            failure,
            cancel_requested_at_ms: state.cancel.map(|claim| claim.requested_at_unix_ms),
            cancel_owner: state.cancel.map(|claim| claim.owner),
            #[cfg(test)]
            cancel_sent: state.cancel.is_some_and(|claim| claim.sent),
            terminal_finalized,
            #[cfg(test)]
            process_reaped: _process_reaped,
            #[cfg(test)]
            reader_closed: state.reader_closed,
            #[cfg(test)]
            stdin_closed: _stdin_closed,
            cgroup_quiesced: state.cgroup_quiesced,
        }
    }

    #[cfg(test)]
    fn install_deadline_send_barrier(
        &self,
        claimed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        *self
            .deadline_send_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(ProtocolDeadlineSendBarrier { claimed, release });
    }

    #[cfg(test)]
    fn wait_at_deadline_send_barrier(&self) {
        let barrier = self
            .deadline_send_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(barrier) = barrier {
            barrier.claimed.wait();
            barrier.release.wait();
        }
    }

    #[cfg(test)]
    fn install_post_terminal_barrier(
        &self,
        observed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        *self
            .post_terminal_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(ProtocolPostTerminalBarrier { observed, release });
    }

    #[cfg(test)]
    fn wait_at_post_terminal_barrier(&self) {
        let barrier = self
            .post_terminal_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(barrier) = barrier {
            barrier.observed.wait();
            barrier.release.wait();
        }
    }

    #[cfg(test)]
    fn install_pre_quiescence_barrier(
        &self,
        observed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        *self
            .pre_quiescence_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(ProtocolPreQuiescenceBarrier { observed, release });
    }

    #[cfg(test)]
    fn wait_at_pre_quiescence_barrier(&self) {
        let barrier = self
            .pre_quiescence_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(barrier) = barrier {
            barrier.observed.wait();
            barrier.release.wait();
        }
    }
}

struct ProcessTermination {
    started: AtomicBool,
    reaped: AtomicBool,
    signal_attempts: Arc<AtomicUsize>,
    #[cfg(test)]
    reap_publications: AtomicUsize,
}

struct ProtocolReaderCloseGuard(Arc<ProtocolSupervision>);

impl Drop for ProtocolReaderCloseGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.reader_closed = true;
        if matches!(
            state.outcome,
            ProtocolOutcome::Running | ProtocolOutcome::ProcessExitedPending { .. }
        ) {
            state.outcome =
                ProtocolOutcome::FailurePending(ProtocolSupervisionFailure::ChannelDisconnected);
        }
    }
}

impl Default for ProcessTermination {
    fn default() -> Self {
        Self {
            started: AtomicBool::new(false),
            reaped: AtomicBool::new(false),
            signal_attempts: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            reap_publications: AtomicUsize::new(0),
        }
    }
}

pub(crate) struct ProtocolDrain {
    pub(crate) lines: Vec<String>,
    pub(crate) queue_overflowed: bool,
}

/// Handle fuer einen laufenden Agent-Prozess in bwrap.
///
/// Haelt den Child-Handle am Leben (bwrap hat --die-with-parent,
/// stirbt also wenn der Daemon stirbt). stdin ist piped fuer
/// stream-json Kommunikation mit dem Agent.
pub struct AgentProcess {
    /// PID des bwrap-Supervisor-Prozesses (bleibt by-design im Root-netns;
    /// genutzt fuer cgroup-Membership und SIGTERM).
    pub pid: u32,
    /// PID des sandboxed `agent-runtime` im Agent-netns (aus bwrap `--info-fd`).
    /// `None`, falls bwrap ihn nicht meldete -> netns-Verifikation entfaellt;
    /// das bwrap-Exit bleibt das primaere fail-closed-Signal (#75).
    pub child_pid: Option<u32>,
    /// Child handle — NICHT droppen solange Agent laufen soll.
    child: Arc<Mutex<Child>>,
    protocol_stdin: Arc<Mutex<Option<ChildStdin>>>,
    protocol_stdout: Option<Receiver<ProtocolFrame>>,
    protocol_queue_overflowed: Arc<AtomicBool>,
    protocol_reader: Option<std::thread::JoinHandle<()>>,
    protocol_supervisor: Option<std::thread::JoinHandle<()>>,
    supervision: Arc<ProtocolSupervision>,
    termination: Arc<ProcessTermination>,
    supervised_cgroup: Option<String>,
    workbench_isolation: Option<WorkbenchIsolationAttestation>,
}

impl AgentProcess {
    fn from_child(mut child: Child, child_pid: Option<u32>) -> Self {
        let pid = child.id();
        let protocol_stdin = child.stdin.take().and_then(|stdin| {
            let configured = nix::fcntl::fcntl(&stdin, nix::fcntl::FcntlArg::F_GETFL)
                .map(nix::fcntl::OFlag::from_bits_truncate)
                .and_then(|flags| {
                    nix::fcntl::fcntl(
                        &stdin,
                        nix::fcntl::FcntlArg::F_SETFL(flags | nix::fcntl::OFlag::O_NONBLOCK),
                    )
                })
                .is_ok();
            if !configured {
                None
            } else {
                Some(stdin)
            }
        });
        let protocol_stdin = Arc::new(Mutex::new(protocol_stdin));
        let supervision = Arc::new(ProtocolSupervision::default());
        let (protocol_stdout, protocol_queue_overflowed, protocol_reader) =
            protocol_reader_parts(child.stdout.take(), Arc::clone(&supervision));
        Self {
            pid,
            child_pid,
            child: Arc::new(Mutex::new(child)),
            protocol_stdin,
            protocol_stdout,
            protocol_queue_overflowed,
            protocol_reader,
            protocol_supervisor: None,
            supervision,
            termination: Arc::new(ProcessTermination::default()),
            supervised_cgroup: None,
            workbench_isolation: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn launch_fixture() -> Result<Self> {
        let mut command = std::process::Command::new("/usr/bin/sleep");
        command
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let child = command.spawn().context("start sandbox lifecycle fixture")?;
        Ok(Self::from_child(child, None))
    }

    #[cfg(test)]
    pub(crate) fn launch_protocol_fixture(lines: &[&str]) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "IFS= read -r frame; if [ \"$#\" -gt 0 ]; then printf '%s\\n' \"$@\"; fi; sleep 5",
                "fixture",
            ])
            .args(lines)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let child = command.spawn().context("start sandbox protocol fixture")?;
        Ok(Self::from_child(child, None))
    }

    #[cfg(test)]
    pub(crate) fn launch_raw_protocol_fixture(script: &str) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let child = command
            .spawn()
            .context("start raw sandbox protocol fixture")?;
        Ok(Self::from_child(child, None))
    }

    #[cfg(test)]
    pub(crate) fn launch_recording_protocol_fixture(
        lines: &[&str],
        record_path: &Path,
        descendant_pid_path: &Path,
    ) -> Result<Self> {
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "record=$1; descendant_file=$2; shift 2; sleep 30 & descendant=$!; printf '%s\\n' \"$descendant\" > \"$descendant_file\"; emitted=0; while IFS= read -r frame; do printf '%s\\n' \"$frame\" >> \"$record\"; if [ \"$emitted\" -eq 0 ]; then if [ \"$#\" -gt 0 ]; then printf '%s\\n' \"$@\"; fi; emitted=1; fi; done; wait \"$descendant\"",
                "fixture",
            ])
            .arg(record_path)
            .arg(descendant_pid_path)
            .args(lines)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        command.process_group(0);
        let child = command
            .spawn()
            .context("start recording sandbox protocol fixture")?;
        Ok(Self::from_child(child, None))
    }

    /// Nimmt den stdin-Handle fuer stream-json Kommunikation (einmalig).
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.protocol_stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Sends one bounded JSONL protocol frame to the sandboxed child.
    pub fn send_protocol_line(&mut self, line: &str) -> Result<()> {
        write_protocol_line(&self.protocol_stdin, line)
    }

    pub(crate) fn protocol_channel_available(&self) -> bool {
        self.protocol_stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
            && self.protocol_stdout.is_some()
    }

    #[cfg(test)]
    pub(crate) fn protocol_queue_overflowed(&self) -> bool {
        self.protocol_queue_overflowed.load(Ordering::Acquire)
    }

    pub(crate) fn start_protocol_supervision(
        &mut self,
        invocation_id: &str,
        deadline_unix_ms: u64,
        execute_line: &str,
    ) -> std::result::Result<(), ProtocolSupervisionFailure> {
        if self.protocol_supervisor.is_some() {
            return Err(ProtocolSupervisionFailure::ProtocolViolation);
        }
        {
            let mut state = self.supervision.lock();
            if let ProtocolOutcome::FailurePending(failure)
            | ProtocolOutcome::FinalFailure(failure) = state.outcome
            {
                return Err(failure);
            }
            if state.reader_closed
                || !matches!(state.outcome, ProtocolOutcome::Running)
                || state.execute_sent
            {
                return Err(ProtocolSupervisionFailure::ChannelDisconnected);
            }
            state.reader.invocation_id = Some(invocation_id.to_string());
            state.deadline_unix_ms = deadline_unix_ms;
            if write_protocol_line(&self.protocol_stdin, execute_line).is_err() {
                state.outcome = ProtocolOutcome::FailurePending(
                    ProtocolSupervisionFailure::ChannelDisconnected,
                );
                return Err(ProtocolSupervisionFailure::ChannelDisconnected);
            }
            // This release is the sole authority for child output and deadline
            // actions. The initial execute record is fully written while the
            // same state lock excludes the reader, and the deadline supervisor
            // is created only after this publication.
            state.execute_sent = true;
        }

        let pid = self.pid;
        let child = Arc::clone(&self.child);
        let stdin = Arc::clone(&self.protocol_stdin);
        let supervision = Arc::clone(&self.supervision);
        let termination = Arc::clone(&self.termination);
        let supervised_cgroup = self.supervised_cgroup.clone();
        let invocation_id = invocation_id.to_string();
        self.protocol_supervisor = Some(std::thread::spawn(move || loop {
            let now_ms = unix_time_ms();
            let now = std::time::Instant::now();
            let mut send_deadline_cancel = false;
            let needs_quiescence;
            {
                let mut state = supervision.lock();
                if state.stop {
                    return;
                }
                if matches!(state.outcome, ProtocolOutcome::Running)
                    && state.cancel.is_none()
                    && state.deadline_unix_ms != 0
                    && now_ms >= state.deadline_unix_ms
                {
                    state.cancel = Some(ProtocolCancelClaim {
                        owner: ProtocolCancelOwner::Deadline,
                        claimed_at: now,
                        requested_at_unix_ms: now_ms.max(1),
                        send_started: false,
                        #[cfg(test)]
                        sent: false,
                    });
                }
                if let Some(claim) = state.cancel.as_mut() {
                    if claim.owner == ProtocolCancelOwner::Deadline && !claim.send_started {
                        claim.send_started = true;
                        send_deadline_cancel = true;
                    }
                }
                if matches!(state.outcome, ProtocolOutcome::Running) {
                    expire_protocol_cancel_if_due(&mut state, now);
                }
                needs_quiescence = match state.outcome {
                    ProtocolOutcome::FailurePending(_) => true,
                    ProtocolOutcome::ProcessExitedPending { observed_at }
                    | ProtocolOutcome::TerminalPending { observed_at } => {
                        now.saturating_duration_since(observed_at)
                            >= std::time::Duration::from_millis(PROTOCOL_TERMINAL_SETTLE_MS)
                    }
                    _ => false,
                };
                if matches!(
                    state.outcome,
                    ProtocolOutcome::FinalFailure(_) | ProtocolOutcome::FinalTerminal
                ) {
                    return;
                }
            }

            if send_deadline_cancel {
                #[cfg(test)]
                supervision.wait_at_deadline_send_barrier();
                let cancel = serde_json::json!({
                    "kind": "cancel",
                    "schema_version": 1,
                    "invocation_id": invocation_id.as_str(),
                    "reason": "deadline_expired"
                })
                .to_string();
                let sent = write_protocol_line(&stdin, &cancel).is_ok();
                let mut state = supervision.lock();
                #[cfg(test)]
                if let Some(claim) = state.cancel.as_mut() {
                    if claim.owner == ProtocolCancelOwner::Deadline {
                        claim.sent = sent;
                    }
                }
                if !sent && matches!(state.outcome, ProtocolOutcome::Running) {
                    state.outcome = ProtocolOutcome::FailurePending(
                        ProtocolSupervisionFailure::ChannelDisconnected,
                    );
                }
                drop(state);
                continue;
            }

            if needs_quiescence {
                #[cfg(test)]
                supervision.wait_at_pre_quiescence_barrier();
                let process_reaped =
                    terminate_owned_process_once(pid, &child, &stdin, &termination).is_ok();
                let cgroup_quiesced = match supervised_cgroup.as_deref() {
                    Some(cgroup) => cleanup_cgroup_after_process_exit(cgroup).is_ok(),
                    None => true,
                };
                let mut state = supervision.lock();
                state.cgroup_quiesced |= cgroup_quiesced;
                if process_reaped && state.reader_closed && state.cgroup_quiesced {
                    state.outcome = match state.outcome {
                        ProtocolOutcome::FailurePending(failure) => {
                            ProtocolOutcome::FinalFailure(failure)
                        }
                        ProtocolOutcome::TerminalPending { .. } => ProtocolOutcome::FinalTerminal,
                        outcome => outcome,
                    };
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }));
        Ok(())
    }

    /// Records the first explicit cancellation request. A deadline that won the
    /// race remains the single cancellation owner and will not emit a duplicate.
    pub(crate) fn begin_explicit_protocol_cancel(
        &self,
        requested_at_ms: u64,
    ) -> Option<ProtocolCancelOwner> {
        let mut state = self.supervision.lock();
        if let Some(claim) = state.cancel {
            return Some(claim.owner);
        }
        if !matches!(state.outcome, ProtocolOutcome::Running) {
            return None;
        }
        state.cancel = Some(ProtocolCancelClaim {
            owner: ProtocolCancelOwner::Explicit,
            claimed_at: std::time::Instant::now(),
            requested_at_unix_ms: requested_at_ms.max(1),
            send_started: true,
            #[cfg(test)]
            sent: false,
        });
        Some(ProtocolCancelOwner::Explicit)
    }

    #[cfg(test)]
    pub(crate) fn mark_protocol_cancel_sent(&self) {
        if let Some(claim) = self.supervision.lock().cancel.as_mut() {
            claim.sent = true;
        }
    }

    pub(crate) fn mark_protocol_channel_disconnected(&self) {
        let mut state = self.supervision.lock();
        if matches!(state.outcome, ProtocolOutcome::Running) {
            state.outcome =
                ProtocolOutcome::FailurePending(ProtocolSupervisionFailure::ChannelDisconnected);
        }
    }

    fn set_supervised_cgroup(&mut self, cgroup: &str) {
        self.supervised_cgroup = Some(cgroup.to_string());
    }

    pub(crate) fn protocol_supervision_snapshot(&self) -> ProtocolSupervisionSnapshot {
        let stdin_closed = self
            .protocol_stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none();
        self.supervision.snapshot(
            self.termination.reaped.load(Ordering::Acquire),
            stdin_closed,
        )
    }

    #[cfg(test)]
    pub(crate) fn protocol_start_failure(&self) -> Option<ProtocolSupervisionFailure> {
        let state = self.supervision.lock();
        match state.outcome {
            ProtocolOutcome::FailurePending(failure) | ProtocolOutcome::FinalFailure(failure) => {
                Some(failure)
            }
            _ if state.reader_closed && !state.execute_sent => {
                Some(ProtocolSupervisionFailure::ChannelDisconnected)
            }
            _ => None,
        }
    }

    pub(crate) fn owned_process_reaped(&self) -> bool {
        self.termination.reaped.load(Ordering::Acquire)
    }

    pub(crate) fn workbench_isolation_attestation(&self) -> Option<WorkbenchIsolationAttestation> {
        self.workbench_isolation
    }

    #[cfg(test)]
    pub(crate) fn install_workbench_isolation_attestation(
        &mut self,
        child_pid: u32,
        landlock_abi: u8,
    ) {
        self.child_pid = Some(child_pid);
        self.workbench_isolation = Some(WorkbenchIsolationAttestation {
            child_pid,
            landlock_abi,
        });
    }

    #[cfg(test)]
    pub(crate) fn install_deadline_send_barrier(
        &self,
        claimed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        self.supervision
            .install_deadline_send_barrier(claimed, release);
    }

    #[cfg(test)]
    pub(crate) fn install_post_terminal_barrier(
        &self,
        observed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        self.supervision
            .install_post_terminal_barrier(observed, release);
    }

    #[cfg(test)]
    pub(crate) fn install_pre_quiescence_barrier(
        &self,
        observed: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    ) {
        self.supervision
            .install_pre_quiescence_barrier(observed, release);
    }

    #[cfg(test)]
    pub(crate) fn retry_supervised_cgroup_cleanup_with<List, Kill, Remove>(
        &self,
        name: &str,
        list_pids: List,
        kill_pids: Kill,
        remove: Remove,
    ) -> Result<()>
    where
        List: Fn(&str) -> Result<Vec<u32>>,
        Kill: Fn(&str) -> Result<usize>,
        Remove: Fn(&str) -> Result<()>,
    {
        if !self.termination.reaped.load(Ordering::Acquire) {
            bail!("sandbox supervisor must be reaped before cgroup-only retry");
        }
        cleanup_cgroup_after_process_exit_with(name, list_pids, kill_pids, remove)?;
        self.supervision.lock().cgroup_quiesced = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn termination_signal_attempts(&self) -> usize {
        self.termination.signal_attempts.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn termination_signal_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.termination.signal_attempts)
    }

    #[cfg(test)]
    pub(crate) fn reap_publications(&self) -> usize {
        self.termination.reap_publications.load(Ordering::Acquire)
    }

    /// Drains complete JSONL frames already emitted by the child. Reading is
    /// performed by a dedicated thread so registry polling never blocks.
    pub(crate) fn drain_protocol_lines(&mut self) -> Result<ProtocolDrain> {
        let receiver = self
            .protocol_stdout
            .as_ref()
            .context("sandbox protocol stdout is unavailable")?;
        let mut lines = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(ProtocolFrame::Line(line)) => lines.push(line),
                Ok(ProtocolFrame::Rejected) => {
                    anyhow::bail!("sandbox protocol stdout emitted an invalid or oversized frame")
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        Ok(ProtocolDrain {
            lines,
            queue_overflowed: self.protocol_queue_overflowed.load(Ordering::Acquire),
        })
    }

    /// Prueft ob der Prozess noch laeuft.
    pub fn is_running(&mut self) -> bool {
        if self.termination.reaped.load(Ordering::Acquire) {
            return false;
        }
        if self
            .termination
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Another exact-child observer or terminator owns the transition.
            // Health is conservative while that owner publishes its result.
            return false;
        }
        let observed = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_wait();
        match observed {
            Ok(None) => {
                self.termination.started.store(false, Ordering::Release);
                true
            }
            Ok(Some(_)) => {
                // `try_wait` reaped the exact owned Child. Publish the permanent
                // no-more-numeric-signal state before another cleanup owner can
                // proceed, and close input so the reader/supervisor can finish.
                self.protocol_stdin
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let mut supervision = self.supervision.lock();
                if matches!(supervision.outcome, ProtocolOutcome::Running) {
                    supervision.outcome = ProtocolOutcome::ProcessExitedPending {
                        observed_at: std::time::Instant::now(),
                    };
                }
                drop(supervision);
                #[cfg(test)]
                self.termination
                    .reap_publications
                    .fetch_add(1, Ordering::AcqRel);
                self.termination.reaped.store(true, Ordering::Release);
                false
            }
            Err(_) => {
                self.termination.started.store(false, Ordering::Release);
                false
            }
        }
    }

    /// Terminates and reaps the child process owned by this handle.
    pub fn terminate(&mut self) {
        let _ = self.terminate_process_group();
        self.join_protocol_reader();
    }

    /// Terminates and reaps the child, surfacing incomplete cleanup so the
    /// NanoRuntime can retain ownership and retry instead of forgetting a live
    /// sandbox process. The protocol reader is joined separately after cgroup
    /// cleanup, because a descendant outside the process group may still hold
    /// stdout while remaining inside the adapter-owned cgroup.
    pub fn terminate_checked(&mut self) -> Result<()> {
        self.terminate_process_group()
    }

    pub(crate) fn terminate_process_group(&mut self) -> Result<()> {
        self.protocol_stdout.take();
        terminate_owned_process_once(
            self.pid,
            &self.child,
            &self.protocol_stdin,
            &self.termination,
        )
    }

    pub(crate) fn join_protocol_reader(&mut self) {
        self.supervision.lock().stop = true;
        if let Some(reader) = self.protocol_reader.take() {
            let _ = reader.join();
        }
        if let Some(supervisor) = self.protocol_supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

impl From<SpawnedSandbox> for AgentProcess {
    fn from(spawned: SpawnedSandbox) -> Self {
        Self::from_child(spawned.child, spawned.child_pid)
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        // The runtime's normal teardown joins the reader after cgroup cleanup.
        // Drop still closes pipes and kills/reaps the owned process group, but
        // does not risk blocking forever on a descendant that escaped that
        // group while remaining in the production cgroup.
        let _ = self.terminate_process_group();
    }
}

fn write_protocol_line(protocol_stdin: &Arc<Mutex<Option<ChildStdin>>>, line: &str) -> Result<()> {
    if line.len() > PROTOCOL_LINE_LIMIT_BYTES {
        bail!("sandbox protocol input exceeded its configured bound");
    }
    if line
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bail!("sandbox protocol input must contain exactly one JSONL record");
    }
    let mut protocol_stdin = protocol_stdin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stdin = protocol_stdin
        .as_mut()
        .context("sandbox protocol stdin is unavailable")?;
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(PROTOCOL_WRITE_TIMEOUT_MS);
    write_nonblocking_until(stdin, line.as_bytes(), deadline)
        .and_then(|_| write_nonblocking_until(stdin, b"\n", deadline))
        .context("write sandbox protocol frame")
}

fn write_nonblocking_until(
    output: &mut ChildStdin,
    mut bytes: &[u8],
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match output.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "sandbox protocol pipe accepted no data",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "sandbox protocol pipe remained backpressured",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn terminate_owned_process_once(
    pid: u32,
    child: &Arc<Mutex<Child>>,
    protocol_stdin: &Arc<Mutex<Option<ChildStdin>>>,
    termination: &Arc<ProcessTermination>,
) -> Result<()> {
    let mut owns_transition = false;
    for _ in 0..=40 {
        if termination.reaped.load(Ordering::Acquire) {
            return Ok(());
        }
        if termination
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            owns_transition = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    if !owns_transition {
        bail!("sandbox process termination remained in progress");
    }

    termination.signal_attempts.fetch_add(1, Ordering::AcqRel);
    protocol_stdin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let signal_group = (|| -> Result<()> {
        let group = i32::try_from(pid).context("sandbox supervisor PID exceeds pid_t")?;
        // The first owner signals the process group exactly once. Once the
        // supervisor is reaped, retries must never address this numeric PID.
        match nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(group),
            nix::sys::signal::Signal::SIGKILL,
        ) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(error) => Err(error).context("kill owned sandbox process group"),
        }
    })();
    if let Err(error) = signal_group {
        termination.started.store(false, Ordering::Release);
        return Err(error);
    }
    let supervisor_reap = (|| -> Result<()> {
        let mut child = child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if child
            .try_wait()
            .context("query sandbox supervisor")?
            .is_none()
        {
            child
                .kill()
                .context("kill exact owned sandbox supervisor")?;
            child
                .wait()
                .context("reap exact owned sandbox supervisor")?;
        }
        Ok(())
    })();
    if let Err(error) = supervisor_reap {
        termination.started.store(false, Ordering::Release);
        return Err(error);
    }

    // Publish the permanent no-more-signal state before any later cleanup can
    // fail. A retry may validate remaining descendants/cgroups, but it cannot
    // signal a potentially reused supervisor process-group id.
    #[cfg(test)]
    termination.reap_publications.fetch_add(1, Ordering::AcqRel);
    termination.reaped.store(true, Ordering::Release);
    Ok(())
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn pid_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn wait_for_pid_exit(pid: u32) {
    for _ in 0..20 {
        if !pid_exists(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn cleanup_cgroup_after_process_exit(name: &str) -> Result<()> {
    cleanup_cgroup_after_process_exit_with(
        name,
        cgroups::list_pids_in_cgroup,
        cgroups::kill_cgroup_processes,
        cgroups::remove_cgroup,
    )
}

fn cleanup_cgroup_after_process_exit_with<List, Kill, Remove>(
    name: &str,
    list_pids: List,
    kill_pids: Kill,
    remove: Remove,
) -> Result<()>
where
    List: Fn(&str) -> Result<Vec<u32>>,
    Kill: Fn(&str) -> Result<usize>,
    Remove: Fn(&str) -> Result<()>,
{
    match list_pids(name) {
        Ok(pids) if pids.is_empty() => remove(name),
        Ok(pids) => {
            debug!(
                cgroup = %name,
                pid_count = pids.len(),
                "cgroup vor Entfernen noch belegt, beende Mitglieder"
            );
            kill_pids(name)?;
            remove(name)
        }
        Err(error) => {
            warn!(
                cgroup = %name,
                error = %error,
                "cgroup-Mitglieder konnten vor Remove nicht gelesen werden"
            );
            remove(name)
        }
    }
}

impl std::fmt::Debug for AgentProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentProcess")
            .field("pid", &self.pid)
            .finish()
    }
}

/// Warnings about degraded sandbox capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxWarning {
    /// Landlock LSM not available on this kernel.
    LandlockNotAvailable,
    /// A cgroup controller is not delegated to the user.
    CgroupNotDelegated(String),
    /// bwrap can't create user namespaces (AppArmor blocks it).
    BwrapUsernsDenied,
    /// IO controller not delegated — io.max limits cannot be enforced.
    IoNotDelegated,
    /// Failed to set OOM score for ECS core process.
    OomScoreFailed(String),
}

/// Result of verifying that an agent runs in its own network namespace (#75).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationStatus {
    /// Agent netns differs from the daemon's — full cage in effect.
    Isolated,
    /// Agent shares the daemon's netns (same inode) — NOT caged. Act on this.
    NotIsolated,
    /// Namespace inode could not be read (transient). MUST NOT be treated as a
    /// cage breach — the bwrap exit code is the primary fail-closed signal.
    ProbeError,
}

/// Handle returned by setup_agent() — tracks what was created.
#[derive(Debug, Clone)]
pub struct SandboxHandle {
    pub agent_name: String,
    pub cgroup_created: bool,
    /// Captured at setup so eBPF deregistration remains possible after the
    /// adapter has removed the cgroup directory.
    pub cgroup_id: Option<u64>,
    pub io_available: bool,
    pub bwrap_pid: Option<u32>,
    pub landlock_applied: bool,
    /// Whether the post-spawn netns verification confirmed isolation (#75).
    pub network_isolated: bool,
}

/// Central sandbox enforcement facade.
///
/// Bundles Landlock + cgroups v2 + bwrap into a single interface.
/// Created via `detect()` which probes kernel capabilities.
pub struct SandboxEnforcer {
    /// Detected Landlock ABI version (None = not available).
    landlock_abi: Option<u8>,
    /// cgroup v2 root for sentinel agents.
    cgroup_root: PathBuf,
    /// Whether cgroup root is writable by current user.
    cgroup_available: bool,
    /// Whether bwrap with user namespaces works.
    bwrap_available: bool,
    /// Whether OOM score has been set for ECS core.
    oom_set: AtomicBool,
    /// Optional sentinel-fs FUSE mount path.
    /// When set, bwrap binds `{fs_mount}/{name}` instead of `/ram/agents/{name}`.
    fs_mount: Option<String>,
}

impl std::fmt::Debug for SandboxEnforcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxEnforcer")
            .field("landlock_abi", &self.landlock_abi)
            .field("cgroup_root", &self.cgroup_root)
            .field("cgroup_available", &self.cgroup_available)
            .field("bwrap_available", &self.bwrap_available)
            .finish()
    }
}

impl SandboxEnforcer {
    fn cgroup_root_writable(cgroup_root: &Path) -> bool {
        let probe = cgroup_root.join(format!(".sentinel-write-check-{}", std::process::id()));
        match std::fs::create_dir(&probe) {
            Ok(()) => {
                let _ = std::fs::remove_dir(&probe);
                true
            }
            Err(_) => false,
        }
    }

    /// Detects available kernel sandbox features.
    ///
    /// Probes:
    /// - Landlock ABI version
    /// - cgroup v2 root writability
    /// - IO controller delegation
    /// - bwrap user namespace support
    /// - Sets OOM score -1000 for ECS core process (immortal)
    pub fn detect() -> (Self, Vec<SandboxWarning>) {
        let mut warnings = Vec::new();

        // 1. Landlock detection
        let landlock_abi = landlock::detect_abi();
        if let Some(abi) = landlock_abi {
            info!("Landlock ABI v{abi} detected");
        } else {
            warnings.push(SandboxWarning::LandlockNotAvailable);
        }

        // 2. cgroup root + controller delegation
        let cgroup_root = PathBuf::from("/sys/fs/cgroup/sentinel");
        let cgroup_available = if cgroup_root.exists() {
            if Self::cgroup_root_writable(&cgroup_root) {
                true
            } else {
                warnings.push(SandboxWarning::CgroupNotDelegated(format!(
                    "{} exists but is not writable by the current user",
                    cgroup_root.display()
                )));
                false
            }
        } else {
            match std::fs::create_dir_all(&cgroup_root) {
                Ok(_) => {
                    info!("Created cgroup root: {}", cgroup_root.display());
                    true
                }
                Err(e) => {
                    warnings.push(SandboxWarning::CgroupNotDelegated(format!(
                        "Cannot create {}: {e}",
                        cgroup_root.display()
                    )));
                    false
                }
            }
        };

        // 2b. Delegate controllers (cpu, memory, pids, io) from root cgroup to sentinel
        // This enables cpu.max, memory.max, etc. in agent child cgroups.
        if cgroup_available {
            // First enable controllers at /sys/fs/cgroup level (root → sentinel)
            cgroups::delegate_controllers("/sys/fs/cgroup");
            // Then enable at sentinel level (sentinel → agent children)
            cgroups::delegate_controllers("/sys/fs/cgroup/sentinel");
        }

        // 3. IO controller check — verify IO is now available in sentinel subtree
        let sentinel_has_io =
            cgroup_available && cgroups::io_controller_enabled("/sys/fs/cgroup/sentinel");
        if !sentinel_has_io {
            warnings.push(SandboxWarning::IoNotDelegated);
        }

        // 4. bwrap userns check
        let bwrap_available = BwrapConfig::test_userns();
        if !bwrap_available {
            warnings.push(SandboxWarning::BwrapUsernsDenied);
        } else {
            info!("bwrap user namespace support confirmed");
        }

        // 5. OOM score for ECS core (-1000 = immortal)
        let oom_set = match cgroups::set_oom_score(std::process::id(), -1000) {
            Ok(_) => {
                info!("ECS core OOM score set to -1000 (immortal)");
                AtomicBool::new(true)
            }
            Err(e) => {
                warnings.push(SandboxWarning::OomScoreFailed(e.to_string()));
                AtomicBool::new(false)
            }
        };

        // #75: no CAP_NET_ADMIN / bridge/veth detection — agents are full-caged
        // by bwrap --unshare-all (needs user namespaces, checked above). The
        // daemon verifies isolation post-spawn on the sandboxed child PID.

        let enforcer = Self {
            landlock_abi,
            cgroup_root,
            cgroup_available,
            bwrap_available,
            oom_set,
            fs_mount: None,
        };

        (enforcer, warnings)
    }

    /// Sets the sentinel-fs FUSE mount path.
    ///
    /// When set, `start_agent_process()` binds `{fs_mount}/{host_agent_dir}` instead
    /// of the default `/ram/agents/{name}` as the agent's writable home.
    pub fn set_fs_mount(&mut self, path: String) {
        self.fs_mount = Some(path);
    }

    /// Creates sandbox resources for an agent (cgroup + home directory).
    ///
    /// Does NOT start a process. Call `start_agent_process()` to launch bwrap.
    /// Called by RuntimeOrchestrator::spawn_agent().
    pub fn setup_agent(&self, name: &str, limits: &CgroupLimits) -> Result<SandboxHandle> {
        let mut handle = SandboxHandle {
            agent_name: name.to_string(),
            cgroup_created: false,
            cgroup_id: None,
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
            network_isolated: false,
        };

        // 1. Create cgroup with resource limits
        if self.cgroup_available {
            let setup = cgroups::create_cgroup(name, limits)
                .with_context(|| format!("Failed to create cgroup for agent {name}"))?;
            handle.cgroup_created = true;
            handle.cgroup_id = cgroups::cgroup_id(name);
            handle.io_available = setup.io_available;
        } else {
            warn!("Skipping cgroup creation for {name} (cgroup root not available)");
        }

        // 2. Create agent home directory (sentinel-fs Integrationspunkt)
        let home = format!("/ram/agents/{name}");
        if let Err(e) = std::fs::create_dir_all(&home) {
            warn!("Failed to create agent home {home}: {e}");
            // Non-fatal: might be on a system without /ram/agents
        }

        Ok(handle)
    }

    /// Starts a bwrap process for the agent.
    ///
    /// The bwrap process runs in isolated namespaces with Landlock FS restrictions.
    /// If Landlock is available, a wrapper binary is injected between bwrap and the
    /// agent command that applies irreversible Landlock rules before exec.
    /// Returns an [`AgentProcess`] with PID and Child handle.
    /// The Child's stdin is piped for stream-json communication.
    pub fn start_agent_process(
        &self,
        name: &str,
        fs_host_agent_dir: Option<&str>,
        command: &[String],
    ) -> Result<AgentProcess> {
        if !self.bwrap_available {
            anyhow::bail!("bwrap not available — cannot start agent process");
        }

        let mut config = BwrapConfig::for_agent(name);

        // sentinel-fs FUSE mount: replace /ram/agents/ with FUSE mount path
        if let Some(ref fs_mount) = self.fs_mount {
            config = config.with_fs_mount(fs_mount, fs_host_agent_dir.unwrap_or(name), name);
        }

        self.start_process_with_config(name, config, command, false)
    }

    /// Starts the persistent agent-runtime used by the M0 workbench protocol.
    /// Its only writable roots are the exact agent-owned workspace and artifact
    /// directories. Unlike the general agent sandbox, missing binds are fatal.
    pub fn start_workbench_process(
        &self,
        name: &str,
        _fs_host_agent_dir: Option<&str>,
        command: &[String],
    ) -> Result<AgentProcess> {
        if !self.bwrap_available {
            anyhow::bail!("bwrap not available — cannot start workbench process");
        }
        // sentinel-fs is the normal agent-home view, but its POSIX surface does
        // not own mutable workbench directories. Keep those roots on the
        // persistent, agent-private host backing that setup_agent() creates.
        let host_agent_root = workbench_host_agent_root(name);
        prepare_workbench_roots(&host_agent_root)?;
        let config = BwrapConfig::for_agent(name)
            .for_workbench()
            .with_workbench_roots(&host_agent_root);
        self.start_process_with_config(name, config, command, true)
    }

    fn start_process_with_config(
        &self,
        name: &str,
        mut config: BwrapConfig,
        command: &[String],
        require_workbench_attestation: bool,
    ) -> Result<AgentProcess> {
        // #75: full cage is unconditional — BwrapConfig::for_agent already sets
        // share_net=false (no --share-net). The daemon verifies isolation
        // post-spawn on the sandboxed child PID.

        let attestation_nonce =
            require_workbench_attestation.then(|| uuid::Uuid::new_v4().to_string());
        let landlock_abi = if require_workbench_attestation {
            Some(
                self.landlock_abi
                    .context("workbench requires Landlock enforcement")?,
            )
        } else {
            self.landlock_abi
        };
        let wrapper_path = landlock_wrapper_path();
        let wrapper_is_regular = std::fs::symlink_metadata(&wrapper_path)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        if require_workbench_attestation {
            validate_workbench_static_prerequisites(
                landlock_abi,
                wrapper_is_regular,
                self.cgroup_available,
            )?;
        }

        // Wrap command with Landlock enforcement if available. Workbench mode
        // requires the wrapper and a post-exec attestation; the general agent
        // path retains its existing best-effort defense-in-depth behavior.
        let wrapped_command = if let Some(abi) = landlock_abi {
            // Bind the wrapper binary into the namespace
            if wrapper_is_regular {
                config.readonly_binds.push((
                    wrapper_path.to_string_lossy().into_owned(),
                    "/landlock-wrapper".to_string(),
                ));
                let mut cmd = vec!["/landlock-wrapper".to_string()];
                if let Some(nonce) = attestation_nonce.as_deref() {
                    cmd.extend([
                        "--attest-v1".to_string(),
                        nonce.to_string(),
                        abi.to_string(),
                    ]);
                }
                cmd.extend([name.to_string(), "--".to_string()]);
                cmd.extend_from_slice(command);
                info!("Landlock wrapper injected for agent {name}");
                cmd
            } else if require_workbench_attestation {
                bail!("workbench Landlock wrapper is unavailable");
            } else {
                warn!(
                    "landlock-wrapper binary not found at {}, skipping Landlock",
                    wrapper_path.display()
                );
                command.to_vec()
            }
        } else {
            command.to_vec()
        };

        let mut process = AgentProcess::from(config.spawn(&wrapped_command)?);
        let pid = process.pid;
        let child_pid = process.child_pid;

        // Add bwrap process to agent's cgroup (supervisor PID — children inherit
        // the cgroup; this is correct for cgroups, unlike netns which needs the
        // sandboxed child PID).
        if self.cgroup_available {
            process.set_supervised_cgroup(name);
            if let Err(error) = cgroups::add_pid_to_cgroup(name, pid) {
                if require_workbench_attestation {
                    return Err(error).context("attach workbench supervisor to cgroup");
                }
                warn!("Failed to add bwrap PID {pid} to cgroup {name}: {error}");
            }
        } else if require_workbench_attestation {
            bail!("workbench requires an active cgroup boundary");
        }

        if require_workbench_attestation {
            let child_pid = child_pid.context("workbench sandbox did not report its child PID")?;
            cgroups::add_pid_to_cgroup(name, child_pid)
                .context("attach exact workbench child to cgroup")?;
            let cgroup_members = cgroups::list_pids_in_cgroup(name)
                .context("verify exact workbench child cgroup membership")?;
            validate_workbench_child_boundary(
                Some(child_pid),
                cgroup_members.contains(&child_pid),
                self.verify_agent_netns_isolation(child_pid),
            )?;
            let nonce = attestation_nonce
                .as_deref()
                .context("workbench attestation nonce is unavailable")?;
            let abi = landlock_abi.context("workbench Landlock ABI is unavailable")?;
            await_workbench_startup_attestation(child_pid, nonce, abi)?;
            process.workbench_isolation = Some(WorkbenchIsolationAttestation {
                child_pid,
                landlock_abi: abi,
            });
        }

        debug!(
            name,
            pid, child_pid, "bwrap process started, returning AgentProcess handle"
        );

        Ok(process)
    }

    /// Verifies that the sandboxed agent process runs in its own network
    /// namespace (#75 full cage), comparing `/proc/<child_pid>/ns/net` to the
    /// daemon's `/proc/self/ns/net`.
    ///
    /// `child_pid` MUST be the sandboxed `agent-runtime` PID (from bwrap
    /// `--info-fd`), NOT the bwrap supervisor PID — the supervisor stays in the
    /// root netns by design, so verifying it would falsely report every agent
    /// as un-caged. A transient read failure returns [`IsolationStatus::ProbeError`]
    /// and MUST NOT be treated as a cage breach; the bwrap exit code is the
    /// primary fail-closed signal.
    pub fn verify_agent_netns_isolation(&self, child_pid: u32) -> IsolationStatus {
        let daemon_ns = read_netns_inode("/proc/self/ns/net");
        let agent_ns = read_netns_inode(&format!("/proc/{child_pid}/ns/net"));
        classify_isolation(daemon_ns, agent_ns)
    }

    /// Tears down sandbox resources for an agent.
    ///
    /// Kills the bwrap process (if running) and removes the cgroup. The agent's
    /// network namespace is anonymous (bwrap --unshare-all) and is torn down by
    /// the kernel when the sandboxed process exits — no explicit netns cleanup
    /// (#75).
    /// Called by RuntimeOrchestrator::despawn_agent().
    pub fn teardown_agent(&self, handle: &SandboxHandle) -> Result<()> {
        // Ask bwrap to exit first; remaining cgroup members are handled below.
        if let Some(pid) = handle.bwrap_pid {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            wait_for_pid_exit(pid);
        }

        if handle.cgroup_created {
            cleanup_cgroup_after_process_exit(&handle.agent_name)
                .with_context(|| format!("remove sandbox cgroup for {}", handle.agent_name))?;
        }

        Ok(())
    }

    /// Reconcile setup that failed before a [`SandboxHandle`] could be
    /// returned. This is intentionally limited to the deterministic cgroup
    /// path; workload home ownership remains guarded by the NanoRuntime marker.
    pub fn recover_partial_agent_setup(&self, agent_name: &str) -> Result<()> {
        if std::path::Path::new(&cgroups::cgroup_path(agent_name)).exists() {
            cleanup_cgroup_after_process_exit(agent_name)
                .with_context(|| format!("recover partial sandbox setup for {agent_name}"))?;
        }
        Ok(())
    }

    /// Reads PSI metrics for an agent's cgroup.
    ///
    /// Used for Zenoh publish -> Bio-Engine pipeline.
    /// resource: "cpu" or "memory"
    pub fn read_agent_psi(&self, name: &str, resource: &str) -> Result<PsiMetrics> {
        cgroups::read_psi_from_cgroup(name, resource)
    }

    /// Whether Landlock is available on this system.
    pub fn has_landlock(&self) -> bool {
        self.landlock_abi.is_some()
    }

    /// Detected Landlock ABI version.
    pub fn landlock_abi(&self) -> Option<u8> {
        self.landlock_abi
    }

    /// Whether cgroup v2 is available for agent isolation.
    pub fn has_cgroups(&self) -> bool {
        self.cgroup_available
    }

    /// Whether bwrap with user namespaces is available.
    pub fn has_bwrap(&self) -> bool {
        self.bwrap_available
    }

    /// Whether OOM score was set for the ECS core process.
    pub fn oom_score_set(&self) -> bool {
        self.oom_set.load(Ordering::Relaxed)
    }
}

fn validate_workbench_static_prerequisites(
    landlock_abi: Option<u8>,
    wrapper_is_regular: bool,
    cgroup_available: bool,
) -> Result<()> {
    ensure!(
        landlock_abi.is_some_and(|abi| abi > 0),
        "workbench requires Landlock enforcement"
    );
    ensure!(
        wrapper_is_regular,
        "workbench Landlock wrapper is unavailable"
    );
    ensure!(
        cgroup_available,
        "workbench requires an active cgroup boundary"
    );
    Ok(())
}

fn validate_workbench_child_boundary(
    child_pid: Option<u32>,
    exact_child_in_cgroup: bool,
    network_isolation: IsolationStatus,
) -> Result<u32> {
    let child_pid = child_pid.context("workbench sandbox did not report its child PID")?;
    ensure!(
        exact_child_in_cgroup,
        "workbench child cgroup membership was not observed"
    );
    ensure!(
        network_isolation == IsolationStatus::Isolated,
        "workbench network namespace isolation was not attested"
    );
    Ok(child_pid)
}

fn protocol_reader_parts(
    stdout: Option<std::process::ChildStdout>,
    supervision: Arc<ProtocolSupervision>,
) -> (
    Option<Receiver<ProtocolFrame>>,
    Arc<AtomicBool>,
    Option<std::thread::JoinHandle<()>>,
) {
    let queue_overflowed = Arc::new(AtomicBool::new(false));
    match stdout {
        Some(stdout) => {
            let (receiver, reader) =
                protocol_line_receiver(stdout, Arc::clone(&queue_overflowed), supervision);
            (Some(receiver), queue_overflowed, Some(reader))
        }
        None => (None, queue_overflowed, None),
    }
}

fn protocol_line_receiver(
    stdout: std::process::ChildStdout,
    queue_overflowed: Arc<AtomicBool>,
    supervision: Arc<ProtocolSupervision>,
) -> (Receiver<ProtocolFrame>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::sync_channel(PROTOCOL_QUEUE_DEPTH);
    let reader = std::thread::spawn(move || {
        let _closed = ProtocolReaderCloseGuard(Arc::clone(&supervision));
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            let mut reached_eof = false;
            loop {
                let available = match reader.fill_buf() {
                    Ok(available) => available,
                    Err(_) => {
                        record_protocol_failure(
                            &supervision,
                            ProtocolSupervisionFailure::ChannelDisconnected,
                        );
                        return;
                    }
                };
                if available.is_empty() {
                    reached_eof = true;
                    break;
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |index| index + 1);
                let payload = if newline.is_some() {
                    &available[..consumed - 1]
                } else {
                    &available[..consumed]
                };
                if bytes.len().saturating_add(payload.len()) > PROTOCOL_LINE_LIMIT_BYTES {
                    record_protocol_failure(
                        &supervision,
                        ProtocolSupervisionFailure::OutputLimitExceeded,
                    );
                    let _ = sender.try_send(ProtocolFrame::Rejected);
                    return;
                }
                bytes.extend_from_slice(payload);
                reader.consume(consumed);
                if newline.is_some() {
                    break;
                }
            }
            if reached_eof && bytes.is_empty() {
                break;
            }
            if reached_eof {
                // JSONL records are newline terminated. A syntactically valid
                // JSON value at EOF is still an incomplete protocol record.
                record_protocol_failure(&supervision, ProtocolSupervisionFailure::InvalidFrame);
                let _ = sender.try_send(ProtocolFrame::Rejected);
                return;
            }
            let line = match String::from_utf8(bytes) {
                Ok(line) => line,
                Err(_) => {
                    record_protocol_failure(&supervision, ProtocolSupervisionFailure::InvalidFrame);
                    let _ = sender.try_send(ProtocolFrame::Rejected);
                    return;
                }
            };
            let (next_reader_state, terminal) =
                match validate_protocol_output_line(&supervision, &line) {
                    Ok(validated) => validated,
                    Err(failure) => {
                        record_protocol_failure(&supervision, failure);
                        let _ = sender.try_send(ProtocolFrame::Rejected);
                        return;
                    }
                };
            let mut state = supervision.lock();
            let committed = commit_protocol_reader_state(
                &mut state,
                next_reader_state,
                terminal,
                std::time::Instant::now(),
            );
            if !committed {
                return;
            }
            match sender.try_send(ProtocolFrame::Line(line)) {
                Ok(()) => {
                    drop(state);
                    if terminal {
                        #[cfg(test)]
                        supervision.wait_at_post_terminal_barrier();
                    }
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    queue_overflowed.store(true, Ordering::Release);
                    state.outcome = ProtocolOutcome::FailurePending(
                        ProtocolSupervisionFailure::OutputLimitExceeded,
                    );
                    return;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    state.outcome = ProtocolOutcome::FailurePending(
                        ProtocolSupervisionFailure::ChannelDisconnected,
                    );
                    return;
                }
            }
        }
    });
    (receiver, reader)
}

fn validate_protocol_output_line(
    supervision: &ProtocolSupervision,
    line: &str,
) -> std::result::Result<(ProtocolReaderState, bool), ProtocolSupervisionFailure> {
    let state = supervision.lock();
    if !state.execute_sent {
        // Before the initial execute write is fully published, any child output
        // is an invalid pre-execute record. Do not parse or retain it.
        return Err(ProtocolSupervisionFailure::InvalidFrame);
    }
    let mut next = state.reader.clone();
    if next.terminal {
        return Err(ProtocolSupervisionFailure::ProtocolViolation);
    }
    next.retained_bytes = next.retained_bytes.saturating_add(line.len());
    if next.retained_bytes > PROTOCOL_OUTPUT_LIMIT_BYTES {
        return Err(ProtocolSupervisionFailure::OutputLimitExceeded);
    }
    let message: serde_json::Value =
        serde_json::from_str(line).map_err(|_| ProtocolSupervisionFailure::InvalidFrame)?;
    if message
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(ProtocolSupervisionFailure::UnsupportedVersion);
    }
    let expected_invocation = next
        .invocation_id
        .as_deref()
        .ok_or(ProtocolSupervisionFailure::ProtocolViolation)?;
    if message
        .get("invocation_id")
        .and_then(serde_json::Value::as_str)
        != Some(expected_invocation)
    {
        return Err(ProtocolSupervisionFailure::InvocationConflict);
    }

    let terminal = match message.get("kind").and_then(serde_json::Value::as_str) {
        Some("result") if !next.result_seen => {
            next.result_seen = true;
            false
        }
        Some("progress")
            if message.get("stage").and_then(serde_json::Value::as_str) == Some("completed") =>
        {
            if !next.result_seen {
                return Err(ProtocolSupervisionFailure::ProtocolViolation);
            }
            next.terminal = true;
            true
        }
        Some("progress") if !next.result_seen => false,
        Some("error") if !next.result_seen => {
            next.terminal = true;
            true
        }
        Some("cancelled") if !next.result_seen && state.cancel.is_some() => {
            next.terminal = true;
            true
        }
        _ => return Err(ProtocolSupervisionFailure::ProtocolViolation),
    };
    Ok((next, terminal))
}

fn record_protocol_failure(supervision: &ProtocolSupervision, failure: ProtocolSupervisionFailure) {
    let mut state = supervision.lock();
    if matches!(
        state.outcome,
        ProtocolOutcome::Running
            | ProtocolOutcome::ProcessExitedPending { .. }
            | ProtocolOutcome::TerminalPending { .. }
    ) {
        state.outcome = ProtocolOutcome::FailurePending(failure);
    }
}

fn commit_protocol_reader_state(
    state: &mut ProtocolSupervisionState,
    reader: ProtocolReaderState,
    terminal: bool,
    observed_at: std::time::Instant,
) -> bool {
    if !matches!(
        state.outcome,
        ProtocolOutcome::Running | ProtocolOutcome::ProcessExitedPending { .. }
    ) {
        return false;
    }
    state.reader = reader;
    if terminal {
        state.outcome = ProtocolOutcome::TerminalPending { observed_at };
    }
    true
}

fn expire_protocol_cancel_if_due(
    state: &mut ProtocolSupervisionState,
    now: std::time::Instant,
) -> bool {
    let Some(claim) = state.cancel else {
        return false;
    };
    if !matches!(state.outcome, ProtocolOutcome::Running)
        || now.saturating_duration_since(claim.claimed_at)
            < std::time::Duration::from_millis(PROTOCOL_CANCEL_GRACE_MS)
    {
        return false;
    }
    state.outcome = ProtocolOutcome::FailurePending(match claim.owner {
        ProtocolCancelOwner::Deadline => ProtocolSupervisionFailure::DeadlineExceeded,
        ProtocolCancelOwner::Explicit => ProtocolSupervisionFailure::Cancelled,
    });
    true
}

/// Returns the expected path for the landlock-wrapper binary.
///
/// Checks (in order): next to current executable, /opt/sentinel/bin/, /usr/local/bin/.
fn prepare_workbench_roots(host_agent_root: &Path) -> Result<()> {
    let parent = host_agent_root
        .parent()
        .ok_or_else(|| anyhow!("workbench agent root has no parent"))?;
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize workbench root parent {}", parent.display()))?;
    match std::fs::symlink_metadata(host_agent_root) {
        Ok(metadata) => anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "workbench agent root must be a real directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(host_agent_root).with_context(|| {
                format!("create workbench agent root {}", host_agent_root.display())
            })?;
        }
        Err(error) => return Err(error).context("inspect workbench agent root"),
    }
    let canonical_root = std::fs::canonicalize(host_agent_root).with_context(|| {
        format!(
            "canonicalize workbench agent root {}",
            host_agent_root.display()
        )
    })?;
    anyhow::ensure!(
        canonical_root.starts_with(&parent),
        "workbench agent root escaped its configured host boundary"
    );
    for child in ["inputs", "workspaces", "artifacts"] {
        let path = canonical_root.join(child);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "workbench {child} root must be a real directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path)
                    .with_context(|| format!("create workbench {child} root {}", path.display()))?;
            }
            Err(error) => return Err(error).context("inspect workbench persistent root"),
        }
        let canonical = std::fs::canonicalize(&path)
            .with_context(|| format!("canonicalize workbench {child} root {}", path.display()))?;
        anyhow::ensure!(
            canonical.parent() == Some(canonical_root.as_path()),
            "workbench {child} root escaped its agent boundary"
        );
    }
    Ok(())
}

fn await_workbench_startup_attestation(
    child_pid: u32,
    expected_nonce: &str,
    expected_landlock_abi: u8,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(WORKBENCH_ATTESTATION_TIMEOUT_MS);
    let path = PathBuf::from(format!(
        "/proc/{child_pid}/root/tmp/.sentinel-workbench-attestation-{expected_nonce}.json"
    ));
    loop {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.nlink() == 1
                        && metadata.len() > 0
                        && metadata.len() <= WORKBENCH_ATTESTATION_MAX_BYTES,
                    "workbench startup attestation failed its file boundary"
                );
                let file =
                    std::fs::File::open(&path).context("open workbench startup attestation")?;
                let opened = file
                    .metadata()
                    .context("inspect opened workbench startup attestation")?;
                ensure!(
                    opened.dev() == metadata.dev() && opened.ino() == metadata.ino(),
                    "workbench startup attestation identity changed before read"
                );
                let mut bytes = Vec::with_capacity(opened.len() as usize);
                file.take(WORKBENCH_ATTESTATION_MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .context("read workbench startup attestation")?;
                ensure!(
                    !bytes.is_empty() && bytes.len() as u64 <= WORKBENCH_ATTESTATION_MAX_BYTES,
                    "workbench startup attestation exceeded its bound"
                );
                validate_workbench_startup_attestation(
                    &bytes,
                    expected_nonce,
                    expected_landlock_abi,
                    child_pid,
                )?;
                std::fs::remove_file(&path)
                    .context("remove consumed workbench startup attestation")?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::time::Instant::now() >= deadline {
                    bail!("workbench startup attestation timed out");
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => return Err(error).context("inspect workbench startup attestation"),
        }
    }
}

fn validate_workbench_startup_attestation(
    bytes: &[u8],
    expected_nonce: &str,
    expected_landlock_abi: u8,
    expected_child_pid: u32,
) -> Result<()> {
    let attestation: WorkbenchStartupAttestation =
        serde_json::from_slice(bytes).context("decode workbench startup attestation")?;
    ensure!(
        attestation.schema_version == WORKBENCH_ATTESTATION_SCHEMA_VERSION
            && attestation.nonce == expected_nonce
            && attestation.wrapper_version == env!("CARGO_PKG_VERSION")
            && attestation.runtime_version == sentinel_common::WORKBENCH_AGENT_RUNTIME_VERSION
            && attestation.landlock_abi == expected_landlock_abi
            && attestation.host_pid == expected_child_pid,
        "workbench startup attestation did not match its exact child"
    );
    Ok(())
}

fn workbench_host_agent_root(agent_name: &str) -> PathBuf {
    PathBuf::from("/ram/agents").join(agent_name)
}

fn landlock_wrapper_path() -> PathBuf {
    // 1. Same directory as current executable
    if let Ok(exe) = std::env::current_exe() {
        let candidate = exe
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("landlock-wrapper");
        if candidate.exists() {
            return candidate;
        }
    }

    // 2. Standard deployment path
    let deploy = PathBuf::from("/opt/sentinel/bin/landlock-wrapper");
    if deploy.exists() {
        return deploy;
    }

    // 3. System path (fallback)
    PathBuf::from("/usr/local/bin/landlock-wrapper")
}

/// Reads the network-namespace inode behind `/proc/<pid>/ns/net`.
///
/// The symlink target has the form `net:[INODE]`; the inode uniquely
/// identifies the namespace. Returns `None` if the link cannot be read or
/// parsed (transient race / process already gone).
fn read_netns_inode(ns_path: &str) -> Option<u64> {
    let target = std::fs::read_link(ns_path).ok()?;
    parse_ns_inode(&target.to_string_lossy())
}

/// Parses the inode out of a `net:[INODE]` namespace link target.
fn parse_ns_inode(link: &str) -> Option<u64> {
    let start = link.find('[')? + 1;
    let end = link.find(']')?;
    link.get(start..end)?.parse().ok()
}

/// Classifies isolation from the daemon's and the agent's netns inodes.
///
/// Pure decision function (unit-tested): different inodes -> [`IsolationStatus::Isolated`];
/// equal inodes -> [`IsolationStatus::NotIsolated`] (agent shares the daemon netns);
/// any missing inode -> [`IsolationStatus::ProbeError`] (never a cage breach).
fn classify_isolation(daemon_ns: Option<u64>, agent_ns: Option<u64>) -> IsolationStatus {
    match (daemon_ns, agent_ns) {
        (Some(d), Some(a)) if d == a => IsolationStatus::NotIsolated,
        (Some(_), Some(_)) => IsolationStatus::Isolated,
        _ => IsolationStatus::ProbeError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_warning_variants() {
        // Verify all warning variants exist and are distinct
        let warnings = [
            SandboxWarning::LandlockNotAvailable,
            SandboxWarning::CgroupNotDelegated("io".into()),
            SandboxWarning::BwrapUsernsDenied,
            SandboxWarning::IoNotDelegated,
            SandboxWarning::OomScoreFailed("test".into()),
        ];
        assert_eq!(warnings.len(), 5);
        assert_ne!(warnings[0], warnings[1]);
    }

    #[test]
    fn sandbox_handle_defaults() {
        let handle = SandboxHandle {
            agent_name: "test".into(),
            cgroup_created: false,
            cgroup_id: None,
            io_available: false,
            bwrap_pid: None,
            landlock_applied: false,
            network_isolated: false,
        };
        assert_eq!(handle.agent_name, "test");
        assert!(!handle.cgroup_created);
        assert!(handle.bwrap_pid.is_none());
        assert!(!handle.network_isolated);
    }

    #[test]
    fn classify_isolation_three_states() {
        // #75: different inodes -> isolated; equal -> not isolated (cage breach);
        // missing inode -> probe error (must never terminate a healthy agent).
        assert_eq!(
            classify_isolation(Some(4026531840), Some(4026532500)),
            IsolationStatus::Isolated
        );
        assert_eq!(
            classify_isolation(Some(4026531840), Some(4026531840)),
            IsolationStatus::NotIsolated
        );
        assert_eq!(
            classify_isolation(None, Some(4026532500)),
            IsolationStatus::ProbeError
        );
        assert_eq!(
            classify_isolation(Some(4026531840), None),
            IsolationStatus::ProbeError
        );
        assert_eq!(classify_isolation(None, None), IsolationStatus::ProbeError);
    }

    #[test]
    fn parse_ns_inode_extracts_inode() {
        assert_eq!(parse_ns_inode("net:[4026531840]"), Some(4026531840));
        assert_eq!(parse_ns_inode("net:[1]"), Some(1));
        assert_eq!(parse_ns_inode("garbage"), None);
        assert_eq!(parse_ns_inode("net:[notnum]"), None);
        assert_eq!(parse_ns_inode("net:[]"), None);
    }

    #[test]
    fn teardown_cgroup_kills_members_before_remove() {
        let calls = std::cell::RefCell::new(Vec::new());

        cleanup_cgroup_after_process_exit_with(
            "agent",
            |_| {
                calls.borrow_mut().push("list");
                Ok(vec![42])
            },
            |_| {
                calls.borrow_mut().push("kill");
                Ok(1)
            },
            |_| {
                let killed = calls.borrow().contains(&"kill");
                assert!(killed, "occupied cgroup must be killed before remove");
                calls.borrow_mut().push("remove");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(calls.into_inner(), vec!["list", "kill", "remove"]);
    }

    #[test]
    fn teardown_cgroup_removes_empty_without_kill() {
        let calls = std::cell::RefCell::new(Vec::new());

        cleanup_cgroup_after_process_exit_with(
            "agent",
            |_| {
                calls.borrow_mut().push("list");
                Ok(Vec::new())
            },
            |_| {
                calls.borrow_mut().push("kill");
                Ok(0)
            },
            |_| {
                calls.borrow_mut().push("remove");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(calls.into_inner(), vec!["list", "remove"]);
    }

    #[test]
    fn acknowledgement_at_grace_has_one_stable_winner_across_barriers() {
        fn cancelling_state() -> ProtocolSupervisionState {
            ProtocolSupervisionState {
                reader: ProtocolReaderState {
                    invocation_id: Some("invocation".to_string()),
                    ..ProtocolReaderState::default()
                },
                cancel: Some(ProtocolCancelClaim {
                    owner: ProtocolCancelOwner::Explicit,
                    claimed_at: std::time::Instant::now()
                        - std::time::Duration::from_millis(PROTOCOL_CANCEL_GRACE_MS),
                    requested_at_unix_ms: 1,
                    send_started: true,
                    #[cfg(test)]
                    sent: true,
                }),
                ..ProtocolSupervisionState::default()
            }
        }

        let terminal_wins = Arc::new(ProtocolSupervision {
            state: Mutex::new(cancelling_state()),
            deadline_send_barrier: Mutex::new(None),
            post_terminal_barrier: Mutex::new(None),
            pre_quiescence_barrier: Mutex::new(None),
        });
        let terminal_start = Arc::new(std::sync::Barrier::new(2));
        let terminal_committed = Arc::new(std::sync::Barrier::new(2));
        let terminal_worker = {
            let supervision = Arc::clone(&terminal_wins);
            let start = Arc::clone(&terminal_start);
            let committed = Arc::clone(&terminal_committed);
            std::thread::spawn(move || {
                start.wait();
                let mut reader = supervision.lock().reader.clone();
                reader.terminal = true;
                assert!(commit_protocol_reader_state(
                    &mut supervision.lock(),
                    reader,
                    true,
                    std::time::Instant::now(),
                ));
                committed.wait();
            })
        };
        terminal_start.wait();
        terminal_committed.wait();
        assert!(!expire_protocol_cancel_if_due(
            &mut terminal_wins.lock(),
            std::time::Instant::now(),
        ));
        terminal_worker.join().unwrap();
        assert!(matches!(
            terminal_wins.lock().outcome,
            ProtocolOutcome::TerminalPending { .. }
        ));

        let timeout_wins = Arc::new(ProtocolSupervision {
            state: Mutex::new(cancelling_state()),
            deadline_send_barrier: Mutex::new(None),
            post_terminal_barrier: Mutex::new(None),
            pre_quiescence_barrier: Mutex::new(None),
        });
        let timeout_start = Arc::new(std::sync::Barrier::new(2));
        let timeout_committed = Arc::new(std::sync::Barrier::new(2));
        let timeout_worker = {
            let supervision = Arc::clone(&timeout_wins);
            let start = Arc::clone(&timeout_start);
            let committed = Arc::clone(&timeout_committed);
            std::thread::spawn(move || {
                start.wait();
                assert!(expire_protocol_cancel_if_due(
                    &mut supervision.lock(),
                    std::time::Instant::now(),
                ));
                committed.wait();
            })
        };
        timeout_start.wait();
        timeout_committed.wait();
        let mut reader = timeout_wins.lock().reader.clone();
        reader.terminal = true;
        assert!(!commit_protocol_reader_state(
            &mut timeout_wins.lock(),
            reader,
            true,
            std::time::Instant::now(),
        ));
        timeout_worker.join().unwrap();
        assert!(matches!(
            timeout_wins.lock().outcome,
            ProtocolOutcome::FailurePending(ProtocolSupervisionFailure::Cancelled)
        ));
    }

    #[test]
    fn post_reap_cgroup_kill_and_remove_retries_never_resignal_numeric_pid() {
        for fail_kill in [true, false] {
            let mut process = AgentProcess::launch_fixture().unwrap();
            process.terminate_checked().unwrap();
            let signal_attempts = process.termination_signal_counter();
            assert_eq!(signal_attempts.load(Ordering::Acquire), 1);

            let attempts = std::cell::Cell::new(0usize);
            let first = process.retry_supervised_cgroup_cleanup_with(
                "retry-cgroup",
                |_| Ok(if fail_kill { vec![4242] } else { Vec::new() }),
                |_| {
                    attempts.set(attempts.get() + 1);
                    if fail_kill && attempts.get() == 1 {
                        bail!("injected first cgroup kill failure");
                    }
                    Ok(1)
                },
                |_| {
                    attempts.set(attempts.get() + 1);
                    if !fail_kill && attempts.get() == 1 {
                        bail!("injected first cgroup remove failure");
                    }
                    Ok(())
                },
            );
            assert!(first.is_err());
            assert!(!process.protocol_supervision_snapshot().cgroup_quiesced);
            assert_eq!(signal_attempts.load(Ordering::Acquire), 1);

            process
                .retry_supervised_cgroup_cleanup_with(
                    "retry-cgroup",
                    |_| Ok(if fail_kill { vec![4242] } else { Vec::new() }),
                    |_| Ok(1),
                    |_| Ok(()),
                )
                .unwrap();
            assert!(process.protocol_supervision_snapshot().cgroup_quiesced);
            assert_eq!(
                signal_attempts.load(Ordering::Acquire),
                1,
                "cgroup-only retry must not signal a reused numeric process target"
            );
        }
    }

    #[test]
    fn workbench_roots_stay_on_writable_agent_backing_with_active_fs_mount() {
        assert_eq!(
            workbench_host_agent_root("alice"),
            PathBuf::from("/ram/agents/alice")
        );

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("AGENT-01");
        prepare_workbench_roots(&root).unwrap();
        for child in ["inputs", "workspaces", "artifacts"] {
            assert!(root.join(child).is_dir());
        }
        std::fs::remove_dir(root.join("inputs")).unwrap();
        std::os::unix::fs::symlink(directory.path(), root.join("inputs")).unwrap();
        assert!(prepare_workbench_roots(&root).is_err());
    }

    #[test]
    fn workbench_startup_attestation_is_exact_child_and_version_bound() {
        let nonce = "018f3f32-4f01-4f2c-a6c1-f6f4a81b2903";
        let bytes = |child_pid: u32, abi: u8, wrapper_version: &str, runtime_version: &str| {
            serde_json::to_vec(&serde_json::json!({
                "schema_version": WORKBENCH_ATTESTATION_SCHEMA_VERSION,
                "nonce": nonce,
                "wrapper_version": wrapper_version,
                "runtime_version": runtime_version,
                "landlock_abi": abi,
                "host_pid": child_pid,
            }))
            .unwrap()
        };
        let valid = bytes(
            4242,
            4,
            env!("CARGO_PKG_VERSION"),
            sentinel_common::WORKBENCH_AGENT_RUNTIME_VERSION,
        );
        validate_workbench_startup_attestation(&valid, nonce, 4, 4242).unwrap();
        assert!(validate_workbench_startup_attestation(&valid, nonce, 4, 4243).is_err());
        assert!(validate_workbench_startup_attestation(&valid, nonce, 3, 4242).is_err());
        assert!(validate_workbench_startup_attestation(
            &bytes(
                4242,
                4,
                "foreign",
                sentinel_common::WORKBENCH_AGENT_RUNTIME_VERSION,
            ),
            nonce,
            4,
            4242,
        )
        .is_err());
        assert!(validate_workbench_startup_attestation(
            &bytes(4242, 4, env!("CARGO_PKG_VERSION"), "foreign"),
            nonce,
            4,
            4242,
        )
        .is_err());
        assert!(validate_workbench_startup_attestation(b"{}", nonce, 4, 4242).is_err());
    }

    #[test]
    fn workbench_spawn_prerequisites_reject_missing_or_shared_boundaries() {
        assert!(validate_workbench_static_prerequisites(None, true, true).is_err());
        assert!(validate_workbench_static_prerequisites(Some(4), false, true).is_err());
        assert!(validate_workbench_static_prerequisites(Some(4), true, false).is_err());
        validate_workbench_static_prerequisites(Some(4), true, true).unwrap();

        assert!(validate_workbench_child_boundary(None, true, IsolationStatus::Isolated).is_err());
        assert!(
            validate_workbench_child_boundary(Some(42), false, IsolationStatus::Isolated).is_err()
        );
        assert!(
            validate_workbench_child_boundary(Some(42), true, IsolationStatus::NotIsolated,)
                .is_err()
        );
        assert!(
            validate_workbench_child_boundary(Some(42), true, IsolationStatus::ProbeError,)
                .is_err()
        );
        assert_eq!(
            validate_workbench_child_boundary(Some(42), true, IsolationStatus::Isolated).unwrap(),
            42
        );
    }

    #[test]
    #[ignore] // Needs real system capabilities (VM only)
    fn enforcer_detect() {
        let (enforcer, warnings) = SandboxEnforcer::detect();
        // On the VM, we expect landlock + cgroups + bwrap to be available
        println!("Landlock ABI: {:?}", enforcer.landlock_abi);
        println!("cgroup available: {}", enforcer.cgroup_available);
        println!("bwrap available: {}", enforcer.bwrap_available);
        println!("Warnings: {:?}", warnings);
    }
}
