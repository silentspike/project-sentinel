use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sentinel_gaia::{CompanyType, GaiaSpec, ShiftModel};

fn gaia_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sentinel-gaia"))
}

fn write_spec(root: &Path, agent_count: u16) -> PathBuf {
    let spec = GaiaSpec {
        company_name: "CLI Test GmbH".to_string(),
        company_type: CompanyType::SoftwareAgency,
        city: "Nuernberg".to_string(),
        address: "Teststrasse 1".to_string(),
        agent_count,
        seed: 123,
        shift_model: ShiftModel::Hybrid,
        time_scale: 1.0,
        departments: Vec::new(),
    };
    let path = root.join("spec.toml");
    fs::write(&path, toml::to_string_pretty(&spec).unwrap()).unwrap();
    path
}

#[test]
fn init_from_spec_writes_valid_configs_and_protects_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let spec = write_spec(temp.path(), 12);
    let output_dir = temp.path().join("config");

    let init = Command::new(gaia_bin())
        .arg("init")
        .arg("--spec")
        .arg(&spec)
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--yes")
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(String::from_utf8_lossy(&init.stdout).contains("\"agents\": 12"));
    assert!(output_dir.join("gaia-spec.toml").exists());
    assert_eq!(fs::read_dir(output_dir.join("agents")).unwrap().count(), 12);
    assert!(!output_dir.join("company.toml").exists());

    let validate = Command::new(gaia_bin())
        .arg("validate")
        .arg("--output-dir")
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        validate.status.success(),
        "{}",
        String::from_utf8_lossy(&validate.stderr)
    );
    assert!(String::from_utf8_lossy(&validate.stdout).contains("OK: 12 agents"));

    let refused = Command::new(gaia_bin())
        .arg("init")
        .arg("--spec")
        .arg(&spec)
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--yes")
        .output()
        .unwrap();
    assert!(!refused.status.success());

    let overwritten = Command::new(gaia_bin())
        .arg("init")
        .arg("--spec")
        .arg(&spec)
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--yes")
        .arg("--force")
        .output()
        .unwrap();
    assert!(
        overwritten.status.success(),
        "{}",
        String::from_utf8_lossy(&overwritten.stderr)
    );
    assert!(fs::read_dir(temp.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("config.backup-")));
}

#[test]
fn init_interactive_scripted_input_collects_spec() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("config");
    let mut child = Command::new(gaia_bin())
        .arg("init")
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--yes")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            b"Scripted GmbH\nsoftware_agency\nWien\nOperngasse 1\n8\n9\nthree_shift\nOps, Finance\n",
        )
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let spec = fs::read_to_string(output_dir.join("gaia-spec.toml")).unwrap();
    assert!(spec.contains("Scripted GmbH"));
    assert!(spec.contains("three_shift"));
    assert!(spec.contains("Ops"));
}

#[test]
fn init_can_run_daemon_dry_run_with_config_output_dir() {
    let temp = tempfile::tempdir().unwrap();
    let spec = write_spec(temp.path(), 6);
    let root = temp.path().join("company-root");
    let output_dir = root.join("config");
    fs::create_dir_all(&root).unwrap();

    let call_file = temp.path().join("daemon-call.txt");
    let daemon_bin = temp.path().join("fake-daemon.sh");
    fs::write(
        &daemon_bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 0\n",
            call_file.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&daemon_bin).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&daemon_bin, perms).unwrap();
    }

    let output = Command::new(gaia_bin())
        .arg("init")
        .arg("--spec")
        .arg(&spec)
        .arg("--output-dir")
        .arg(&output_dir)
        .arg("--yes")
        .arg("--daemon-dry-run")
        .arg("--daemon-bin")
        .arg(&daemon_bin)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let call = fs::read_to_string(call_file).unwrap();
    assert!(call.contains("--config"));
    assert!(call.contains("--dry-run"));
    assert!(call.contains("daemon.toml"));
}
