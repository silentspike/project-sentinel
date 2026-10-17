use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_runtime::command_sandbox::{launch_command, CommandBoundary, CHILD_MODE};
use agent_runtime::WorkbenchExecutor;
use sentinel_common::{
    AgentId, CommandRule, WorkbenchMessage, WorkbenchOutcome, WorkbenchRequest,
    WorkbenchResourceLimits, WorkbenchTool, WORKBENCH_RUNTIME_BWRAP, WORKBENCH_SCHEMA_VERSION,
};

static INVOCATION: AtomicU64 = AtomicU64::new(1);
const TEST_ENV_CANARY: &str = "must-not-survive";
const SETUP_DIAGNOSTIC_BYTES: usize = 4096;

fn diagnostic_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).replace(TEST_ENV_CANARY, "[redacted-test-canary]");
    let mut end = text.len().min(SETUP_DIAGNOSTIC_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{:?} (captured_bytes={}, truncated={})",
        &text[..end],
        bytes.len(),
        end < text.len()
    )
}

fn setup_diagnostic(output: &Output, phase: &str, error: &dyn std::fmt::Display) -> String {
    use std::os::unix::process::ExitStatusExt;
    format!(
        "fixture {phase}: {}; facade_status={} code={:?} signal={:?}; stdout={}; stderr={}",
        diagnostic_output(error.to_string().as_bytes()),
        output.status,
        output.status.code(),
        output.status.signal(),
        diagnostic_output(&output.stdout),
        diagnostic_output(&output.stderr),
    )
}

fn limits() -> WorkbenchResourceLimits {
    WorkbenchResourceLimits {
        wall_time_ms: 5000,
        cpu_time_ms: 3000,
        memory_bytes: 256 * 1024 * 1024,
        process_count: 32,
        file_bytes: 1024 * 1024,
        stdout_bytes: 65536,
        stderr_bytes: 65536,
    }
}

fn runtime_binary() -> PathBuf {
    std::env::var_os("SENTINEL_COMMAND_TEST_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_agent-runtime")))
}

struct Fixture {
    _directory: tempfile::TempDir,
    _private_workspace: tempfile::TempDir,
    workspace_root: PathBuf,
    input_root: PathBuf,
    input_project: PathBuf,
    project_id: String,
    workspace: PathBuf,
    inputs: PathBuf,
    scripts: PathBuf,
    artifacts: PathBuf,
    foreign: PathBuf,
}

struct FixtureCommand {
    arguments: Vec<String>,
    inputs: Vec<PathBuf>,
    _script: Option<tempfile::NamedTempFile>,
}

impl Fixture {
    fn new() -> Self {
        Self::create(None)
    }

    fn kernel() -> Self {
        let workspace = PathBuf::from(
            std::env::var_os("SENTINEL_COMMAND_TEST_WORKSPACE_PARENT")
                .expect("deployment harness must supply the real SentinelFUSE agent root"),
        );
        let inputs = PathBuf::from(
            std::env::var_os("SENTINEL_COMMAND_TEST_INPUT_ROOT")
                .expect("deployment harness must supply the matching private broker input root"),
        );
        assert_eq!(
            nix::sys::statfs::statfs(&workspace)
                .unwrap()
                .filesystem_type(),
            nix::sys::statfs::FUSE_SUPER_MAGIC,
            "workspace parent must be a real FUSE mount"
        );
        Self::create(Some((workspace, inputs)))
    }

    fn create(roots: Option<(PathBuf, PathBuf)>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let (workspace_root, input_root) = match roots {
            Some(roots) => roots,
            None => {
                let workspace = root.join("workspaces");
                let inputs = root.join("inputs");
                fs::create_dir(&workspace).unwrap();
                fs::create_dir(&inputs).unwrap();
                (workspace, inputs)
            }
        };
        let workspace_root = fs::canonicalize(workspace_root).unwrap();
        let input_root = fs::canonicalize(input_root).unwrap();
        assert!(
            !workspace_root.starts_with(&input_root) && !input_root.starts_with(&workspace_root)
        );
        let private_workspace = tempfile::Builder::new()
            .prefix("project-")
            .tempdir_in(&workspace_root)
            .unwrap();
        let project_id = private_workspace
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let workspace = fs::canonicalize(private_workspace.path())
            .unwrap()
            .join("workspace");
        let input_project = input_root.join(&project_id);
        fs::create_dir(&input_project).unwrap();
        let input_scope = input_project.join("workspace");
        fs::create_dir(&input_scope).unwrap();
        let inputs = input_scope.join("declared");
        let scripts = input_scope.join("fixture-code");
        let artifacts = root.join("artifacts");
        let foreign = root.join("foreign");
        for path in [&workspace, &inputs, &scripts, &artifacts, &foreign] {
            fs::create_dir(path).unwrap();
        }
        fs::write(inputs.join("source.txt"), "declared source").unwrap();
        fs::write(artifacts.join("receipt.json"), "trusted completion").unwrap();
        fs::write(foreign.join("private.txt"), "other assignment").unwrap();
        Self {
            _directory: directory,
            _private_workspace: private_workspace,
            workspace_root,
            input_root,
            input_project,
            project_id,
            workspace,
            inputs,
            scripts,
            artifacts,
            foreign,
        }
    }

    fn python(&self, source: &str) -> Output {
        self.run("python3", &["-I", "-c", source], true)
    }

    fn boundary(&self) -> CommandBoundary {
        assert!(
            std::env::var_os("SENTINEL_COMMAND_TEST_WORKSPACE_PARENT").is_some(),
            "deployment harness must supply the private budget-enforced FUSE workspace parent"
        );
        CommandBoundary {
            cgroup_root: std::env::var_os("SENTINEL_COMMAND_TEST_CGROUP_ROOT")
                .map(PathBuf::from)
                .expect("deployment harness must supply delegated cgroup v2"),
            workspace_budget_bytes: std::env::var("SENTINEL_COMMAND_TEST_WORKSPACE_BUDGET_BYTES")
                .expect("deployment harness must attest the enforced FUSE workspace budget")
                .parse()
                .unwrap(),
        }
    }

    fn run(&self, program: &str, arguments: &[&str], inputs: bool) -> Output {
        let prepared = self.prepare_command(program, arguments, inputs);
        let scoped = launch_command(
            &runtime_binary(),
            &self.workspace,
            &prepared.inputs,
            program,
            &limits(),
            &self.boundary(),
        )
        .unwrap();
        let mut command = scoped.command;
        let mut result = command
            .args(&prepared.arguments)
            .env("SENTINEL_FORBIDDEN_SECRET", TEST_ENV_CANARY)
            .env("SENTINEL_WORKBENCH_ATTESTATION_NONCE", TEST_ENV_CANARY)
            .output()
            .unwrap();
        drop(command);
        scoped
            .membership
            .kill_and_wait()
            .unwrap_or_else(|error| panic!("{}", setup_diagnostic(&result, "quiescence", &error)));
        let mut setup = scoped.setup;
        let confirmed = setup.poll().unwrap_or_else(|error| {
            panic!(
                "{}",
                setup_diagnostic(&result, "setup/exec/terminal", &error)
            )
        });
        assert!(
            confirmed,
            "{}",
            setup_diagnostic(
                &result,
                "setup/exec/terminal",
                &"channel did not confirm execution"
            )
        );
        result.status = setup.terminal_status().unwrap_or_else(|| {
            panic!(
                "{}",
                setup_diagnostic(
                    &result,
                    "terminal",
                    &"broker did not prove candidate status"
                )
            )
        });
        result
    }

    fn prepare_command(&self, program: &str, arguments: &[&str], inputs: bool) -> FixtureCommand {
        let mut prepared = FixtureCommand {
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            inputs: if inputs {
                vec![self.inputs.join("source.txt")]
            } else {
                Vec::new()
            },
            _script: None,
        };
        let (source, suffix, prefix) = match (program, arguments) {
            ("python3", ["-I", "-c", source]) => (*source, ".py", vec!["-I".to_owned()]),
            ("node", ["-e", source]) => (*source, ".js", Vec::new()),
            _ => return prepared,
        };
        // Inline multiline argv violates the real command grammar. Declare the
        // exact script as a RO input, outside both the FUSE budget and data view.
        let script = tempfile::Builder::new()
            .prefix("command-")
            .suffix(suffix)
            .tempfile_in(&self.scripts)
            .unwrap();
        fs::write(script.path(), source.as_bytes()).unwrap();
        prepared.arguments = prefix;
        prepared
            .arguments
            .push(script.path().to_str().unwrap().to_owned());
        prepared.inputs.push(script.path().to_owned());
        prepared._script = Some(script);
        prepared
    }

    fn require_command_support(&self) {
        let result = self.python("from pathlib import Path\np = Path(__file__)\nassert p.read_text()\ntry:\n    p.write_text('must not mutate RO fixture code')\n    raise AssertionError('fixture script was writable')\nexcept OSError:\n    pass\nprint('strict command sandbox active')\n");
        assert!(
            result.status.success(),
            "strict child positive did not execute: {:?}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(result.stdout, b"strict command sandbox active\n");
    }

    fn executor(&self) -> WorkbenchExecutor {
        WorkbenchExecutor::with_input_root(&self.workspace_root, &self.artifacts, &self.input_root)
            .with_command_runner(runtime_binary())
            .with_command_boundary(self.boundary())
    }

    fn request(&self, program: &str, source: &str) -> WorkbenchRequest {
        let workspace = &self.workspace;
        let script = if program == "node" {
            "program.js"
        } else {
            "program.py"
        };
        fs::write(workspace.join(script), source).unwrap();
        let args = if program == "node" {
            vec![script.into()]
        } else {
            vec!["-I".into(), script.into()]
        };
        WorkbenchRequest {
            schema_version: WORKBENCH_SCHEMA_VERSION,
            invocation_id: format!(
                "018f3f32-4f01-7f2c-a6c1-{:012x}",
                INVOCATION.fetch_add(1, Ordering::Relaxed)
            ),
            agent_id: AgentId(7),
            project_id: self.project_id.clone(),
            work_item_id: "workspace".into(),
            workspace_id: format!("{}:workspace", self.project_id),
            caller_id: "AGENT-07".into(),
            caller_role: "developer".into(),
            assignment_version: 2,
            credential_generation: 3,
            policy_digest: "a".repeat(64),
            tool_profile: "coding-v1".into(),
            tool_profile_digest: "b".repeat(64),
            runtime_key: WORKBENCH_RUNTIME_BWRAP.into(),
            capabilities: BTreeSet::from(["command.run_allowlisted".into()]),
            output_artifact_kinds: BTreeSet::from(["source_tree".into()]),
            inputs: Vec::new(),
            command_policy: vec![CommandRule {
                program: program.into(),
                required_arg_prefix: if program == "python3" {
                    vec!["-I".into()]
                } else {
                    Vec::new()
                },
                max_args: 2,
            }],
            resource_limits: limits(),
            deadline_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + 10000,
            attempt: 1,
            tool: WorkbenchTool::RunCommand {
                program: program.into(),
                args,
            },
            input_digest: String::new(),
        }
        .bind_digest()
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let cleanup = fs::remove_dir_all(&self.input_project);
        if !thread::panicking() {
            cleanup.expect("private fixture input scope cleanup failed");
        }
    }
}

#[test]
fn fixture_local_paths_match_broker_scope_and_executor_binding() {
    for (program, script) in [("python3", "program.py"), ("node", "program.js")] {
        let fixture = Fixture::new();
        let request = fixture.request(program, "layout only");
        request.validate_for_replay().unwrap();
        let relative = fixture
            .workspace
            .strip_prefix(&fixture.workspace_root)
            .unwrap();
        assert_eq!(relative.components().count(), 2);
        assert_eq!(
            relative,
            Path::new(&request.project_id).join(&request.work_item_id)
        );
        assert_eq!(
            fixture.inputs,
            fixture.input_root.join(relative).join("declared")
        );
        assert_eq!(request.input_digest, request.canonical_digest().unwrap());
        assert!(fixture.workspace.join(script).is_file());
        assert!(!fixture.workspace.join("project-01/work-04").exists());
        let mut ungranted = request.clone();
        ungranted.tool = WorkbenchTool::RunCommand {
            program: program.into(),
            args: vec!["-S".into(), script.into()],
        };
        let ungranted = ungranted.bind_digest().unwrap();
        assert_eq!(
            ungranted.validate_for_replay(),
            Err(sentinel_common::WorkbenchValidationError::CommandDenied)
        );

        let (source, inline) = if program == "python3" {
            (
                "print('exact python source')\n",
                vec!["-I", "-c", "print('exact python source')\n"],
            )
        } else {
            (
                "console.log('exact node source');\n",
                vec!["-e", "console.log('exact node source');\n"],
            )
        };
        assert_eq!(
            WorkbenchTool::RunCommand {
                program: program.into(),
                args: inline
                    .iter()
                    .map(|argument| (*argument).to_owned())
                    .collect(),
            }
            .validate_shape(),
            Err(sentinel_common::WorkbenchValidationError::CommandArgumentEscape),
            "real broker grammar must keep rejecting multiline command arguments"
        );
        for declare_data in [false, true] {
            let prepared = fixture.prepare_command(program, &inline, declare_data);
            let script = prepared._script.as_ref().unwrap().path().to_owned();
            assert_eq!(fs::read(&script).unwrap(), source.as_bytes());
            assert!(script.starts_with(fixture.input_root.join(relative)));
            assert!(!script.starts_with(&fixture.workspace_root));
            assert!(!script.starts_with(&fixture.inputs));
            assert_eq!(prepared.inputs.last(), Some(&script));
            assert_eq!(prepared.inputs.len(), if declare_data { 2 } else { 1 });
            assert_eq!(
                prepared.inputs.contains(&fixture.inputs.join("source.txt")),
                declare_data
            );
            assert_eq!(prepared.arguments.last().unwrap(), script.to_str().unwrap());
            // Mirror only the broker's exact-declared-path transport form;
            // undeclared absolute arguments remain forbidden by production.
            let arguments = prepared
                .arguments
                .iter()
                .map(|argument| {
                    if prepared
                        .inputs
                        .iter()
                        .any(|input| input == Path::new(argument))
                    {
                        Path::new(argument)
                            .strip_prefix("/")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned()
                    } else {
                        argument.clone()
                    }
                })
                .collect();
            WorkbenchTool::RunCommand {
                program: program.into(),
                args: arguments,
            }
            .validate_shape()
            .unwrap();
            drop(prepared);
            assert!(
                !script.exists(),
                "private RO script staging must be cleaned up"
            );
        }
    }
}

#[test]
fn fixture_local_cleanup_preserves_foreign_project_scopes() {
    use std::os::unix::process::ExitStatusExt;
    let captured = Output {
        status: std::process::ExitStatus::from_raw(126 << 8),
        stdout: format!("observed stdout {TEST_ENV_CANARY}").into_bytes(),
        stderr: format!(
            "actual client failure\n{}",
            "x".repeat(SETUP_DIAGNOSTIC_BYTES + 1)
        )
        .into_bytes(),
    };
    let diagnostic = setup_diagnostic(&captured, "setup/exec/terminal", &"unconfirmed");
    assert!(diagnostic.contains("code=Some(126)"));
    assert!(diagnostic.contains("observed stdout"));
    assert!(diagnostic.contains("actual client failure"));
    assert!(diagnostic.contains("truncated=true"));
    assert!(diagnostic.contains("[redacted-test-canary]"));
    assert!(!diagnostic.contains(TEST_ENV_CANARY));
    assert!(diagnostic.len() < 2 * SETUP_DIAGNOSTIC_BYTES);
    assert!(diagnostic_output(&[0xff, 0xfe]).contains("captured_bytes=2"));

    let root = tempfile::tempdir().unwrap();
    let workspace_root = root.path().join("workspaces");
    let input_root = root.path().join("inputs");
    fs::create_dir(&workspace_root).unwrap();
    fs::create_dir(&input_root).unwrap();
    fs::write(input_root.join("foreign"), b"retained foreign scope").unwrap();
    let fixture = Fixture::create(Some((workspace_root.clone(), input_root.clone())));
    let project = fixture.project_id.clone();
    drop(fixture);
    assert!(!workspace_root.join(&project).exists());
    assert!(!input_root.join(&project).exists());
    assert_eq!(
        fs::read(input_root.join("foreign")).unwrap(),
        b"retained foreign scope"
    );
}

fn literal(path: &Path) -> String {
    serde_json::to_string(path.to_str().unwrap()).unwrap()
}

#[test]
#[ignore = "requires Landlock ABI 6: mandatory deployment-host gate"]
fn real_command_child_runs_workspace_code_and_keeps_actual_failure_output() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    fs::write(fixture.workspace.join("program.py"),
        "from pathlib import Path\nPath('result.txt').write_text('real result')\nprint('observed stdout')\nraise SystemExit(7)\n").unwrap();
    let result = fixture.run("python3", &["-I", "program.py"], false);
    assert_eq!(result.status.code(), Some(7));
    assert_eq!(result.stdout, b"observed stdout\n");
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("result.txt")).unwrap(),
        "real result"
    );
}

#[test]
#[ignore = "requires Landlock ABI 6: mandatory deployment-host gate"]
fn real_command_child_denies_receipts_foreign_paths_and_parent_authority() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    symlink(&fixture.artifacts, fixture.workspace.join("artifact-alias")).unwrap();
    let parent_limits =
        nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE).unwrap();
    let receipt_before = fs::metadata(fixture.artifacts.join("receipt.json")).unwrap();
    let targets = serde_json::to_string(&[
        fixture.artifacts.join("receipt.json").to_str().unwrap(),
        fixture.foreign.join("private.txt").to_str().unwrap(),
        fixture.artifacts.to_str().unwrap(),
        fixture.foreign.to_str().unwrap(),
        fixture
            .workspace
            .join("artifact-alias/receipt.json")
            .to_str()
            .unwrap(),
    ])
    .unwrap();
    let result = fixture.python(&format!(
        r#"
import os
from pathlib import Path
for target in {targets}:
    for mode in ('r', 'w'):
        try:
            with open(target, mode):
                raise AssertionError('foreign authority was accessible')
        except (PermissionError, FileNotFoundError):
            pass
    for mutate in (lambda: os.chmod(target, 0o777), lambda: os.utime(target, (1, 1))):
        try:
            mutate()
            raise AssertionError('foreign metadata authority was accessible')
        except OSError:
            pass
try:
    Path('/proc/{}/mem').read_bytes()
    raise AssertionError('parent memory was accessible')
except (PermissionError, FileNotFoundError, ProcessLookupError):
    pass
try:
    os.kill({}, 0)
    raise AssertionError('parent signal authority was accessible')
except (PermissionError, ProcessLookupError):
    pass
import resource
try:
    resource.prlimit({}, resource.RLIMIT_NOFILE, (32, 32))
    raise AssertionError('trusted parent prlimit authority was accessible')
except (PermissionError, ProcessLookupError):
    pass
for name in os.listdir('/proc/self/fd'):
    if int(name) > 2:
        try:
            os.fstat(int(name))
            raise AssertionError('an authority descriptor survived exec')
        except OSError:
            pass
assert set(os.environ) <= {{'HOME', 'TMPDIR', 'PATH', 'LANG', 'LC_ALL'}}
assert os.environ['HOME'] == os.getcwd()
assert os.environ['TMPDIR'] == os.path.join(os.getcwd(), '.command-tmp')
print('authority boundaries held')
"#,
        std::process::id(),
        std::process::id(),
        std::process::id()
    ));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"authority boundaries held\n");
    assert_eq!(
        nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE).unwrap(),
        parent_limits
    );
    use std::os::unix::fs::MetadataExt;
    let receipt_after = fs::metadata(fixture.artifacts.join("receipt.json")).unwrap();
    assert_eq!(
        (receipt_after.mode(), receipt_after.mtime()),
        (receipt_before.mode(), receipt_before.mtime())
    );
    assert_eq!(
        fs::read_to_string(fixture.artifacts.join("receipt.json")).unwrap(),
        "trusted completion"
    );
    assert_eq!(
        fs::read_to_string(fixture.foreign.join("private.txt")).unwrap(),
        "other assignment"
    );
}

#[test]
#[ignore = "requires Landlock ABI 6: mandatory deployment-host gate"]
fn real_command_child_reads_declared_inputs_but_cannot_mutate_them() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    let input = literal(&fixture.inputs.join("source.txt"));
    let root = literal(&fixture.inputs);
    fs::write(
        fixture.inputs.join("retained.txt"),
        b"previous request only",
    )
    .unwrap();
    let result = fixture.python(&format!(
        r#"
from pathlib import Path
source = Path({input})
assert source.read_text() == 'declared source'
assert sorted(p.name for p in Path({root}).iterdir()) == ['source.txt']
try:
    Path({root}, 'retained.txt').read_bytes()
    raise AssertionError('undeclared retained input was exposed')
except OSError:
    pass
for mutate in (lambda: source.write_text('changed'), lambda: source.unlink(),
               lambda: source.chmod(0o600), lambda: __import__('os').utime(source, (1, 1)),
               lambda: Path({root}, 'extra.txt').write_text('new')):
    try:
        mutate()
        raise AssertionError('input mutation succeeded')
    except OSError:
        pass
print('read-only inputs held')
"#
    ));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.inputs.join("source.txt")).unwrap(),
        "declared source"
    );
    assert!(!fixture.inputs.join("extra.txt").exists());
}

#[test]
#[ignore = "requires Landlock ABI 6: mandatory deployment-host gate"]
fn real_command_child_denies_tcp_but_preserves_tool_subprocesses_and_loader_reexec() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    let result = fixture.python(
        r#"
import socket, subprocess
for action in (lambda: socket.socket().connect(('127.0.0.1', 9)),
               lambda: socket.socket().bind(('127.0.0.1', 0))):
    try:
        action()
        raise AssertionError('ungranted operation succeeded')
    except PermissionError:
        pass
subprocess.run(['/usr/bin/true'], check=True)
from pathlib import Path
for loader in ('/lib64/ld-linux-x86-64.so.2', '/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2'):
    if Path(loader).exists():
        subprocess.run([loader, '/usr/bin/true'], check=True)
        break
print('network isolation and tool compatibility held')
"#,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[ignore = "requires Landlock ABI 6: mandatory deployment-host gate"]
fn real_command_child_applies_file_limit_before_running_code() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    let result = fixture.python(
        r#"
from pathlib import Path
try:
    Path('oversized.bin').write_bytes(b'x' * (2 * 1024 * 1024))
    raise AssertionError('file limit was not enforced')
except OSError:
    pass
assert Path('oversized.bin').stat().st_size <= 1024 * 1024
print('file limit held')
"#,
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        fs::metadata(fixture.workspace.join("oversized.bin"))
            .unwrap()
            .len()
            <= 1024 * 1024
    );
}

#[test]
fn child_entry_rejects_malformed_or_overlapping_envelopes_before_project_code() {
    let fixture = Fixture::new();
    for (workspace, program, budget) in [
        (fixture.workspace.join("missing"), "python3", "4096"),
        (fixture.workspace.clone(), "../python3", "4096"),
        (fixture.workspace.clone(), "python3", "0"),
    ] {
        let result = Command::new(runtime_binary())
            .arg(CHILD_MODE)
            .arg(workspace)
            .arg(program)
            .arg(budget)
            .arg("--")
            .args(["-I", "-c", "print('must not execute')"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(126));
        assert!(result.stdout.is_empty());
    }
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn real_python_and_node_preserve_metadata_rename_append_truncate_and_subprocesses() {
    let fixture = Fixture::kernel();
    fixture.require_command_support();
    let python = fixture.python(
        r#"
import os, subprocess
from pathlib import Path
p = Path('file.txt')
p.write_text('first')
with p.open('a') as f: f.write(' second')
os.chmod(p, 0o640)
os.utime(p, (100, 100))
p.rename('renamed.txt')
with open('renamed.txt', 'r+') as f: f.truncate(5)
assert Path('renamed.txt').read_text() == 'first'
subprocess.run(['/usr/bin/sh', '-c', 'printf toolchain'], check=True)
print('python complete')
"#,
    );
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    assert_eq!(python.stdout, b"toolchainpython complete\n");
    let node = fixture.run(
        "node",
        &[
            "-e",
            r#"
const fs = require('fs');
const cp = require('child_process');
fs.writeFileSync('node.txt', 'first');
fs.appendFileSync('node.txt', ' second');
fs.chmodSync('node.txt', 0o640);
fs.utimesSync('node.txt', 100, 100);
fs.renameSync('node.txt', 'node-renamed.txt');
fs.truncateSync('node-renamed.txt', 5);
if (fs.readFileSync('node-renamed.txt', 'utf8') !== 'first') throw Error('content');
cp.execFileSync('/usr/bin/true');
console.log('node complete');
"#,
        ],
        false,
    );
    assert!(
        node.status.success(),
        "{}",
        String::from_utf8_lossy(&node.stderr)
    );
    assert_eq!(node.stdout, b"node complete\n");
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn genuine_exit_126_is_not_an_isolation_failure() {
    let fixture = Fixture::kernel();
    let output = fixture.python("print('tool ran'); raise SystemExit(126)");
    assert_eq!(output.status.code(), Some(126));
    assert_eq!(output.stdout, b"tool ran\n");
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn aggregate_workspace_budget_bounds_many_small_files_and_truncate_growth() {
    let fixture = Fixture::kernel();
    let budget = fixture.boundary().workspace_budget_bytes;
    let result = fixture.python(&format!(
        r#"
import errno
from pathlib import Path
budget = {budget}
part_size = 256 * 1024
assert budget >= 4 * part_size, 'fixture budget too small for aggregate proof'
denied = False
successful_bytes = 0
for i in range(budget // part_size + 2):
    try:
        Path('part-' + str(i)).write_bytes(b'x' * part_size)
        successful_bytes += part_size
    except OSError as error:
        assert error.errno == errno.EDQUOT, error
        denied = True
        break
assert denied, 'aggregate growth was not rejected'
assert successful_bytes >= budget - part_size, 'per-file limit cannot prove aggregate quota'
assert sum(p.stat().st_size for p in Path('.').iterdir() if p.is_file()) <= budget
for p in Path('.').glob('part-*'):
    p.unlink()
with open('sparse', 'wb') as f:
    f.truncate(part_size)
for i in range(budget // part_size - 1):
    Path('part-' + str(i)).write_bytes(b'x' * part_size)
try:
    with open('sparse', 'r+b') as f:
        f.truncate(part_size + 1)
    raise AssertionError('truncate bypassed aggregate reservation')
except OSError as error:
    assert error.errno == errno.EDQUOT, error
assert sum(p.stat().st_size for p in Path('.').iterdir() if p.is_file()) <= budget
print('aggregate workspace budget held')
"#
    ));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"aggregate workspace budget held\n");
}

#[test]
fn production_launch_refuses_missing_kernel_membership_and_workspace_budget() {
    let fixture = Fixture::new();
    for budget in [0, limits().file_bytes + 1, limits().file_bytes] {
        let boundary = CommandBoundary {
            cgroup_root: fixture.foreign.clone(),
            workspace_budget_bytes: budget,
        };
        assert!(launch_command(
            &runtime_binary(),
            &fixture.workspace,
            &[],
            "true",
            &limits(),
            &boundary
        )
        .is_err());
    }
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn production_executor_python_node_feedback_and_receipt_recovery() {
    for (program, source, stdout) in [
        ("python3", "from pathlib import Path\nPath('result.txt').write_text('python')\nprint('python observed')\nraise SystemExit(126)\n", "python observed"),
        ("node", "require('fs').writeFileSync('result.txt', 'node'); console.log('node observed'); process.exit(126);", "node observed"),
    ] {
        let fixture = Fixture::kernel();
        let request = fixture.request(program, source);
        let executor = fixture.executor();
        let result = executor.execute(request.clone(), Arc::new(AtomicBool::new(false)));
        match &result {
            WorkbenchMessage::Result { outcome, output, error, resources, .. } => {
                assert_eq!(*outcome, WorkbenchOutcome::Failed);
                assert_eq!(error.as_ref().unwrap().code, "command_failed");
                assert_eq!(output.get("exit_code").unwrap(), "126");
                assert_eq!(output.get("stdout").unwrap(), stdout);
                assert_eq!(output.get("bytes_written_basis").unwrap(), "cgroup_v2_io_stat_wbytes");
                assert!(resources.cpu_time_ms > 0);
                assert!(resources.peak_process_count > 0);
            }
            other => panic!("production result missing: {other:?}"),
        }
        executor.persist_completion_receipt(&result).unwrap();
        let replay = fixture.executor().recover_completion(&request.invocation_id, &request.input_digest).unwrap();
        assert!(matches!(replay, WorkbenchMessage::Result { outcome: WorkbenchOutcome::Failed, .. }));
        assert!(fs::read(fixture.workspace.join("result.txt")).is_ok());
    }
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn production_executor_quiesces_setsid_setpgid_and_inherited_output_fds() {
    for change_session in ["os.setsid()", "os.setpgid(0, 0)"] {
        let fixture = Fixture::kernel();
        let request = fixture.request(
            "python3",
            &format!(
                r#"
import os, time
from pathlib import Path
if os.fork() == 0:
    {change_session}
    if os.fork() == 0:
        time.sleep(0.5)
        Path('escaped.txt').write_text('must not survive')
        os._exit(0)
    os._exit(0)
print('primary completed', flush=True)
"#
            ),
        );
        let started = Instant::now();
        let result = fixture
            .executor()
            .execute(request, Arc::new(AtomicBool::new(false)));
        assert!(
            matches!(
                result,
                WorkbenchMessage::Result {
                    outcome: WorkbenchOutcome::Succeeded,
                    ..
                }
            ),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(600));
        assert!(!fixture.workspace.join("escaped.txt").exists());
    }
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn production_executor_cancellation_and_deadline_stop_escaped_descendants() {
    for cancel in [true, false] {
        let fixture = Fixture::kernel();
        let mut request = fixture.request(
            "python3",
            r#"
import os, time
from pathlib import Path
if os.fork() == 0:
    os.setsid()
    Path('ready.txt').write_text('ready')
    time.sleep(2)
    Path('escaped.txt').write_text('must not survive')
    os._exit(0)
time.sleep(10)
"#,
        );
        if !cancel {
            request.resource_limits.wall_time_ms = 800;
        }
        request.input_digest = request.canonical_digest().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancelled.clone();
        let ready = fixture.workspace.join("ready.txt");
        let cancellation = thread::spawn(move || {
            if cancel {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !ready.exists() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(
                    ready.exists(),
                    "cancellation must follow actual isolated execution"
                );
                cancel_flag.store(true, Ordering::Release);
            }
        });
        let result = fixture.executor().execute(request, cancelled);
        cancellation.join().unwrap();
        match result {
            WorkbenchMessage::Result {
                outcome, resources, ..
            } => {
                assert_eq!(
                    outcome,
                    if cancel {
                        WorkbenchOutcome::Cancelled
                    } else {
                        WorkbenchOutcome::TimedOut
                    }
                );
                assert!(resources.cpu_time_ms > 0);
            }
            other => panic!("terminal result missing: {other:?}"),
        }
        thread::sleep(Duration::from_millis(2100));
        assert!(!fixture.workspace.join("escaped.txt").exists());
    }
}

#[test]
#[ignore = "requires production outer bwrap, delegated cgroup v2 and budget-enforced FUSE workspace: mandatory deployment-host gate"]
fn production_executor_accounts_exited_children_and_kernel_memory_pid_limits() {
    for source in [
        "import os,time\nfor i in range(12):\n p=os.fork()\n if p==0:\n  t=time.process_time()\n  while time.process_time()-t<0.15: pass\n  os._exit(0)\n os.waitpid(p,0)\n",
        "import time\nx=bytearray(512*1024*1024)\ntime.sleep(1)\n",
        "import os,time\nfor i in range(64):\n if os.fork()==0: time.sleep(10); os._exit(0)\ntime.sleep(10)\n",
    ] {
        let fixture = Fixture::kernel();
        let mut request = fixture.request("python3", source);
        request.resource_limits.cpu_time_ms = 500;
        request.input_digest = request.canonical_digest().unwrap();
        let result = fixture.executor().execute(request, Arc::new(AtomicBool::new(false)));
        match result {
            WorkbenchMessage::Result { outcome, error, resources, .. } => {
                assert_eq!(outcome, WorkbenchOutcome::Failed);
                assert_eq!(error.unwrap().class, sentinel_common::WorkbenchErrorClass::Resource);
                assert!(resources.cpu_time_ms > 0);
            }
            other => panic!("resource result missing: {other:?}"),
        }
    }
}
