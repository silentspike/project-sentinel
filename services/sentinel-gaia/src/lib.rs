//! Deterministic Gaia config generator.
//!
//! Gaia turns a company specification into the Sentinel runtime configuration
//! set without LLM calls or ambient randomness.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sentinel_common::agent_config::{AgentConfig, AgentConfigValidation};
use sentinel_common::room::BuildingConfig;
use sentinel_common::{AgentId, RUNTIME_ECS_NATIVE};
use serde::{Deserialize, Serialize};

pub const GAIA_SPEC_FILENAME: &str = "gaia-spec.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompanyType {
    #[default]
    SoftwareAgency,
    Manufacturing,
    Healthcare,
    Generic,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShiftModel {
    OfficeHours,
    ThreeShift,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DepartmentSpec {
    pub name: String,
    #[serde(default = "default_department_weight")]
    pub weight: u16,
    #[serde(default)]
    pub roles: Vec<String>,
}

fn default_department_weight() -> u16 {
    1
}

/// Strukturierte Kultur-/Sozial-Dimension der Firma (#441). Steuert deterministisch (blake3,
/// kein LLM) die Big-Five-Verteilung der generierten Agents und fliesst in die `company-context.md`.
/// Alle Achsen in [0.0, 1.0]; 0.5 = neutral. `mission`/`values` sind die Prosa-Felder, die Gaia
/// (Claude Code) im Interview befuellt — leer => aus `company_type` abgeleitet (siehe Render).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CultureSpec {
    /// Formalitaet (0 = locker/flach, 1 = formell/hierarchisch) → praegt Conscientiousness.
    #[serde(default = "default_culture_axis")]
    pub formality: f32,
    /// Zusammenarbeit (0 = einzelkaempferisch, 1 = teamzentriert) → praegt Agreeableness.
    #[serde(default = "default_culture_axis")]
    pub collaboration: f32,
    /// Konfliktniveau (0 = ausgeglichen, 1 = reibungsintensiv) → praegt Neuroticism-Streuung.
    #[serde(default = "default_culture_axis")]
    pub conflict_level: f32,
    /// Innovationsgrad (0 = bewahrend, 1 = experimentierfreudig) → praegt Openness.
    #[serde(default = "default_culture_axis")]
    pub innovation: f32,
    /// Diversitaet (0 = homogen, 1 = heterogen) → globale Verteilungsstreuung.
    #[serde(default = "default_culture_axis")]
    pub diversity: f32,
    /// Firmenmission (Prosa, von Gaia befuellt). Leer => aus `company_type` abgeleitet.
    #[serde(default)]
    pub mission: String,
    /// Firmenwerte (Prosa, von Gaia befuellt). Leer => aus `company_type` abgeleitet.
    #[serde(default)]
    pub values: Vec<String>,
}

fn default_culture_axis() -> f32 {
    0.5
}

impl Default for CultureSpec {
    fn default() -> Self {
        Self {
            formality: default_culture_axis(),
            collaboration: default_culture_axis(),
            conflict_level: default_culture_axis(),
            innovation: default_culture_axis(),
            diversity: default_culture_axis(),
            mission: String::new(),
            values: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GaiaSpec {
    pub company_name: String,
    #[serde(default)]
    pub company_type: CompanyType,
    #[serde(default = "default_city")]
    pub city: String,
    #[serde(default = "default_address")]
    pub address: String,
    pub agent_count: u16,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default)]
    pub shift_model: ShiftModel,
    #[serde(default = "default_time_scale")]
    pub time_scale: f32,
    #[serde(default)]
    pub departments: Vec<DepartmentSpec>,
    #[serde(default)]
    pub culture: CultureSpec,
}

impl GaiaSpec {
    pub fn example() -> Self {
        Self {
            company_name: "Gaia Demo GmbH".to_string(),
            company_type: CompanyType::SoftwareAgency,
            city: default_city(),
            address: default_address(),
            agent_count: 75,
            seed: default_seed(),
            shift_model: ShiftModel::Hybrid,
            time_scale: default_time_scale(),
            departments: Vec::new(),
            culture: CultureSpec {
                formality: 0.4,
                collaboration: 0.7,
                conflict_level: 0.3,
                innovation: 0.8,
                diversity: 0.6,
                mission: "Digitale Produkte mit Handwerks-Qualitaet bauen.".to_string(),
                values: vec![
                    "Qualitaet vor Tempo".to_string(),
                    "Transparente Zusammenarbeit".to_string(),
                    "Kontinuierliches Lernen".to_string(),
                ],
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.company_name.trim().is_empty() {
            bail!("company_name must not be empty");
        }
        if self.agent_count == 0 {
            bail!("agent_count must be at least 1");
        }
        if self.time_scale <= 0.0 {
            bail!("time_scale must be > 0.0");
        }
        for department in &self.departments {
            if department.name.trim().is_empty() {
                bail!("department name must not be empty");
            }
            if department.weight == 0 {
                bail!("department '{}' weight must be > 0", department.name);
            }
        }
        for (name, value) in [
            ("formality", self.culture.formality),
            ("collaboration", self.culture.collaboration),
            ("conflict_level", self.culture.conflict_level),
            ("innovation", self.culture.innovation),
            ("diversity", self.culture.diversity),
        ] {
            if !(0.0..=1.0).contains(&value) {
                bail!("culture.{name} must be in [0.0, 1.0], got {value}");
            }
        }
        Ok(())
    }

    fn effective_departments(&self) -> Vec<DepartmentSpec> {
        if !self.departments.is_empty() {
            return self.departments.clone();
        }

        match self.company_type {
            CompanyType::SoftwareAgency => vec![
                dept("Geschaeftsfuehrung", 1, &["CEO", "COO"]),
                dept(
                    "Entwicklung",
                    5,
                    &["Backend Engineer", "Frontend Engineer", "DevOps Engineer"],
                ),
                dept(
                    "Design",
                    3,
                    &["Product Designer", "UX Researcher", "Visual Designer"],
                ),
                dept(
                    "Projektmanagement",
                    2,
                    &["Project Manager", "Delivery Manager"],
                ),
                dept("Vertrieb", 2, &["Account Executive", "Sales Manager"]),
                dept("Marketing", 1, &["Marketing Manager", "Content Strategist"]),
                dept(
                    "Qualitaetssicherung",
                    2,
                    &["QA Engineer", "Test Automation Engineer"],
                ),
                dept("IT", 1, &["IT Administrator", "Security Engineer"]),
                dept("Verwaltung", 1, &["Office Manager", "HR Coordinator"]),
            ],
            CompanyType::Manufacturing => vec![
                dept(
                    "Geschaeftsfuehrung",
                    1,
                    &["Plant Manager", "Operations Director"],
                ),
                dept(
                    "Produktion",
                    6,
                    &["Line Operator", "Shift Supervisor", "Machine Technician"],
                ),
                dept("Qualitaetssicherung", 3, &["Quality Engineer", "Inspector"]),
                dept(
                    "Instandhaltung",
                    2,
                    &["Maintenance Technician", "Facilities Engineer"],
                ),
                dept("Logistik", 2, &["Warehouse Coordinator", "Dispatcher"]),
                dept("Verwaltung", 1, &["Office Manager", "HR Coordinator"]),
            ],
            CompanyType::Healthcare => vec![
                dept("Leitung", 1, &["Medical Director", "Operations Lead"]),
                dept("Pflege", 5, &["Care Coordinator", "Nurse", "Shift Lead"]),
                dept("Aerztlicher Dienst", 3, &["Physician", "Resident"]),
                dept("Diagnostik", 2, &["Lab Technician", "Radiology Specialist"]),
                dept(
                    "Verwaltung",
                    2,
                    &["Patient Coordinator", "Billing Specialist"],
                ),
                dept("IT", 1, &["Systems Administrator"]),
            ],
            CompanyType::Generic => vec![
                dept("Geschaeftsfuehrung", 1, &["Managing Director"]),
                dept(
                    "Operations",
                    4,
                    &["Operations Specialist", "Team Coordinator"],
                ),
                dept(
                    "Customer Success",
                    2,
                    &["Customer Success Manager", "Support Specialist"],
                ),
                dept("Finance", 1, &["Controller", "Accountant"]),
                dept("IT", 1, &["IT Administrator"]),
            ],
        }
    }
}

fn default_city() -> String {
    "Nuernberg".to_string()
}

fn default_address() -> String {
    "Fuerther Strasse 42, 90429 Nuernberg".to_string()
}

fn default_seed() -> u64 {
    42
}

fn default_time_scale() -> f32 {
    1.0
}

fn dept(name: &str, weight: u16, roles: &[&str]) -> DepartmentSpec {
    DepartmentSpec {
        name: name.to_string(),
        weight,
        roles: roles.iter().map(|role| role.to_string()).collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DepartmentPlan {
    pub name: String,
    pub agent_count: u16,
    pub roles: Vec<String>,
    pub lead: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompanyStructure {
    pub departments: Vec<DepartmentPlan>,
    pub hierarchy_root: String,
    pub shift_model: ShiftModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub relative_path: PathBuf,
    pub contents: String,
}

#[derive(Debug, Clone)]
pub struct GeneratedCompany {
    pub spec: GaiaSpec,
    pub structure: CompanyStructure,
    pub files: Vec<GeneratedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    pub root: PathBuf,
    pub files_written: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub agents: usize,
    pub rooms: usize,
    pub total_room_capacity: u32,
    pub daemon_max_agents: u64,
    pub nightrun_max_agent_id: u64,
}

pub fn generate(spec: GaiaSpec) -> Result<GeneratedCompany> {
    spec.validate()?;
    let departments = spec.effective_departments();
    let counts = allocate_departments(spec.agent_count, &departments);
    let structure = build_structure(&spec, &departments, &counts);
    let rooms = build_rooms(&spec, &structure);
    let agents = build_agents(&spec, &structure, &rooms.department_room);

    let mut files = Vec::new();
    files.push(GeneratedFile {
        relative_path: PathBuf::from(GAIA_SPEC_FILENAME),
        contents: toml::to_string_pretty(&spec).context("serialize gaia spec")?,
    });
    for agent in agents {
        let file_name = agent_file_name(agent.identity.id, spec.agent_count, &agent.identity.name);
        files.push(GeneratedFile {
            relative_path: PathBuf::from("agents").join(file_name),
            contents: toml::to_string_pretty(&agent).context("serialize agent TOML")?,
        });
    }
    files.push(GeneratedFile {
        relative_path: PathBuf::from("rooms.toml"),
        contents: toml::to_string_pretty(&rooms.toml).context("serialize rooms TOML")?,
    });
    files.push(GeneratedFile {
        relative_path: PathBuf::from("daemon.toml"),
        contents: daemon_toml(&spec),
    });
    files.push(GeneratedFile {
        relative_path: PathBuf::from("nightrun.toml"),
        contents: nightrun_toml(&spec),
    });
    files.push(GeneratedFile {
        relative_path: PathBuf::from("company-context.md"),
        contents: render_company_context(&spec),
    });

    Ok(GeneratedCompany {
        spec,
        structure,
        files,
    })
}

pub fn read_spec(path: &Path) -> Result<GaiaSpec> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read Gaia spec {}", path.display()))?;
    let spec: GaiaSpec =
        toml::from_str(&content).with_context(|| format!("parse Gaia spec {}", path.display()))?;
    spec.validate()?;
    Ok(spec)
}

pub fn validate_output_dir(root: &Path) -> Result<ValidationReport> {
    let spec = read_spec(&root.join(GAIA_SPEC_FILENAME))?;
    let validation = AgentConfigValidation::with_max_agent_id(spec.agent_count);
    let agents = sentinel_common::agent_config::load_all_agents_with_validation(
        &root.join("agents"),
        validation,
    )
    .with_context(|| {
        format!(
            "load generated agents from {}",
            root.join("agents").display()
        )
    })?;

    if agents.len() != usize::from(spec.agent_count) {
        bail!(
            "output contains {} agents, expected {}",
            agents.len(),
            spec.agent_count
        );
    }

    let mut ids = BTreeSet::new();
    for agent in &agents {
        if !ids.insert(agent.identity.id) {
            bail!("duplicate generated AgentId {}", agent.identity.id);
        }
        if agent.runtime.nano_runtime.as_deref() != Some(RUNTIME_ECS_NATIVE) {
            bail!(
                "generated agent {} does not use runtime {}",
                agent.identity.id,
                RUNTIME_ECS_NATIVE
            );
        }
    }

    let rooms_path = root.join("rooms.toml");
    let rooms_content = fs::read_to_string(&rooms_path)
        .with_context(|| format!("read generated {}", rooms_path.display()))?;
    let rooms: BuildingConfig = toml::from_str(&rooms_content)
        .with_context(|| format!("parse generated {}", rooms_path.display()))?;
    rooms
        .validate(spec.agent_count)
        .map_err(|errors| anyhow!("generated rooms.toml invalid: {}", errors.join("; ")))?;
    let total_room_capacity = rooms.rooms.iter().map(|room| room.capacity as u32).sum();

    let daemon_path = root.join("daemon.toml");
    let daemon_content = fs::read_to_string(&daemon_path)
        .with_context(|| format!("read generated {}", daemon_path.display()))?;
    let daemon_max_agents = table_int(&daemon_content, &["daemon", "max_agents"])?;
    if daemon_max_agents != u64::from(spec.agent_count) {
        bail!(
            "daemon.max_agents {} != spec.agent_count {}",
            daemon_max_agents,
            spec.agent_count
        );
    }

    let nightrun_path = root.join("nightrun.toml");
    let nightrun_content = fs::read_to_string(&nightrun_path)
        .with_context(|| format!("read generated {}", nightrun_path.display()))?;
    let nightrun_max_agent_id = table_int(&nightrun_content, &["nightrun", "max_agent_id"])?;
    if nightrun_max_agent_id != u64::from(spec.agent_count) {
        bail!(
            "nightrun.max_agent_id {} != spec.agent_count {}",
            nightrun_max_agent_id,
            spec.agent_count
        );
    }

    // #441 AC3: eigene company-context.md muss existieren + die Firmendaten tragen (kein Default).
    let context_path = root.join("company-context.md");
    let context_content = fs::read_to_string(&context_path)
        .with_context(|| format!("read generated {}", context_path.display()))?;
    if !context_content.contains(&spec.company_name) {
        bail!(
            "generated company-context.md does not mention company '{}'",
            spec.company_name
        );
    }

    Ok(ValidationReport {
        agents: agents.len(),
        rooms: rooms.rooms.len(),
        total_room_capacity,
        daemon_max_agents,
        nightrun_max_agent_id,
    })
}

impl GeneratedCompany {
    pub fn file(&self, relative_path: impl AsRef<Path>) -> Option<&GeneratedFile> {
        let relative_path = relative_path.as_ref();
        self.files
            .iter()
            .find(|file| file.relative_path == relative_path)
    }

    pub fn write_to_dir(&self, root: &Path, overwrite: bool) -> Result<WriteReport> {
        let mut files_written = Vec::new();
        for file in &self.files {
            let target = root.join(&file.relative_path);
            if target.exists() && !overwrite {
                bail!("refusing to overwrite {}", target.display());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create output dir {}", parent.display()))?;
            }
            fs::write(&target, &file.contents)
                .with_context(|| format!("write generated file {}", target.display()))?;
            files_written.push(file.relative_path.clone());
        }
        Ok(WriteReport {
            root: root.to_path_buf(),
            files_written,
        })
    }

    pub fn validate(&self) -> Result<ValidationReport> {
        let mut ids = BTreeSet::new();
        let validation = AgentConfigValidation::with_max_agent_id(self.spec.agent_count);
        let mut agent_count = 0usize;
        for file in self.files.iter().filter(|file| {
            file.relative_path
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == "agents")
        }) {
            let agent: AgentConfig = toml::from_str(&file.contents)
                .with_context(|| format!("parse generated {}", file.relative_path.display()))?;
            agent.personality.validate()?;
            AgentId::new_with_bounds(agent.identity.id, validation.agent_id_bounds)
                .with_context(|| format!("validate generated {}", file.relative_path.display()))?;
            if !ids.insert(agent.identity.id) {
                bail!("duplicate generated AgentId {}", agent.identity.id);
            }
            if agent.runtime.nano_runtime.as_deref() != Some(RUNTIME_ECS_NATIVE) {
                bail!(
                    "generated agent {} does not use runtime {}",
                    agent.identity.id,
                    RUNTIME_ECS_NATIVE
                );
            }
            agent_count += 1;
        }
        if agent_count != usize::from(self.spec.agent_count) {
            bail!(
                "generated {} agents, expected {}",
                agent_count,
                self.spec.agent_count
            );
        }

        let rooms_file = self
            .file("rooms.toml")
            .ok_or_else(|| anyhow!("rooms.toml missing"))?;
        let rooms: BuildingConfig =
            toml::from_str(&rooms_file.contents).context("parse generated rooms.toml")?;
        rooms
            .validate(self.spec.agent_count)
            .map_err(|errors| anyhow!("generated rooms.toml invalid: {}", errors.join("; ")))?;
        let total_room_capacity = rooms.rooms.iter().map(|room| room.capacity as u32).sum();

        let daemon_max_agents =
            table_int(self.file_text("daemon.toml")?, &["daemon", "max_agents"])?;
        if daemon_max_agents != u64::from(self.spec.agent_count) {
            bail!(
                "daemon.max_agents {} != spec.agent_count {}",
                daemon_max_agents,
                self.spec.agent_count
            );
        }
        let nightrun_max_agent_id = table_int(
            self.file_text("nightrun.toml")?,
            &["nightrun", "max_agent_id"],
        )?;
        if nightrun_max_agent_id != u64::from(self.spec.agent_count) {
            bail!(
                "nightrun.max_agent_id {} != spec.agent_count {}",
                nightrun_max_agent_id,
                self.spec.agent_count
            );
        }
        if self.file("company.toml").is_some() {
            bail!("Gaia must not emit runtime company.toml; use gaia-spec.toml");
        }
        if self.file(GAIA_SPEC_FILENAME).is_none() {
            bail!("{GAIA_SPEC_FILENAME} missing");
        }

        Ok(ValidationReport {
            agents: agent_count,
            rooms: rooms.rooms.len(),
            total_room_capacity,
            daemon_max_agents,
            nightrun_max_agent_id,
        })
    }

    fn file_text(&self, relative_path: &str) -> Result<&str> {
        self.file(relative_path)
            .map(|file| file.contents.as_str())
            .ok_or_else(|| anyhow!("{relative_path} missing"))
    }
}

fn table_int(toml_text: &str, path: &[&str]) -> Result<u64> {
    let value: toml::Value = toml::from_str(toml_text).context("parse generated TOML")?;
    let mut cursor = &value;
    for key in path {
        cursor = cursor
            .get(*key)
            .ok_or_else(|| anyhow!("missing TOML key {}", path.join(".")))?;
    }
    cursor
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| anyhow!("TOML key {} is not a non-negative integer", path.join(".")))
}

fn allocate_departments(agent_count: u16, departments: &[DepartmentSpec]) -> Vec<u16> {
    let active_len = usize::from(agent_count).min(departments.len());
    let active = &departments[..active_len];
    let mut counts = vec![1u16; active_len];
    let mut remaining = agent_count.saturating_sub(active_len as u16);
    let total_weight: u32 = active
        .iter()
        .map(|department| u32::from(department.weight))
        .sum();
    let mut order: Vec<(usize, u32)> = active
        .iter()
        .enumerate()
        .map(|(index, department)| (index, u32::from(department.weight)))
        .collect();
    order.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    while remaining > 0 {
        for (index, weight) in &order {
            let quota =
                ((*weight).max(1) * u32::from(remaining)).max(total_weight) / total_weight.max(1);
            let add = quota.max(1).min(u32::from(remaining)) as u16;
            counts[*index] += add;
            remaining -= add;
            if remaining == 0 {
                break;
            }
        }
    }

    counts
}

fn build_structure(
    spec: &GaiaSpec,
    departments: &[DepartmentSpec],
    counts: &[u16],
) -> CompanyStructure {
    let plans = departments
        .iter()
        .zip(counts.iter())
        .map(|(department, count)| DepartmentPlan {
            name: department.name.clone(),
            agent_count: *count,
            roles: department.roles.clone(),
            lead: None,
        })
        .collect();

    CompanyStructure {
        departments: plans,
        hierarchy_root: format!("{} Leitung", spec.company_name),
        shift_model: spec.shift_model.clone(),
    }
}

#[derive(Debug, Serialize)]
struct AgentToml {
    identity: AgentIdentityToml,
    personality: AgentPersonalityToml,
    preferences: AgentPreferencesToml,
    background: AgentBackgroundToml,
    runtime: AgentRuntimeToml,
    capabilities: AgentCapabilitiesToml,
}

#[derive(Debug, Serialize)]
struct AgentIdentityToml {
    id: u16,
    name: String,
    role: String,
    department: String,
    tier: u8,
    shift_set: u8,
    kpis: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reports_to: Option<String>,
    direct_reports: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct AgentPersonalityToml {
    openness: f32,
    conscientiousness: f32,
    extraversion: f32,
    agreeableness: f32,
    neuroticism: f32,
    caffeine_tolerance: f32,
    morning_person: bool,
}

#[derive(Debug, Serialize)]
struct AgentPreferencesToml {
    favorite_room: String,
    coffee_preference: String,
    lunch_time: String,
}

#[derive(Debug, Serialize)]
struct AgentBackgroundToml {
    bio: String,
    quirks: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AgentRuntimeToml {
    nano_runtime: String,
}

#[derive(Debug, Serialize)]
struct AgentCapabilitiesToml {
    tools: Vec<String>,
    sandbox_allowed_paths: Vec<String>,
}

fn build_agents(
    spec: &GaiaSpec,
    structure: &CompanyStructure,
    department_room: &BTreeMap<String, String>,
) -> Vec<AgentToml> {
    let mut raw = Vec::new();
    let mut id = 1u16;
    let mut leads: BTreeMap<String, String> = BTreeMap::new();
    let mut used_names = BTreeSet::new();

    for department in &structure.departments {
        for index in 0..department.agent_count {
            let mut name = deterministic_name(spec.seed, id);
            if !used_names.insert(name.clone()) {
                name = format!("{name} {id}");
                used_names.insert(name.clone());
            }
            let is_company_lead = id == 1;
            let is_department_lead = index == 0;
            let role = if is_company_lead {
                department
                    .roles
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Managing Director".to_string())
            } else if is_department_lead {
                format!("{} Lead", department.name)
            } else {
                role_for(department, usize::from(index), spec.seed, id)
            };
            if is_department_lead {
                leads.insert(department.name.clone(), name.clone());
            }
            raw.push(RawAgent {
                id,
                name,
                role,
                department: department.name.clone(),
                tier: if is_company_lead {
                    1
                } else if is_department_lead {
                    2
                } else {
                    3
                },
                shift_set: shift_for(spec, id, is_company_lead || is_department_lead),
            });
            id += 1;
        }
    }

    let company_lead = raw
        .first()
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| structure.hierarchy_root.clone());
    let mut direct_reports: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for agent in raw.iter().skip(1) {
        let reports_to = if leads.get(&agent.department) == Some(&agent.name) {
            &company_lead
        } else {
            leads.get(&agent.department).unwrap_or(&company_lead)
        };
        direct_reports
            .entry(reports_to.clone())
            .or_default()
            .push(agent.name.clone());
    }

    raw.into_iter()
        .map(|agent| {
            let favorite_room = department_room
                .get(&agent.department)
                .cloned()
                .unwrap_or_else(|| "kueche".to_string());
            let reports_to = if agent.id == 1 {
                None
            } else if leads.get(&agent.department) == Some(&agent.name) {
                Some(company_lead.clone())
            } else {
                leads.get(&agent.department).cloned().or(Some(company_lead.clone()))
            };
            AgentToml {
                identity: AgentIdentityToml {
                    id: agent.id,
                    name: agent.name.clone(),
                    role: agent.role,
                    department: agent.department.clone(),
                    tier: agent.tier,
                    shift_set: agent.shift_set,
                    kpis: kpis_for(&agent.department),
                    reports_to,
                    direct_reports: direct_reports.remove(&agent.name).unwrap_or_default(),
                },
                personality: personality_for(spec.seed, agent.id, &spec.culture),
                preferences: preferences_for(spec.seed, agent.id, favorite_room),
                background: AgentBackgroundToml {
                    bio: format!(
                        "{} arbeitet im Bereich {} bei {}. Der Agent wurde deterministisch aus Gaia-Seed {} erzeugt.",
                        agent.name, agent.department, spec.company_name, spec.seed
                    ),
                    quirks: quirks_for(spec.seed, agent.id),
                },
                runtime: AgentRuntimeToml {
                    nano_runtime: RUNTIME_ECS_NATIVE.to_string(),
                },
                capabilities: capabilities_for(&agent.department, agent.id == 1),
            }
        })
        .collect()
}

#[derive(Debug)]
struct RawAgent {
    id: u16,
    name: String,
    role: String,
    department: String,
    tier: u8,
    shift_set: u8,
}

fn role_for(department: &DepartmentPlan, index: usize, seed: u64, id: u16) -> String {
    if department.roles.is_empty() {
        return format!("{} Specialist", department.name);
    }
    let offset = usize::from(hash_byte(seed, id, "role"));
    department.roles[(index + offset) % department.roles.len()].clone()
}

fn shift_for(spec: &GaiaSpec, id: u16, lead: bool) -> u8 {
    match spec.shift_model {
        ShiftModel::OfficeHours => 1,
        ShiftModel::ThreeShift => ((id - 1) % 3 + 1) as u8,
        ShiftModel::Hybrid => {
            if lead {
                1
            } else {
                ((id - 1) % 3 + 1) as u8
            }
        }
    }
}

fn kpis_for(department: &str) -> Vec<String> {
    vec![
        format!("{department} cycle time within target"),
        format!("{department} handoffs completed"),
    ]
}

/// Firmenmission: explizit aus der Spec, sonst deterministisch aus `company_type` abgeleitet (#441).
fn effective_mission(spec: &GaiaSpec) -> String {
    let trimmed = spec.culture.mission.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    match spec.company_type {
        CompanyType::SoftwareAgency => format!(
            "{} entwickelt digitale Produkte und Dienstleistungen in verlaesslicher Qualitaet.",
            spec.company_name
        ),
        CompanyType::Manufacturing => format!(
            "{} fertigt Produkte mit gleichbleibend hoher Qualitaet und sicheren Prozessen.",
            spec.company_name
        ),
        CompanyType::Healthcare => format!(
            "{} versorgt Patientinnen und Patienten verantwortungsvoll und zugewandt.",
            spec.company_name
        ),
        CompanyType::Generic => {
            format!(
                "{} liefert verlaessliche Leistungen fuer ihre Kunden.",
                spec.company_name
            )
        }
    }
}

/// Firmenwerte: explizit aus der Spec, sonst ein neutraler Default-Satz (#441).
fn effective_values(spec: &GaiaSpec) -> Vec<String> {
    if !spec.culture.values.is_empty() {
        return spec.culture.values.clone();
    }
    vec![
        "Verlaesslichkeit".to_string(),
        "Zusammenarbeit".to_string(),
        "Qualitaet".to_string(),
    ]
}

fn level_label(value: f32) -> &'static str {
    if value < 0.34 {
        "niedrig"
    } else if value < 0.67 {
        "mittel"
    } else {
        "hoch"
    }
}

fn company_type_label(company_type: &CompanyType) -> &'static str {
    match company_type {
        CompanyType::SoftwareAgency => "Software-/Digitalagentur",
        CompanyType::Manufacturing => "Produktion / Fertigung",
        CompanyType::Healthcare => "Gesundheitswesen",
        CompanyType::Generic => "Allgemein",
    }
}

fn shift_model_label(shift_model: &ShiftModel) -> &'static str {
    match shift_model {
        ShiftModel::OfficeHours => "Bueroarbeitszeiten",
        ShiftModel::ThreeShift => "3-Schicht-Betrieb (24/7)",
        ShiftModel::Hybrid => "Hybrid (Kernzeit + Schichten)",
    }
}

/// Rendert die firmenweite `company-context.md` deterministisch aus der Spec (#441, kein LLM).
/// Mission/Werte stammen aus der (von Gaia befuellten) Spec oder werden aus `company_type` abgeleitet;
/// Organigramm/KPIs aus den (effektiven) Abteilungen; Kultur-Achsen als Prosa. blake3-frei → bei
/// gleicher Spec byte-identisch.
fn render_company_context(spec: &GaiaSpec) -> String {
    let departments = spec.effective_departments();
    let mission = effective_mission(spec);
    let values = effective_values(spec);
    let mut out = String::new();

    out.push_str(&format!("# {} — Firmenkontext\n\n", spec.company_name));
    out.push_str(
        "> Deterministisch von `sentinel-gaia` aus der `gaia-spec` erzeugt (#441). \
         Nicht manuell editieren — ueber die gaia-spec + Regenerierung pflegen.\n\n",
    );

    out.push_str("## Unternehmen\n");
    out.push_str(&format!("- **Standort:** {}\n", spec.city));
    out.push_str(&format!("- **Adresse:** {}\n", spec.address));
    out.push_str(&format!(
        "- **Typ:** {}\n",
        company_type_label(&spec.company_type)
    ));
    out.push_str(&format!("- **Mitarbeiter:** {}\n", spec.agent_count));
    out.push_str(&format!(
        "- **Arbeitsmodell:** {}\n\n",
        shift_model_label(&spec.shift_model)
    ));

    out.push_str("## Mission & Werte\n");
    out.push_str(&format!("{mission}\n\n"));
    out.push_str("Werte:\n");
    for value in &values {
        out.push_str(&format!("- {value}\n"));
    }
    out.push('\n');

    out.push_str("## Organigramm\n");
    if let Some(lead_dept) = departments.first() {
        let lead_role = lead_dept
            .roles
            .first()
            .cloned()
            .unwrap_or_else(|| "Geschaeftsfuehrung".to_string());
        out.push_str(&format!(
            "- **Leitung:** {} (Abteilung {})\n",
            lead_role, lead_dept.name
        ));
    }
    for department in departments.iter().skip(1) {
        let roles = if department.roles.is_empty() {
            "diverse Rollen".to_string()
        } else {
            department.roles.join(", ")
        };
        out.push_str(&format!("- **{}:** {}\n", department.name, roles));
    }
    out.push('\n');

    out.push_str("## Abteilungs-KPIs\n");
    for department in &departments {
        out.push_str(&format!(
            "- **{}:** {}\n",
            department.name,
            kpis_for(&department.name).join("; ")
        ));
    }
    out.push('\n');

    out.push_str("## Kultur\n");
    out.push_str(&format!(
        "- Formalitaet: {}\n",
        level_label(spec.culture.formality)
    ));
    out.push_str(&format!(
        "- Zusammenarbeit: {}\n",
        level_label(spec.culture.collaboration)
    ));
    out.push_str(&format!(
        "- Konfliktniveau: {}\n",
        level_label(spec.culture.conflict_level)
    ));
    out.push_str(&format!(
        "- Innovationsgrad: {}\n",
        level_label(spec.culture.innovation)
    ));
    out.push_str(&format!(
        "- Diversitaet: {}\n",
        level_label(spec.culture.diversity)
    ));

    out
}

/// Leitet die Big-Five-Verteilung eines Agents deterministisch aus Seed+Id UND der Firmen-Kultur ab
/// (#441). Kultur-Achsen verschieben den Mittelwert (`center`) bzw. die Streuung (`spread`) pro Trait;
/// `diversity` weitet die globale Streuung; `conflict_level` weitet speziell die Neuroticism-Streuung.
/// Bleibt blake3-deterministisch: gleiche Spec+Seed → identische Werte.
fn personality_for(seed: u64, id: u16, culture: &CultureSpec) -> AgentPersonalityToml {
    // Globale Streuung: homogen (0.45) bis heterogen (0.80) je nach Diversitaet.
    let base_spread = 0.45 + culture.diversity * 0.35;
    AgentPersonalityToml {
        openness: score_with(
            seed,
            id,
            "openness",
            axis_center(culture.innovation),
            base_spread,
        ),
        conscientiousness: score_with(
            seed,
            id,
            "conscientiousness",
            axis_center(culture.formality),
            base_spread,
        ),
        // Keine eigene Achse → neutraler Mittelwert, kulturweite Streuung.
        extraversion: score_with(seed, id, "extraversion", 0.5, base_spread),
        agreeableness: score_with(
            seed,
            id,
            "agreeableness",
            axis_center(culture.collaboration),
            base_spread,
        ),
        // Konfliktniveau weitet die Streuung (mehr Reibung → breiteres Spektrum an Belastbarkeit).
        neuroticism: score_with(
            seed,
            id,
            "neuroticism",
            0.5,
            base_spread + culture.conflict_level * 0.40,
        ),
        caffeine_tolerance: score_with(seed, id, "caffeine", 0.5, base_spread),
        morning_person: hash_byte(seed, id, "morning").is_multiple_of(2),
    }
}

/// Bildet eine Kultur-Achse \[0,1] auf einen Trait-Mittelwert in \[0.35, 0.65] ab (0.5 = neutral).
fn axis_center(axis: f32) -> f32 {
    0.35 + axis.clamp(0.0, 1.0) * 0.30
}

fn preferences_for(seed: u64, id: u16, favorite_room: String) -> AgentPreferencesToml {
    const COFFEE: &[&str] = &["schwarz", "espresso", "latte", "tee", "filterkaffee"];
    let coffee = COFFEE[usize::from(hash_byte(seed, id, "coffee")) % COFFEE.len()].to_string();
    let lunch_hour = 11 + (hash_byte(seed, id, "lunch") % 3);
    let lunch_min = if hash_byte(seed, id, "lunch_min").is_multiple_of(2) {
        "00"
    } else {
        "30"
    };
    AgentPreferencesToml {
        favorite_room,
        coffee_preference: coffee,
        lunch_time: format!("{lunch_hour}:{lunch_min}"),
    }
}

fn quirks_for(seed: u64, id: u16) -> Vec<String> {
    const QUIRKS: &[&str] = &[
        "Notiert Entscheidungen sofort",
        "Prueft Kalender sehr genau",
        "Bevorzugt kurze Statusupdates",
        "Fragt nach klaren Ownerships",
        "Trinkt Kaffee vor Standups",
        "Dokumentiert offene Punkte",
    ];
    let first = usize::from(hash_byte(seed, id, "quirk-a")) % QUIRKS.len();
    let second = usize::from(hash_byte(seed, id, "quirk-b")) % QUIRKS.len();
    if first == second {
        vec![QUIRKS[first].to_string()]
    } else {
        vec![QUIRKS[first].to_string(), QUIRKS[second].to_string()]
    }
}

fn capabilities_for(department: &str, company_lead: bool) -> AgentCapabilitiesToml {
    let mut tools = vec!["chat".to_string(), "calendar".to_string()];
    if company_lead || matches!(department, "Entwicklung" | "IT" | "Qualitaetssicherung") {
        tools.push("search".to_string());
    }
    AgentCapabilitiesToml {
        tools,
        sandbox_allowed_paths: Vec::new(),
    }
}

fn deterministic_name(seed: u64, id: u16) -> String {
    const FIRST: &[&str] = &[
        "Thomas", "Lisa", "Max", "Sophie", "Andreas", "Julia", "Kai", "Lena", "Sarah", "Daniel",
        "Marco", "Nina", "Petra", "Florian", "Hannah", "Michael", "Carla", "Robin", "Tim",
        "Martin", "Fatima", "Jonas", "Anna", "Elena", "Lukas", "Oliver", "Mara", "Gabi", "Tobias",
        "Yara", "Sandra", "Jens", "Priya", "Lea", "Kevin", "Nils", "Selina", "Paul", "Victoria",
        "David",
    ];
    const LAST: &[&str] = &[
        "Mueller",
        "Brenner",
        "Wolff",
        "Bergmann",
        "Rossi",
        "Wagner",
        "Zimmermann",
        "Schneider",
        "Fischer",
        "Weber",
        "Meyer",
        "Hoffmann",
        "Schulz",
        "Klein",
        "Neumann",
        "Kraus",
        "Lang",
        "Schmitt",
        "Hartmann",
        "Koenig",
    ];
    let first = FIRST[usize::from(hash_byte(seed, id, "first")) % FIRST.len()];
    let last = LAST[usize::from(hash_byte(seed, id, "last")) % LAST.len()];
    format!("{first} {last}")
}

/// Deterministischer Trait-Wert um `center` mit Streuung `spread`, blake3-basiert, auf \[0.05, 0.95]
/// geklemmt (innerhalb des von `PersonalityConfig::validate` geforderten \[0,1]) und auf 2 Nachkommastellen.
fn score_with(seed: u64, id: u16, field: &str, center: f32, spread: f32) -> f32 {
    let norm = f32::from(hash_byte(seed, id, field)) / 255.0; // [0,1]
    let value = center + (norm - 0.5) * spread;
    let clamped = value.clamp(0.05, 0.95);
    (clamped * 100.0).round() / 100.0
}

fn hash_byte(seed: u64, id: u16, field: &str) -> u8 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.update(&id.to_le_bytes());
    hasher.update(field.as_bytes());
    hasher.finalize().as_bytes()[0]
}

fn agent_file_name(id: u16, max_id: u16, name: &str) -> String {
    let width = max_id.to_string().len().max(2);
    let slug = slugify(name);
    format!("AGENT-{id:0width$}-{slug}.toml")
}

fn slugify(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Debug)]
struct RoomsBuild {
    toml: RoomsToml,
    department_room: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct RoomsToml {
    building: BuildingToml,
    rooms: Vec<RoomToml>,
}

#[derive(Debug, Serialize)]
struct BuildingToml {
    name: String,
    address: String,
    floors: u8,
}

#[derive(Debug, Serialize)]
struct RoomToml {
    id: String,
    name: String,
    floor: i8,
    capacity: u16,
    room_type: String,
    adjacent: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    department: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    has_coffee_machine: bool,
    #[serde(skip_serializing_if = "is_false")]
    has_printer: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn build_rooms(spec: &GaiaSpec, structure: &CompanyStructure) -> RoomsBuild {
    let mut rooms = vec![
        room("empfang", "Empfang", 0, 4, "common", &["flur-eg"], None),
        room(
            "flur-eg",
            "Flur Erdgeschoss",
            0,
            20,
            "transit",
            &["empfang", "kueche", "treppenhaus"],
            None,
        ),
        RoomToml {
            has_coffee_machine: true,
            ..room(
                "kueche",
                "Kueche / Pausenraum",
                0,
                12,
                "break",
                &["flur-eg"],
                None,
            )
        },
        room(
            "treppenhaus",
            "Treppenhaus",
            -1,
            8,
            "transit",
            &["flur-eg", "flur-og"],
            None,
        ),
        room(
            "flur-og",
            "Flur Obergeschoss",
            1,
            20,
            "transit",
            &["treppenhaus"],
            None,
        ),
        room(
            "meetingraum-01",
            "Meetingraum A",
            0,
            12,
            "meeting",
            &["flur-eg"],
            None,
        ),
        room(
            "meetingraum-02",
            "Meetingraum B",
            1,
            12,
            "meeting",
            &["flur-og"],
            None,
        ),
        room(
            "toilette-eg",
            "Toilette EG",
            0,
            6,
            "bathroom",
            &["flur-eg"],
            None,
        ),
        room(
            "toilette-og",
            "Toilette OG",
            1,
            6,
            "bathroom",
            &["flur-og"],
            None,
        ),
    ];
    let mut hall_updates: BTreeMap<String, Vec<String>> = BTreeMap::from([
        (
            "flur-eg".to_string(),
            vec!["meetingraum-01".to_string(), "toilette-eg".to_string()],
        ),
        (
            "flur-og".to_string(),
            vec!["meetingraum-02".to_string(), "toilette-og".to_string()],
        ),
    ]);
    let mut department_room = BTreeMap::new();

    for (index, department) in structure.departments.iter().enumerate() {
        let floor = (index % 2) as i8;
        let hall = if floor == 0 { "flur-eg" } else { "flur-og" };
        let room_id = format!("buero-{}", slugify(&department.name).to_ascii_lowercase());
        let capacity = department.agent_count.saturating_add(2).max(2);
        rooms.push(RoomToml {
            has_printer: true,
            ..room(
                &room_id,
                &format!("Buero {}", department.name),
                floor,
                capacity,
                "office",
                &[hall],
                Some(&department.name),
            )
        });
        hall_updates
            .entry(hall.to_string())
            .or_default()
            .push(room_id.clone());
        department_room.insert(department.name.clone(), room_id);
    }

    for room in &mut rooms {
        if let Some(extra) = hall_updates.get(&room.id) {
            for id in extra {
                if !room.adjacent.contains(id) {
                    room.adjacent.push(id.clone());
                }
            }
        }
    }

    RoomsBuild {
        toml: RoomsToml {
            building: BuildingToml {
                name: spec.company_name.clone(),
                address: spec.address.clone(),
                floors: 2,
            },
            rooms,
        },
        department_room,
    }
}

fn room(
    id: &str,
    name: &str,
    floor: i8,
    capacity: u16,
    room_type: &str,
    adjacent: &[&str],
    department: Option<&str>,
) -> RoomToml {
    RoomToml {
        id: id.to_string(),
        name: name.to_string(),
        floor,
        capacity,
        room_type: room_type.to_string(),
        adjacent: adjacent.iter().map(|id| id.to_string()).collect(),
        department: department.map(str::to_string),
        has_coffee_machine: false,
        has_printer: false,
    }
}

fn daemon_toml(spec: &GaiaSpec) -> String {
    format!(
        r#"[daemon]
config_dir = "config"
data_dir = "data"
tick_rate_ms = 1000
time_scale = {time_scale}
max_agents = {agent_count}
zenoh_prefix = "sentinel"
"#,
        time_scale = spec.time_scale,
        agent_count = spec.agent_count
    )
}

fn nightrun_toml(spec: &GaiaSpec) -> String {
    format!(
        r#"[nightrun]
hippocampus_db = "data/hippocampus.redb"
event_store_db = "data/events.db"
agent_config_dir = "config/agents"
max_agent_id = {agent_count}
job_queue_path = "data/nightrun-jobs.db"
timeout_per_agent_secs = 300
timeout_total_secs = 7200
max_episodes_per_agent = 1000
max_jobs_per_run = {max_jobs}
"#,
        agent_count = spec.agent_count,
        max_jobs = usize::from(spec.agent_count).max(100)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::agent_config::load_all_agents_with_validation;

    #[test]
    fn same_seed_is_reproducible() {
        let spec = GaiaSpec::example();
        let a = generate(spec.clone()).unwrap();
        let b = generate(spec).unwrap();

        assert_eq!(a.files, b.files);
    }

    #[test]
    fn spec_without_culture_loads_with_defaults() {
        // #441 AC: Bestands-Specs ohne [culture]-Block bleiben gueltig (serde-default).
        let toml_str = "company_name = \"Legacy GmbH\"\nagent_count = 10\n";
        let spec: GaiaSpec = toml::from_str(toml_str).expect("legacy spec parses");
        assert_eq!(spec.culture, CultureSpec::default());
        spec.validate().expect("legacy spec is valid");
    }

    #[test]
    fn spec_with_culture_toml_round_trips() {
        // #441: company-context-Write-Back braucht stabiles TOML-Round-Trip inkl. CultureSpec.
        let spec = GaiaSpec::example();
        let serialized = toml::to_string_pretty(&spec).expect("serialize GaiaSpec");
        let reparsed: GaiaSpec = toml::from_str(&serialized).expect("re-parse GaiaSpec");
        assert_eq!(spec, reparsed, "GaiaSpec TOML round-trip must be identical");
    }

    #[test]
    fn validate_rejects_out_of_range_culture_axis() {
        // #441 AC: validate lehnt eine Kultur-Achse ausserhalb [0,1] ab.
        let mut spec = GaiaSpec::example();
        spec.culture.conflict_level = 1.5;
        assert!(spec.validate().is_err());
        let mut spec2 = GaiaSpec::example();
        spec2.culture.formality = -0.1;
        assert!(spec2.validate().is_err());
    }

    #[test]
    fn generate_emits_own_company_context() {
        // #441 AC3: generierte Firma hat eigene company-context.md mit Firmendaten (kein PixelPerfekt-Default).
        let spec = GaiaSpec {
            company_name: "Acme Robotics AG".to_string(),
            ..GaiaSpec::example()
        };
        let generated = generate(spec).unwrap();
        let ctx = generated
            .file("company-context.md")
            .expect("company-context.md emitted");
        assert!(
            ctx.contents.contains("Acme Robotics AG"),
            "context names the company"
        );
        assert!(
            !ctx.contents.contains("PixelPerfekt"),
            "context is not the default"
        );
        assert!(ctx.contents.contains("## Mission & Werte"));
        assert!(ctx.contents.contains("## Organigramm"));
        assert!(ctx.contents.contains("## Kultur"));
        // Mission aus example()-Spec uebernommen (nicht aus company_type abgeleitet).
        assert!(ctx
            .contents
            .contains("Digitale Produkte mit Handwerks-Qualitaet"));
    }

    #[test]
    fn company_context_derives_mission_when_spec_empty() {
        // Leere culture.mission/values -> deterministisch aus company_type abgeleitet.
        let spec = GaiaSpec {
            company_name: "Werk Nord GmbH".to_string(),
            company_type: CompanyType::Manufacturing,
            culture: CultureSpec::default(),
            ..GaiaSpec::example()
        };
        let generated = generate(spec).unwrap();
        let ctx = &generated.file("company-context.md").unwrap().contents;
        assert!(ctx.contains("Werk Nord GmbH"));
        assert!(
            ctx.contains("fertigt Produkte"),
            "mission derived from Manufacturing type"
        );
    }

    #[test]
    fn personality_for_is_deterministic_with_culture() {
        // #441 AC1: gleiche Spec+Seed → identische Big-Five-Werte (blake3, kein RNG).
        let culture = CultureSpec {
            conflict_level: 0.8,
            innovation: 0.9,
            ..CultureSpec::default()
        };
        for id in 1..=25u16 {
            assert_eq!(
                personality_for(42, id, &culture),
                personality_for(42, id, &culture)
            );
        }
    }

    fn neuroticism_variance(conflict_level: f32) -> f32 {
        let culture = CultureSpec {
            conflict_level,
            ..CultureSpec::default()
        };
        let values: Vec<f32> = (1..=120u16)
            .map(|id| personality_for(42, id, &culture).neuroticism)
            .collect();
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32
    }

    #[test]
    fn higher_conflict_widens_neuroticism_variance() {
        // #441 AC2: hoeheres Konfliktniveau → spuerbar breitere Neuroticism-Streuung.
        let low = neuroticism_variance(0.0);
        let high = neuroticism_variance(1.0);
        assert!(
            high > low * 1.3,
            "high-conflict variance {high} must clearly exceed low-conflict {low}"
        );
    }

    #[test]
    fn culture_centers_shift_trait_means() {
        // #441 AC2: hohe Innovation hebt den Openness-Mittelwert ggue niedriger Innovation.
        fn openness_mean(innovation: f32) -> f32 {
            let culture = CultureSpec {
                innovation,
                ..CultureSpec::default()
            };
            let values: Vec<f32> = (1..=120u16)
                .map(|id| personality_for(42, id, &culture).openness)
                .collect();
            values.iter().sum::<f32>() / values.len() as f32
        }
        assert!(openness_mean(0.95) > openness_mean(0.05) + 0.1);
    }

    #[test]
    fn custom_spec_derives_structure_hierarchy_roles_and_shifts() {
        let spec = GaiaSpec {
            company_name: "Custom Works AG".to_string(),
            company_type: CompanyType::Generic,
            agent_count: 8,
            seed: 1234,
            shift_model: ShiftModel::ThreeShift,
            departments: vec![
                dept("Ops", 3, &["Operator", "Coordinator"]),
                dept("Finance", 1, &["Controller"]),
            ],
            ..GaiaSpec::example()
        };

        let generated = generate(spec).unwrap();

        assert_eq!(
            generated.structure.hierarchy_root,
            "Custom Works AG Leitung"
        );
        assert_eq!(generated.structure.shift_model, ShiftModel::ThreeShift);
        assert_eq!(generated.structure.departments.len(), 2);
        assert_eq!(
            generated
                .structure
                .departments
                .iter()
                .map(|department| department.agent_count)
                .sum::<u16>(),
            8
        );
        assert!(generated.structure.departments[0]
            .roles
            .contains(&"Operator".to_string()));

        let agents: Vec<AgentConfig> = generated
            .files
            .iter()
            .filter(|file| file.relative_path.starts_with("agents"))
            .map(|file| toml::from_str(&file.contents).unwrap())
            .collect();
        assert_eq!(agents.len(), 8);
        assert_eq!(agents[0].identity.reports_to, None);
        assert!(!agents[0].identity.direct_reports.is_empty());
        assert!(agents
            .iter()
            .skip(1)
            .all(|agent| agent.identity.reports_to.is_some()));
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.identity.tier.map(|tier| tier.get()))
                .collect::<Vec<_>>(),
            vec![
                Some(1),
                Some(3),
                Some(3),
                Some(3),
                Some(3),
                Some(3),
                Some(2),
                Some(3)
            ],
            "Gaia must deterministically emit company lead=1, department lead=2, others=3"
        );
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.identity.shift_set)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 1, 2, 3, 1, 2]
        );
    }

    #[test]
    fn generated_agents_are_valid_and_use_ecs_runtime() {
        let generated = generate(GaiaSpec::example()).unwrap();
        let report = generated.validate().unwrap();

        assert_eq!(report.agents, 75);
        assert_eq!(report.daemon_max_agents, 75);
        assert_eq!(report.nightrun_max_agent_id, 75);
        assert!(generated.file(GAIA_SPEC_FILENAME).is_some());
        assert!(generated.file("company.toml").is_none());
        assert!(generated
            .files
            .iter()
            .filter(|file| file.relative_path.starts_with("agents"))
            .all(|file| file.contents.contains(RUNTIME_ECS_NATIVE)));
    }

    #[test]
    fn write_to_dir_outputs_loadable_agent_tomls() {
        let tmp = tempfile::tempdir().unwrap();
        let generated = generate(GaiaSpec::example()).unwrap();
        generated.write_to_dir(tmp.path(), false).unwrap();

        let agents = load_all_agents_with_validation(
            &tmp.path().join("agents"),
            AgentConfigValidation::with_max_agent_id(75),
        )
        .unwrap();
        assert_eq!(agents.len(), 75);
        assert_eq!(agents[0].identity.id, 1);
        assert_eq!(agents[74].identity.id, 75);
    }

    #[test]
    fn validate_output_dir_checks_written_files() {
        let tmp = tempfile::tempdir().unwrap();
        let generated = generate(GaiaSpec::example()).unwrap();
        generated.write_to_dir(tmp.path(), false).unwrap();

        let report = validate_output_dir(tmp.path()).unwrap();

        assert_eq!(report.agents, 75);
        assert_eq!(report.daemon_max_agents, 75);
        assert_eq!(report.nightrun_max_agent_id, 75);
    }

    #[test]
    fn generated_rooms_are_bidirectional_and_have_capacity() {
        let generated = generate(GaiaSpec::example()).unwrap();
        let rooms_file = generated.file("rooms.toml").unwrap();
        let rooms: BuildingConfig = toml::from_str(&rooms_file.contents).unwrap();

        rooms.validate(75).unwrap();
        let total_capacity: u32 = rooms.rooms.iter().map(|room| room.capacity as u32).sum();
        assert!(total_capacity >= 75);
    }

    #[test]
    fn daemon_and_nightrun_configs_track_agent_count() {
        let spec = GaiaSpec {
            agent_count: 120,
            seed: 99,
            ..GaiaSpec::example()
        };
        let generated = generate(spec).unwrap();
        let report = generated.validate().unwrap();

        assert_eq!(report.agents, 120);
        assert_eq!(report.daemon_max_agents, 120);
        assert_eq!(report.nightrun_max_agent_id, 120);
    }

    #[test]
    fn refuses_overwrite_unless_explicit() {
        let tmp = tempfile::tempdir().unwrap();
        let generated = generate(GaiaSpec::example()).unwrap();
        generated.write_to_dir(tmp.path(), false).unwrap();

        assert!(generated.write_to_dir(tmp.path(), false).is_err());
        assert!(generated.write_to_dir(tmp.path(), true).is_ok());
    }
}
