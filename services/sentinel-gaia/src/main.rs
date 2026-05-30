use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use sentinel_gaia::{
    generate, read_spec, validate_output_dir, CompanyType, DepartmentSpec, GaiaSpec,
    GeneratedCompany, ShiftModel, GAIA_SPEC_FILENAME,
};
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(
    name = "sentinel-gaia",
    version,
    about = "Deterministic Sentinel company-config generator"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Collect a company spec, generate config files, and optionally run daemon checks.
    Init(InitArgs),
    /// Show the generation plan without writing files.
    Preview(SpecArgs),
    /// Validate a generated config directory.
    Validate(ValidateArgs),
    /// Print an example Gaia spec TOML.
    PrintExampleSpec,
}

#[derive(Args, Debug)]
struct InitArgs {
    /// Read the Gaia spec from a TOML file instead of prompting.
    #[arg(long)]
    spec: Option<PathBuf>,

    /// Directory that receives gaia-spec.toml, agents/, rooms.toml, daemon.toml, and nightrun.toml.
    #[arg(long, default_value = "config")]
    output_dir: PathBuf,

    /// Skip the final confirmation prompt.
    #[arg(long)]
    yes: bool,

    /// Allow overwriting an existing non-empty output directory after creating a backup.
    #[arg(long)]
    force: bool,

    /// Run sentinel-daemon --dry-run against the generated config after writing.
    #[arg(long)]
    daemon_dry_run: bool,

    /// Start sentinel-daemon after writing. This spawns the process and returns immediately.
    #[arg(long)]
    start_daemon: bool,

    /// Daemon binary used by --daemon-dry-run and --start-daemon.
    #[arg(long, default_value = "sentinel-daemon")]
    daemon_bin: PathBuf,

    /// Print a machine-readable JSON summary instead of the human summary.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct SpecArgs {
    /// Gaia spec TOML path.
    #[arg(long, default_value = GAIA_SPEC_FILENAME)]
    spec: PathBuf,
}

#[derive(Args, Debug)]
struct ValidateArgs {
    /// Directory containing gaia-spec.toml, agents/, rooms.toml, daemon.toml, and nightrun.toml.
    #[arg(long, default_value = "config")]
    output_dir: PathBuf,
}

#[derive(Debug, Serialize)]
struct InitSummary {
    company_name: String,
    output_dir: PathBuf,
    agents: usize,
    rooms: usize,
    files_written: Vec<PathBuf>,
    backup_dir: Option<PathBuf>,
    daemon_dry_run: bool,
    daemon_started: bool,
}

#[derive(Debug)]
struct WriteOutcome {
    files_written: Vec<PathBuf>,
    backup_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init(args) => init(args),
        Commands::Preview(args) => preview(args),
        Commands::Validate(args) => validate(args),
        Commands::PrintExampleSpec => print_example_spec(),
    }
}

fn init(args: InitArgs) -> Result<()> {
    let spec = match &args.spec {
        Some(path) => read_spec(path)?,
        None => prompt_spec()?,
    };
    let generated = generate(spec)?;
    let report = generated.validate()?;
    print_preview(&generated, &report);

    if !args.yes && !confirm("Write these files? [y/N] ")? {
        bail!("aborted by user");
    }

    let outcome = write_generated(&generated, &args.output_dir, args.force)?;

    if args.daemon_dry_run {
        run_daemon_dry_run(&args.daemon_bin, &args.output_dir)?;
    }

    let daemon_started = if args.start_daemon {
        start_daemon(&args.daemon_bin, &args.output_dir)?
    } else {
        false
    };

    let summary = InitSummary {
        company_name: generated.spec.company_name.clone(),
        output_dir: args.output_dir,
        agents: report.agents,
        rooms: report.rooms,
        files_written: outcome.files_written,
        backup_dir: outcome.backup_dir,
        daemon_dry_run: args.daemon_dry_run,
        daemon_started,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_init_summary(&summary);
    }

    Ok(())
}

fn preview(args: SpecArgs) -> Result<()> {
    let spec = read_spec(&args.spec)?;
    let generated = generate(spec)?;
    let report = generated.validate()?;
    print_preview(&generated, &report);
    Ok(())
}

fn validate(args: ValidateArgs) -> Result<()> {
    let report = validate_output_dir(&args.output_dir)?;
    println!(
        "OK: {} agents, {} rooms, total room capacity {}, daemon.max_agents {}, nightrun.max_agent_id {}",
        report.agents,
        report.rooms,
        report.total_room_capacity,
        report.daemon_max_agents,
        report.nightrun_max_agent_id
    );
    Ok(())
}

fn print_example_spec() -> Result<()> {
    println!("{}", toml::to_string_pretty(&GaiaSpec::example())?);
    Ok(())
}

fn print_preview(generated: &GeneratedCompany, report: &sentinel_gaia::ValidationReport) {
    println!("Company: {}", generated.spec.company_name);
    println!("Type: {:?}", generated.spec.company_type);
    println!("Agents: {}", report.agents);
    println!("Rooms: {}", report.rooms);
    println!("Files:");
    for file in &generated.files {
        println!("  {}", file.relative_path.display());
    }
}

fn print_init_summary(summary: &InitSummary) {
    println!("Written config for {}", summary.company_name);
    println!("Output: {}", summary.output_dir.display());
    println!("Agents: {}", summary.agents);
    println!("Rooms: {}", summary.rooms);
    println!("Files written: {}", summary.files_written.len());
    if let Some(backup_dir) = &summary.backup_dir {
        println!("Backup: {}", backup_dir.display());
    }
    if summary.daemon_dry_run {
        println!("Daemon dry-run: ok");
    }
    if summary.daemon_started {
        println!("Daemon started: yes");
    }
}

fn prompt_spec() -> Result<GaiaSpec> {
    let company_name = prompt("Company name", "Gaia Demo GmbH")?;
    let company_type = parse_company_type(&prompt(
        "Company type [software_agency|manufacturing|healthcare|generic]",
        "software_agency",
    )?)?;
    let city = prompt("City", "Nuernberg")?;
    let address = prompt("Address", "Fuerther Strasse 42, 90429 Nuernberg")?;
    let agent_count = parse_u16(&prompt("Agent count", "75")?, "agent_count")?;
    let seed = parse_u64(&prompt("Seed", "42")?, "seed")?;
    let shift_model = parse_shift_model(&prompt(
        "Shift model [office_hours|three_shift|hybrid]",
        "hybrid",
    )?)?;
    let departments = parse_departments(&prompt(
        "Departments comma-separated (empty = template)",
        "",
    )?);

    Ok(GaiaSpec {
        company_name,
        company_type,
        city,
        address,
        agent_count,
        seed,
        shift_model,
        time_scale: 1.0,
        departments,
    })
}

fn prompt(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    io::stdout().flush()?;

    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    if value.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(value.to_string())
    }
}

fn confirm(label: &str) -> Result<bool> {
    print!("{label}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "j" | "ja"
    ))
}

fn parse_company_type(value: &str) -> Result<CompanyType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "software_agency" | "software" | "agency" => Ok(CompanyType::SoftwareAgency),
        "manufacturing" | "factory" => Ok(CompanyType::Manufacturing),
        "healthcare" | "health" => Ok(CompanyType::Healthcare),
        "generic" => Ok(CompanyType::Generic),
        other => bail!("unknown company type '{other}'"),
    }
}

fn parse_shift_model(value: &str) -> Result<ShiftModel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "office_hours" | "office" => Ok(ShiftModel::OfficeHours),
        "three_shift" | "three" | "3" => Ok(ShiftModel::ThreeShift),
        "hybrid" => Ok(ShiftModel::Hybrid),
        other => bail!("unknown shift model '{other}'"),
    }
}

fn parse_departments(value: &str) -> Vec<DepartmentSpec> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|name| DepartmentSpec {
            name: name.to_string(),
            weight: 1,
            roles: Vec::new(),
        })
        .collect()
}

fn parse_u16(value: &str, field: &str) -> Result<u16> {
    value
        .trim()
        .parse::<u16>()
        .with_context(|| format!("parse {field} as u16"))
}

fn parse_u64(value: &str, field: &str) -> Result<u64> {
    value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("parse {field} as u64"))
}

fn write_generated(
    generated: &GeneratedCompany,
    output_dir: &Path,
    force: bool,
) -> Result<WriteOutcome> {
    let needs_backup = output_dir.exists() && !is_empty_dir(output_dir)?;
    if needs_backup && !force {
        bail!(
            "{} already exists and is not empty; rerun with --force to create a backup and overwrite generated files",
            output_dir.display()
        );
    }

    let backup_dir = if needs_backup {
        let backup_dir = backup_dir_for(output_dir)?;
        copy_dir_all(output_dir, &backup_dir).with_context(|| {
            format!(
                "backup {} to {}",
                output_dir.display(),
                backup_dir.display()
            )
        })?;
        Some(backup_dir)
    } else {
        None
    };

    let report = generated.write_to_dir(output_dir, force)?;
    Ok(WriteOutcome {
        files_written: report.files_written,
        backup_dir,
    })
}

fn is_empty_dir(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        bail!("{} exists but is not a directory", path.display());
    }
    Ok(fs::read_dir(path)?.next().is_none())
}

fn backup_dir_for(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("cannot create backup name for {}", path.display()))?
        .to_string_lossy();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs();

    for suffix in 0..1000u16 {
        let candidate = if suffix == 0 {
            parent.join(format!("{name}.backup-{timestamp}"))
        } else {
            parent.join(format!("{name}.backup-{timestamp}-{suffix}"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!("could not allocate backup directory for {}", path.display())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn run_daemon_dry_run(daemon_bin: &Path, output_dir: &Path) -> Result<()> {
    let daemon_config = output_dir.join("daemon.toml");
    let working_dir = daemon_working_dir(output_dir)?;
    let status = Command::new(daemon_bin)
        .arg("--config")
        .arg(&daemon_config)
        .arg("--dry-run")
        .current_dir(&working_dir)
        .status()
        .with_context(|| format!("run daemon dry-run via {}", daemon_bin.display()))?;

    if !status.success() {
        bail!("daemon dry-run failed with status {status}");
    }
    Ok(())
}

fn start_daemon(daemon_bin: &Path, output_dir: &Path) -> Result<bool> {
    let daemon_config = output_dir.join("daemon.toml");
    let working_dir = daemon_working_dir(output_dir)?;
    let child = Command::new(daemon_bin)
        .arg("--config")
        .arg(&daemon_config)
        .current_dir(&working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("start daemon via {}", daemon_bin.display()))?;
    println!("sentinel-daemon pid {}", child.id());
    Ok(true)
}

fn daemon_working_dir(output_dir: &Path) -> Result<PathBuf> {
    if output_dir.file_name().is_some_and(|name| name == "config") {
        Ok(output_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf())
    } else {
        bail!(
            "daemon integration expects --output-dir to be named 'config' because generated daemon.toml uses config_dir = \"config\""
        )
    }
}
