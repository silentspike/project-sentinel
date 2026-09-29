//! Bounded model-facing syntax, not an authority source or an execution plan.
//!
//! The caller must resolve a validated immutable profile and verify its digest
//! against the current authority before calling. This API has no profile digest
//! argument and cannot establish that binding itself. Dispatch still performs
//! normal authority, profile, command-policy, resource and artifact checks.

use sentinel_common::{CommandRule, TextReplacement, WorkbenchTool};
use sentinel_workflow::CompanyRoleV1;
use serde_json::{json, Value};

const MAX_CATALOG_BYTES: usize = 32 * 1024;
const MAX_METADATA_BYTES: usize = 8 * 1024;
const MAX_RULES: usize = 64;
const INVALID: &str = "adaptive tool catalogue contract is invalid";
const TOO_LARGE: &str = "adaptive tool catalogue exceeds its context bound";

pub(in crate::workflow_api) fn adaptive_tool_catalog(
    profile: &crate::workbench::WorkbenchProfile,
    authority: &sentinel_workflow::RuntimeAuthoritySnapshotV1,
    task: &sentinel_workflow::CompanyWorkItemSpecV1,
) -> Result<serde_json::Value, &'static str> {
    authority.validate().map_err(|_| INVALID)?;
    if profile.schema_version != 1
        || profile.id != authority.profile_id
        || profile.runtime_key != authority.runtime_key
        || profile.runtime_key != sentinel_common::WORKBENCH_RUNTIME_BWRAP
        || profile.network != "deny"
        || task.work_item_id != authority.work_item_id
        || task.owner != authority.agent_id
        || task.outputs.len() != 1
    {
        return Err(INVALID);
    }
    let output = &task.outputs[0];
    let kind = match task.required_role {
        CompanyRoleV1::Designer => "design_specification",
        CompanyRoleV1::Developer => "source_tree",
        CompanyRoleV1::Qa => "qa_report",
        _ => return Err(INVALID),
    };
    // Matches build_execution_plan/admit_adaptive_completion: output names are
    // not artifact kinds and a task's media is not interchangeable with another.
    if output.media_type.len() > 128
        || !identifier(&output.name)
        || output.digest_algorithm != "sha256"
        || output.contract_generation == 0
    {
        return Err(INVALID);
    }
    let package = WorkbenchTool::PackageArtifact {
        artifact_kind: kind.to_owned(),
        media_type: output.media_type.clone(),
        paths: vec!["example.txt".to_owned()],
    };
    package.validate_shape().map_err(|_| INVALID)?;

    let permitted = |tool: &WorkbenchTool| {
        let capability = tool.required_capability();
        authority.capabilities.contains(capability) && profile.capabilities.contains(capability)
    };
    let mut tools = Vec::new();
    let mut metadata_bytes = 0;
    let examples = [
        WorkbenchTool::ListDirectory {
            path: ".".to_owned(),
            after: None,
            max_entries: 64,
        },
        WorkbenchTool::InspectFile {
            path: "example.txt".to_owned(),
            max_bytes: sentinel_common::WORKBENCH_MAX_INSPECT_BYTES,
        },
        WorkbenchTool::WriteFile {
            path: if task.required_role == CompanyRoleV1::Qa {
                "review.json".to_owned()
            } else {
                "example.txt".to_owned()
            },
            content: "example text\n".to_owned(),
            expected_sha256: None,
        },
        WorkbenchTool::ApplyPatch {
            path: "example.txt".to_owned(),
            expected_sha256: "0".repeat(64),
            replacements: vec![TextReplacement {
                old: "example".to_owned(),
                new: "replacement".to_owned(),
                expected_occurrences: 1,
            }],
        },
        WorkbenchTool::RunCommand {
            program: "syntax-placeholder".to_owned(),
            args: Vec::new(),
        },
        WorkbenchTool::RunTests {
            suite_id: "syntax-placeholder".to_owned(),
            program: "syntax-placeholder".to_owned(),
            args: Vec::new(),
        },
        package,
    ];
    for example in examples {
        if !permitted(&example) {
            continue;
        }
        let mut entry = descriptor(&example);
        let mut syntax = Vec::new();
        match &example {
            WorkbenchTool::RunCommand { .. } => {
                check_rules(&profile.command_rules, &mut metadata_bytes)?;
                entry["command_rules"] = json!(profile.command_rules);
                for rule in &profile.command_rules {
                    syntax.push(encoded_example(&WorkbenchTool::RunCommand {
                        program: rule.program.clone(),
                        args: syntax_args(rule),
                    })?);
                }
            }
            WorkbenchTool::RunTests { .. } => {
                check_rules(&profile.command_rules, &mut metadata_bytes)?;
                if profile.test_suites.len() > MAX_RULES {
                    return Err(TOO_LARGE);
                }
                let mut suites = Vec::new();
                for suite in &profile.test_suites {
                    if !identifier(&suite.id) {
                        return Err(INVALID);
                    }
                    charge(&mut metadata_bytes, suite.id.len())?;
                    if suite.program.len() > 128
                        || suite.required_arg_prefix.len()
                            > sentinel_common::WORKBENCH_MAX_COMMAND_ARGUMENTS
                        || suite.required_arg_prefix.iter().any(|arg| arg.len() > 4096)
                    {
                        return Err(INVALID);
                    }
                    charge(&mut metadata_bytes, suite.program.len())?;
                    for arg in &suite.required_arg_prefix {
                        charge(&mut metadata_bytes, arg.len())?;
                    }
                    let rule = CommandRule {
                        program: suite.program.clone(),
                        required_arg_prefix: suite.required_arg_prefix.clone(),
                        max_args: suite.max_args,
                    };
                    rule.validate().map_err(|_| INVALID)?;
                    // Adaptive requests require both a suite and a matching
                    // command policy. Project their intersection, never a union.
                    let mut effective = Vec::new();
                    for command in &profile.command_rules {
                        if let Some(narrowed) = intersect_rules(&rule, command) {
                            let args = syntax_args(&narrowed);
                            syntax.push(encoded_example(&WorkbenchTool::RunTests {
                                suite_id: suite.id.clone(),
                                program: narrowed.program.clone(),
                                args,
                            })?);
                            effective.push(narrowed);
                        }
                    }
                    suites.push(json!({
                        "suite_id": suite.id,
                        "program": suite.program,
                        "required_arg_prefix": suite.required_arg_prefix,
                        "max_args": suite.max_args,
                        "effective_command_rules": effective,
                    }));
                }
                entry["suites"] = json!(suites);
            }
            WorkbenchTool::PackageArtifact { .. } => {
                if !profile.output_artifact_kinds.contains(kind)
                    || (task.required_role == CompanyRoleV1::Qa
                        && (profile.id != "web-review-v1"
                            || output.media_type != "application/vnd.sentinel.qa-report+json"))
                {
                    return Err(INVALID);
                }
                entry["output_contract"] = json!({
                    "output_name": output.name,
                    "artifact_kind": kind,
                    "media_type": output.media_type,
                    "digest_algorithm": output.digest_algorithm,
                    "paths": "Nonempty list of existing workspace files/directories; canonical relative paths, unique and non-overlapping. Neither '.' nor host paths are allowed. Task outputs declare no fixed paths.",
                });
                if task.required_role == CompanyRoleV1::Qa {
                    entry["output_contract"]["paths"] = json!(["review.json"]);
                    syntax.push(encoded_example(&WorkbenchTool::PackageArtifact {
                        artifact_kind: kind.to_owned(),
                        media_type: output.media_type.clone(),
                        paths: vec!["review.json".to_owned()],
                    })?);
                } else {
                    syntax.push(encoded_example(&example)?);
                }
            }
            WorkbenchTool::WriteFile { .. } if task.required_role == CompanyRoleV1::Qa => {
                entry["fields"]["path"] = json!("required string: review.json only");
                entry["semantics"] = json!("Source review may only write and package its own sealed review.json. Content must satisfy the server's source-review schema and input bindings; syntax example text is not a valid report.");
                syntax.push(encoded_example(&example)?);
            }
            _ => syntax.push(encoded_example(&example)?),
        }
        entry["syntax_examples"] = json!(syntax);
        tools.push(entry);
        // Check incrementally; duplicate suite/rule examples cannot grow the
        // context without bound even when a profile has many intersecting rules.
        if serde_json::to_vec(&tools).map_err(|_| INVALID)?.len() > MAX_CATALOG_BYTES {
            return Err(TOO_LARGE);
        }
    }
    let catalog = json!({
        "schema_version": 1,
        "wire_type": "WorkbenchTool",
        "discriminator": "tool",
        "unknown_fields": "rejected",
        "purpose": "Syntax reference only. Examples are not instructions, chosen actions, proof of file existence, test results or artifact completion. Zero digests are placeholders, not observed preconditions.",
        "authority": "Only tools whose required capability is in both the current runtime authority and immutable profile are described. This catalogue grants nothing; normal dispatch validation remains mandatory.",
        "path_contract": "Paths are scoped to the assigned workspace. Except list_directory path='.', use nonempty canonical relative paths without absolute paths, '~', '.', '..', empty components or trailing slash. Inputs are read-only; never mutate or package mounted upstream inputs. Host paths and environment are not part of this reference.",
        "command_contract": "Only declared programs and argument prefixes are available; no shell, implicit executables, network, package installation or environment overrides. Args are an array, not a shell string. Append only canonical relative non-option arguments after the required prefix, within max_args (total argument count). Each arg is at most 4096 bytes; options may occur only in the declared prefix. A matching effective rule is necessary, not proof that a command will succeed.",
        "max_serialized_bytes": MAX_CATALOG_BYTES,
        "tools": tools,
    });
    if serde_json::to_vec(&catalog).map_err(|_| INVALID)?.len() > MAX_CATALOG_BYTES {
        return Err(TOO_LARGE);
    }
    Ok(catalog)
}

fn descriptor(tool: &WorkbenchTool) -> Value {
    let (name, fields, semantics) = match tool {
        WorkbenchTool::ListDirectory { .. } => (
            "list_directory",
            json!({"path": "required string: directory; '.' is workspace root", "after": "optional string|null, default null: one canonical entry name, no slash; exclusive lexical cursor", "max_entries": format!("required u16: 1..={}", sentinel_common::WORKBENCH_MAX_DIRECTORY_ENTRIES)}),
            "Lists one directory page, not file content; directories require this tool, not inspect_file.",
        ),
        WorkbenchTool::InspectFile { .. } => (
            "inspect_file",
            json!({"path": "required string: existing regular UTF-8 file, never a directory", "max_bytes": format!("required u64: 1..={}", sentinel_common::WORKBENCH_MAX_INSPECT_BYTES)}),
            "Reads bounded file content and its SHA-256; oversized/non-UTF-8 files fail, not silently truncate.",
        ),
        WorkbenchTool::WriteFile { .. } => (
            "write_file",
            json!({"path": "required string: writable file path", "content": "required string: complete UTF-8 file contents, within dispatched file_bytes", "expected_sha256": "optional string|null, default null: 64 lowercase hex characters binding existing contents"}),
            "Writes complete contents atomically. A null precondition does not bind prior contents; it is not an append or patch operation.",
        ),
        WorkbenchTool::ApplyPatch { .. } => (
            "apply_patch",
            json!({"path": "required string: existing writable UTF-8 file", "expected_sha256": "required string: observed 64-character lowercase hex SHA-256", "replacements": "required array: 1..=128 objects, exactly {old:string,new:string,expected_occurrences?:u32}; old nonempty, old/new at most 65536 bytes each, expected_occurrences defaults to 1 and must be positive"}),
            "Exact text replacements applied sequentially, each matching its occurrence count in the current intermediate text. Not unified diff syntax. Result stays within dispatched file_bytes.",
        ),
        WorkbenchTool::RunCommand { .. } => (
            "run_command",
            json!({"program": "required string: exact program from command_rules", "args": "optional array<string>, default []: matching required_arg_prefix plus bounded relative arguments"}),
            "Runs one allowlisted program/args tuple. Empty command_rules means no valid invocation.",
        ),
        WorkbenchTool::RunTests { .. } => (
            "run_tests",
            json!({"suite_id": "required string: exact declared suite_id", "program": "required string: that suite's program", "args": "optional array<string>, default []: must match one effective_command_rules entry for that suite"}),
            "Runs a profile test suite, not an arbitrary test command. Empty effective_command_rules means the suite is not dispatchable. Suite identity, program, prefix and total count are all bound.",
        ),
        WorkbenchTool::PackageArtifact { .. } => (
            "package_artifact",
            json!({"artifact_kind": "required string: exact output_contract.artifact_kind", "media_type": "required string: exact task output_contract.media_type", "paths": "required nonempty array<string>: existing workspace files/directories under output_contract.paths"}),
            "Packages local outputs, not a command or a claim of completion. Output name is not artifact kind. Use the returned observation for artifact identity; server completion admission checks the task contract.",
        ),
    };
    json!({"tool": name, "required_capability": tool.required_capability(), "fields": fields, "semantics": semantics})
}

fn encoded_example(tool: &WorkbenchTool) -> Result<Value, &'static str> {
    tool.validate_shape().map_err(|_| INVALID)?;
    serde_json::to_value(tool).map_err(|_| INVALID)
}

fn syntax_args(rule: &CommandRule) -> Vec<String> {
    let mut args = rule.required_arg_prefix.clone();
    if args.len() < usize::from(rule.max_args) {
        args.push("example.txt".to_owned());
    }
    args
}

fn intersect_rules(suite: &CommandRule, command: &CommandRule) -> Option<CommandRule> {
    if suite.program != command.program {
        return None;
    }
    let prefix = if suite
        .required_arg_prefix
        .starts_with(&command.required_arg_prefix)
    {
        &suite.required_arg_prefix
    } else if command
        .required_arg_prefix
        .starts_with(&suite.required_arg_prefix)
    {
        &command.required_arg_prefix
    } else {
        return None;
    };
    // The adapter's command rule always disallows appended options. Applying
    // the same narrowing to suites is conservative for non-coding profiles.
    if !suite.allows(&suite.program, prefix) || !command.allows(&suite.program, prefix) {
        return None;
    }
    Some(CommandRule {
        program: suite.program.clone(),
        required_arg_prefix: prefix.clone(),
        max_args: suite.max_args.min(command.max_args),
    })
}

fn check_rules(rules: &[CommandRule], bytes: &mut usize) -> Result<(), &'static str> {
    if rules.len() > MAX_RULES {
        return Err(TOO_LARGE);
    }
    for rule in rules {
        rule.validate().map_err(|_| INVALID)?;
        charge(bytes, rule.program.len())?;
        for arg in &rule.required_arg_prefix {
            charge(bytes, arg.len())?;
        }
    }
    Ok(())
}

fn charge(bytes: &mut usize, additional: usize) -> Result<(), &'static str> {
    *bytes = bytes.checked_add(additional).ok_or(TOO_LARGE)?;
    if *bytes > MAX_METADATA_BYTES {
        return Err(TOO_LARGE);
    }
    Ok(())
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::AgentId;
    use sentinel_workflow::{CompanyWorkItemSpecV1, RuntimeAuthoritySnapshotV1};
    use std::collections::BTreeSet;

    const PROFILES: [&str; 6] = [
        include_str!("../../../../../config/workbench-profiles/web-authoring-v1.toml"),
        include_str!("../../../../../config/workbench-profiles/python-coding-v1.toml"),
        include_str!("../../../../../config/workbench-profiles/node-coding-v1.toml"),
        include_str!("../../../../../config/workbench-profiles/web-qa-v1.toml"),
        include_str!("../../../../../config/workbench-profiles/coding-qa-v1.toml"),
        include_str!("../../../../../config/workbench-profiles/web-review-v1.toml"),
    ];

    fn fixture(
        source: &str,
    ) -> (
        crate::workbench::WorkbenchProfile,
        RuntimeAuthoritySnapshotV1,
        CompanyWorkItemSpecV1,
    ) {
        let profile: crate::workbench::WorkbenchProfile = toml::from_str(source).unwrap();
        let authority: RuntimeAuthoritySnapshotV1 = serde_json::from_value(json!({
            "schema_version": 1, "tenant_id": "tenant", "project_id": "project", "work_item_id": "work",
            "agent_id": 6, "assignment_version": 1, "assignment_digest": "a".repeat(64),
            "organization_generation": 1, "organization_digest": "b".repeat(64),
            "principal": {"schema_version": 1, "principal_id": "developer", "principal_generation": 1, "authority_digest": "c".repeat(64)},
            "profile_id": profile.id, "profile_generation": 1, "profile_digest": "d".repeat(64),
            "runtime_key": profile.runtime_key, "runtime_generation": 1, "runtime_digest": "e".repeat(64),
            "policy_generation": 1, "policy_digest": "f".repeat(64), "active": true, "capabilities": profile.capabilities,
        })).unwrap();
        let qa = profile.id.contains("qa") || profile.id == "web-review-v1";
        let task: CompanyWorkItemSpecV1 = serde_json::from_value(json!({
            "work_item_id": "work", "title": "Internal task", "objective": "Local output", "required_role": if qa { "qa" } else { "developer" },
            "required_specialties": ["local"], "dependency_ids": [], "owner": 6, "inputs": [],
            "outputs": [{"name": "result", "media_type": if qa { "application/vnd.sentinel.qa-report+json" } else { "application/vnd.sentinel.source-tree+json" }, "digest_algorithm": "sha256", "contract_generation": 1, "contract_digest": "1".repeat(64)}],
            "quality_gate": {"gate_id": "gate", "generation": 1, "digest": "2".repeat(64)}, "budget_micros": 1,
        })).unwrap();
        (profile, authority, task)
    }

    fn names(catalog: &Value) -> BTreeSet<&str> {
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["tool"].as_str().unwrap())
            .collect()
    }

    fn entry<'a>(catalog: &'a Value, name: &str) -> &'a Value {
        catalog["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["tool"] == name)
            .unwrap()
    }

    #[test]
    fn every_real_profile_example_round_trips_and_validates() {
        for source in PROFILES {
            let (profile, authority, task) = fixture(source);
            let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
            assert!(serde_json::to_vec(&catalog).unwrap().len() <= MAX_CATALOG_BYTES);
            for entry in catalog["tools"].as_array().unwrap() {
                for example in entry["syntax_examples"].as_array().unwrap() {
                    let tool: WorkbenchTool = serde_json::from_value(example.clone()).unwrap();
                    tool.validate_shape().unwrap();
                    assert_eq!(serde_json::to_value(&tool).unwrap(), *example);
                    assert!(profile.capabilities.contains(tool.required_capability()));
                    assert!(authority.capabilities.contains(tool.required_capability()));
                    match &tool {
                        WorkbenchTool::RunCommand { program, args } => assert!(profile
                            .command_rules
                            .iter()
                            .any(|r| r.allows(program, args))),
                        WorkbenchTool::RunTests {
                            suite_id,
                            program,
                            args,
                        } => {
                            assert!(profile
                                .command_rules
                                .iter()
                                .any(|r| r.allows(program, args)));
                            assert!(profile.test_suites.iter().any(|s| s.id == *suite_id
                                && s.program == *program
                                && args.starts_with(&s.required_arg_prefix)
                                && args.len() <= usize::from(s.max_args)));
                        }
                        WorkbenchTool::PackageArtifact {
                            artifact_kind,
                            media_type,
                            ..
                        } => {
                            assert!(profile.output_artifact_kinds.contains(artifact_kind));
                            assert_eq!(media_type, &task.outputs[0].media_type);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    #[test]
    fn all_seven_typed_tools_are_described_without_foreign_tools() {
        let (profile, mut authority, task) = fixture(PROFILES[0]);
        authority.capabilities.insert("internet.search".to_owned());
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        assert_eq!(
            names(&catalog),
            BTreeSet::from([
                "list_directory",
                "inspect_file",
                "write_file",
                "apply_patch",
                "run_command",
                "run_tests",
                "package_artifact"
            ])
        );
        assert_eq!(
            entry(&catalog, "package_artifact")["output_contract"]["artifact_kind"],
            "source_tree"
        );
        assert_ne!(
            entry(&catalog, "package_artifact")["output_contract"]["artifact_kind"],
            task.outputs[0].name
        );
    }

    #[test]
    fn capabilities_are_the_exact_intersection_in_both_directions() {
        let (mut profile, mut authority, task) = fixture(PROFILES[0]);
        authority.capabilities = BTreeSet::from([
            "file.inspect".to_owned(),
            "command.run_allowlisted".to_owned(),
            "foreign.tool".to_owned(),
        ]);
        profile.capabilities.remove("command.run_allowlisted");
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        assert_eq!(
            names(&catalog),
            BTreeSet::from(["list_directory", "inspect_file"])
        );
        profile.capabilities.clear();
        assert!(names(&adaptive_tool_catalog(&profile, &authority, &task).unwrap()).is_empty());
    }

    #[test]
    fn filtered_command_metadata_and_environment_never_leak() {
        let (mut profile, mut authority, mut task) = fixture(PROFILES[0]);
        profile
            .environment
            .insert("TOKEN".to_owned(), "secret-token-value".to_owned());
        profile
            .environment
            .insert("HOME".to_owned(), "/sensitive/host/path".to_owned());
        task.objective = "/sensitive/task/path secret-task-value".to_owned();
        profile.command_rules[0].program = "filtered-program".to_owned();
        authority.capabilities = BTreeSet::from(["file.inspect".to_owned()]);
        let encoded =
            serde_json::to_string(&adaptive_tool_catalog(&profile, &authority, &task).unwrap())
                .unwrap();
        for forbidden in [
            "secret-token-value",
            "/sensitive/host/path",
            "secret-task-value",
            "filtered-program",
            &authority.principal.authority_digest,
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn foreign_profile_runtime_task_owner_and_inactive_authority_fail() {
        let (profile, authority, task) = fixture(PROFILES[0]);
        let mut changed = authority.clone();
        changed.profile_id = "foreign-profile".to_owned();
        assert!(adaptive_tool_catalog(&profile, &changed, &task).is_err());
        changed = authority.clone();
        changed.runtime_key = "foreign-runtime".to_owned();
        assert!(adaptive_tool_catalog(&profile, &changed, &task).is_err());
        changed = authority.clone();
        changed.active = false;
        assert!(adaptive_tool_catalog(&profile, &changed, &task).is_err());
        let mut changed_task = task.clone();
        changed_task.work_item_id.0 = "foreign-work".to_owned();
        assert!(adaptive_tool_catalog(&profile, &authority, &changed_task).is_err());
        changed_task = task.clone();
        changed_task.owner = AgentId(7);
        assert!(adaptive_tool_catalog(&profile, &authority, &changed_task).is_err());
        let mut changed_profile = profile.clone();
        changed_profile.runtime_key = "foreign-runtime".to_owned();
        changed = authority.clone();
        changed.runtime_key = changed_profile.runtime_key.clone();
        assert!(adaptive_tool_catalog(&changed_profile, &changed, &task).is_err());
    }

    #[test]
    fn artifact_kind_and_media_follow_the_task_not_the_profile_inventory() {
        let (mut profile, authority, mut task) = fixture(PROFILES[0]);
        task.required_role = CompanyRoleV1::Designer;
        task.outputs[0].media_type = "application/vnd.sentinel.design+json".to_owned();
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        let contract = &entry(&catalog, "package_artifact")["output_contract"];
        assert_eq!(contract["artifact_kind"], "design_specification");
        assert_eq!(contract["media_type"], task.outputs[0].media_type);
        assert_ne!(
            contract["media_type"],
            "application/vnd.sentinel.source-tree+json"
        );
        profile.output_artifact_kinds.remove("design_specification");
        assert!(adaptive_tool_catalog(&profile, &authority, &task).is_err());
        profile
            .output_artifact_kinds
            .insert("design_specification".to_owned());
        task.outputs[0].media_type = "/sensitive/path with spaces".to_owned();
        assert!(adaptive_tool_catalog(&profile, &authority, &task).is_err());
        task.outputs.clear();
        assert!(adaptive_tool_catalog(&profile, &authority, &task).is_err());
    }

    #[test]
    fn foreign_task_media_is_not_a_declared_package_choice() {
        let (profile, authority, task) = fixture(PROFILES[1]);
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        let mut foreign = entry(&catalog, "package_artifact")["syntax_examples"][0].clone();
        foreign["media_type"] = json!("application/octet-stream");
        let tool: WorkbenchTool = serde_json::from_value(foreign.clone()).unwrap();
        // Serde/shape validity is not task authorization. Completion admission
        // must still reject the foreign media, as adaptive_package_tool does.
        tool.validate_shape().unwrap();
        assert_ne!(
            foreign["media_type"],
            entry(&catalog, "package_artifact")["output_contract"]["media_type"]
        );
        assert!(!entry(&catalog, "package_artifact")["syntax_examples"]
            .as_array()
            .unwrap()
            .contains(&foreign));
    }

    #[test]
    fn report_only_profile_preserves_its_path_contract() {
        let (profile, authority, mut task) = fixture(PROFILES[5]);
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        assert_eq!(
            names(&catalog),
            BTreeSet::from([
                "list_directory",
                "inspect_file",
                "write_file",
                "package_artifact"
            ])
        );
        assert_eq!(
            entry(&catalog, "write_file")["syntax_examples"][0]["path"],
            "review.json"
        );
        assert_eq!(
            entry(&catalog, "package_artifact")["output_contract"]["paths"],
            json!(["review.json"])
        );
        task.outputs[0].media_type = "application/json".to_owned();
        assert!(adaptive_tool_catalog(&profile, &authority, &task).is_err());
    }

    #[test]
    fn serde_defaults_and_text_replacement_fields_match_the_reference() {
        let list: WorkbenchTool = serde_json::from_value(
            json!({"tool": "list_directory", "path": ".", "max_entries": 1}),
        )
        .unwrap();
        assert!(matches!(
            list,
            WorkbenchTool::ListDirectory { after: None, .. }
        ));
        let write: WorkbenchTool = serde_json::from_value(
            json!({"tool": "write_file", "path": "example.txt", "content": ""}),
        )
        .unwrap();
        assert!(matches!(
            write,
            WorkbenchTool::WriteFile {
                expected_sha256: None,
                ..
            }
        ));
        let command: WorkbenchTool =
            serde_json::from_value(json!({"tool": "run_command", "program": "node"})).unwrap();
        assert!(command.command().unwrap().1.is_empty());
        let test: WorkbenchTool = serde_json::from_value(
            json!({"tool": "run_tests", "suite_id": "suite", "program": "node"}),
        )
        .unwrap();
        assert!(test.command().unwrap().1.is_empty());
        let replacement: TextReplacement =
            serde_json::from_value(json!({"old": "old", "new": "new"})).unwrap();
        assert_eq!(replacement.expected_occurrences, 1);
        assert!(serde_json::from_value::<TextReplacement>(
            json!({"old": "old", "new": "new", "foreign": 1})
        )
        .is_err());
    }

    #[test]
    fn unsafe_profile_programs_and_argument_prefixes_fail_without_exposure() {
        let (mut profile, authority, task) = fixture(PROFILES[0]);
        profile.command_rules[0].program = "/sensitive/host/program".to_owned();
        assert_eq!(
            adaptive_tool_catalog(&profile, &authority, &task).unwrap_err(),
            INVALID
        );
        let (mut profile, authority, task) = fixture(PROFILES[0]);
        profile.test_suites[0].required_arg_prefix = vec!["../escape".to_owned()];
        assert_eq!(
            adaptive_tool_catalog(&profile, &authority, &task).unwrap_err(),
            INVALID
        );
    }

    #[test]
    fn test_suites_cannot_smuggle_foreign_programs_or_broaden_command_args() {
        let (mut profile, authority, task) = fixture(PROFILES[1]);
        profile.test_suites[0].program = "foreign-executable".to_owned();
        let catalog = adaptive_tool_catalog(&profile, &authority, &task).unwrap();
        let suites = entry(&catalog, "run_tests")["suites"].as_array().unwrap();
        assert!(suites[0]["effective_command_rules"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(entry(&catalog, "run_tests")["syntax_examples"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["program"] != "foreign-executable"));
        let command = CommandRule {
            program: "node".to_owned(),
            required_arg_prefix: vec!["--".to_owned()],
            max_args: 2,
        };
        let suite = CommandRule {
            program: "node".to_owned(),
            required_arg_prefix: vec!["--".to_owned()],
            max_args: 32,
        };
        let effective = intersect_rules(&suite, &command).unwrap();
        assert_eq!(effective.max_args, 2);
        assert!(!effective.allows("node", &["--".to_owned(), "--foreign-option".to_owned()]));
    }

    #[test]
    fn serde_rejects_foreign_tools_fields_and_wrong_run_shapes() {
        for value in [
            json!({"tool": "shell", "command": "node"}),
            json!({"tool": "run_command", "program": "node", "args": "--test"}),
            json!({"tool": "run_tests", "program": "node", "args": []}),
            json!({"tool": "package_artifact", "artifact_kind": "source_tree", "paths": ["example.txt"]}),
            json!({"tool": "inspect_file", "path": "example.txt", "max_bytes": 32, "environment": {}}),
        ] {
            assert!(serde_json::from_value::<WorkbenchTool>(value).is_err());
        }
    }

    #[test]
    fn real_shape_validation_rejects_directory_inspect_escape_patch_and_overlap() {
        for value in [
            json!({"tool": "inspect_file", "path": ".", "max_bytes": 32}),
            json!({"tool": "write_file", "path": "../escape", "content": "text"}),
            json!({"tool": "run_command", "program": "/usr/bin/node", "args": []}),
            json!({"tool": "run_command", "program": "node", "args": ["/host/file"]}),
            json!({"tool": "apply_patch", "path": "example.txt", "expected_sha256": "0".repeat(64), "replacements": [{"old": "text", "new": "other", "expected_occurrences": 0}]}),
            json!({"tool": "package_artifact", "artifact_kind": "source_tree", "media_type": "application/json", "paths": ["src", "src/main.js"]}),
        ] {
            let tool: WorkbenchTool = serde_json::from_value(value).unwrap();
            assert!(tool.validate_shape().is_err());
        }
    }

    #[test]
    fn oversized_rule_count_metadata_and_serialized_catalog_fail_closed() {
        let (mut profile, authority, task) = fixture(PROFILES[0]);
        let rule = profile.command_rules[0].clone();
        profile.command_rules = vec![rule; MAX_RULES + 1];
        assert_eq!(
            adaptive_tool_catalog(&profile, &authority, &task).unwrap_err(),
            TOO_LARGE
        );
        profile.command_rules = vec![CommandRule {
            program: "node".to_owned(),
            required_arg_prefix: vec!["a".repeat(4096), "b".repeat(4096)],
            max_args: 2,
        }];
        assert_eq!(
            adaptive_tool_catalog(&profile, &authority, &task).unwrap_err(),
            TOO_LARGE
        );
        let (mut profile, authority, task) = fixture(PROFILES[0]);
        profile.command_rules = vec![
            CommandRule {
                program: "node".to_owned(),
                required_arg_prefix: Vec::new(),
                max_args: 2
            };
            MAX_RULES
        ];
        profile.test_suites = vec![
            crate::workbench::WorkbenchTestSuite {
                id: "bounded-suite".to_owned(),
                program: "node".to_owned(),
                required_arg_prefix: Vec::new(),
                max_args: 2
            };
            MAX_RULES
        ];
        assert_eq!(
            adaptive_tool_catalog(&profile, &authority, &task).unwrap_err(),
            TOO_LARGE
        );
    }
}
