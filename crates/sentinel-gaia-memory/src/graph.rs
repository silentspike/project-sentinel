//! Bi-temporal relational graph for Gaia Console Memory.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{bail, Context};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const ENTITIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entities");
pub(crate) const FACT_VERSIONS: TableDefinition<&str, &[u8]> =
    TableDefinition::new("fact_versions");
pub(crate) const FACT_INDEX_SUBJECT_RELATION: TableDefinition<&str, &[u8]> =
    TableDefinition::new("fact_index_subject_relation");
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

const KEY_SEP: char = '\u{1f}';
const KEY_RANGE_END: char = '\u{10ffff}';
const META_SCHEMA_VERSION: &str = "schema_version";
const GRAPH_SCHEMA_VERSION: &[u8] = b"1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntityId(pub String);

impl EntityId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl From<&str> for EntityId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FactId(pub String);

impl FactId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn new_uuid_v7() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub attrs: BTreeMap<String, String>,
    pub created_tx_ms: u64,
    pub retired_tx_ms: Option<u64>,
}

impl Entity {
    pub fn new(
        id: impl Into<EntityId>,
        kind: impl Into<String>,
        label: impl Into<String>,
        created_tx_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
            attrs: BTreeMap::new(),
            created_tx_ms,
            retired_tx_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum FactObject {
    Entity(EntityId),
    Literal(String),
}

impl FactObject {
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSource {
    pub kind: String,
    pub uri: Option<String>,
    pub evidence_ref: Option<String>,
}

impl FactSource {
    pub fn manual() -> Self {
        Self {
            kind: "manual".to_string(),
            uri: None,
            evidence_ref: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactVersion {
    pub fact_id: FactId,
    pub subject: EntityId,
    pub relation: String,
    pub object: FactObject,
    pub valid_from_ms: u64,
    pub valid_to_ms: Option<u64>,
    pub tx_from_ms: u64,
    pub tx_to_ms: Option<u64>,
    pub source: FactSource,
    pub confidence: f32,
    pub note: Option<String>,
}

impl FactVersion {
    fn version_key(&self) -> String {
        version_key(&self.fact_id, self.tx_from_ms)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FactWrite {
    pub subject: EntityId,
    pub relation: String,
    pub object: FactObject,
    pub valid_from_ms: u64,
    pub tx_ms: u64,
    pub source: FactSource,
    pub confidence: f32,
    pub note: Option<String>,
}

impl FactWrite {
    pub fn literal(
        subject: impl Into<EntityId>,
        relation: impl Into<String>,
        value: impl Into<String>,
        valid_from_ms: u64,
        tx_ms: u64,
    ) -> Self {
        Self {
            subject: subject.into(),
            relation: relation.into(),
            object: FactObject::literal(value),
            valid_from_ms,
            tx_ms,
            source: FactSource::manual(),
            confidence: 1.0,
            note: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactQuery {
    pub subject: Option<EntityId>,
    pub relation: Option<String>,
    pub valid_at_ms: Option<u64>,
    pub as_of_tx_ms: Option<u64>,
    pub current_only: bool,
    pub include_stale: bool,
}

impl FactQuery {
    pub fn current(subject: impl Into<EntityId>, relation: impl Into<String>) -> Self {
        Self {
            subject: Some(subject.into()),
            relation: Some(relation.into()),
            valid_at_ms: None,
            as_of_tx_ms: None,
            current_only: true,
            include_stale: false,
        }
    }

    pub fn at(
        subject: impl Into<EntityId>,
        relation: impl Into<String>,
        valid_at_ms: u64,
        as_of_tx_ms: u64,
    ) -> Self {
        Self {
            subject: Some(subject.into()),
            relation: Some(relation.into()),
            valid_at_ms: Some(valid_at_ms),
            as_of_tx_ms: Some(as_of_tx_ms),
            current_only: false,
            include_stale: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactQueryResult {
    pub fact: FactVersion,
    pub is_current: bool,
    pub stale_reason: Option<String>,
}

pub struct GaiaConsoleMemoryStore {
    db: Database,
}

impl GaiaConsoleMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let db = Database::create(path).with_context(|| {
            format!(
                "failed to create/open Gaia Console Memory redb at {}",
                path.display()
            )
        })?;

        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(ENTITIES)?;
            write_txn.open_table(FACT_VERSIONS)?;
            write_txn.open_table(FACT_INDEX_SUBJECT_RELATION)?;
            let mut meta = write_txn.open_table(META)?;
            meta.insert(META_SCHEMA_VERSION, GRAPH_SCHEMA_VERSION)?;
        }
        write_txn.commit()?;

        Ok(Self { db })
    }

    pub fn upsert_entity(&self, entity: &Entity) -> anyhow::Result<()> {
        validate_key_part(&entity.id.0, "entity id")?;
        validate_key_part(&entity.kind, "entity kind")?;
        let json = serde_json::to_vec(entity).context("serialize Gaia Console Memory entity")?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(ENTITIES)?;
            table.insert(entity.id.0.as_str(), json.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn load_entity(&self, entity_id: &EntityId) -> anyhow::Result<Option<Entity>> {
        validate_key_part(&entity_id.0, "entity id")?;
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(ENTITIES)?;
        match table.get(entity_id.0.as_str())? {
            Some(guard) => Ok(Some(
                serde_json::from_slice(guard.value())
                    .context("deserialize Gaia Console Memory entity")?,
            )),
            None => Ok(None),
        }
    }

    pub fn insert_fact(&self, write: FactWrite) -> anyhow::Result<FactVersion> {
        self.insert_fact_with_id(FactId::new_uuid_v7(), write)
    }

    pub fn insert_fact_with_id(
        &self,
        fact_id: FactId,
        write: FactWrite,
    ) -> anyhow::Result<FactVersion> {
        validate_fact_write(&write)?;
        validate_key_part(&fact_id.0, "fact id")?;
        let fact = FactVersion {
            fact_id,
            subject: write.subject,
            relation: write.relation,
            object: write.object,
            valid_from_ms: write.valid_from_ms,
            valid_to_ms: None,
            tx_from_ms: write.tx_ms,
            tx_to_ms: None,
            source: write.source,
            confidence: write.confidence,
            note: write.note,
        };

        let write_txn = self.db.begin_write()?;
        write_fact_version(&write_txn, &fact)?;
        write_txn.commit()?;
        Ok(fact)
    }

    pub fn supersede_fact(&self, write: FactWrite) -> anyhow::Result<FactVersion> {
        validate_fact_write(&write)?;
        let current = self.query_facts(FactQuery::current(
            write.subject.clone(),
            write.relation.clone(),
        ))?;

        let mut closed_versions = Vec::new();
        for result in current {
            if write.valid_from_ms <= result.fact.valid_from_ms {
                bail!(
                    "supersede valid_from_ms {} must be after existing valid_from_ms {} for fact {}",
                    write.valid_from_ms,
                    result.fact.valid_from_ms,
                    result.fact.fact_id.0
                );
            }
            let mut closed = result.fact;
            closed.valid_to_ms = Some(write.valid_from_ms);
            closed.tx_from_ms = write.tx_ms;
            closed.tx_to_ms = None;
            closed_versions.push(closed);
        }

        let new_fact = FactVersion {
            fact_id: FactId::new_uuid_v7(),
            subject: write.subject,
            relation: write.relation,
            object: write.object,
            valid_from_ms: write.valid_from_ms,
            valid_to_ms: None,
            tx_from_ms: write.tx_ms,
            tx_to_ms: None,
            source: write.source,
            confidence: write.confidence,
            note: write.note,
        };

        let write_txn = self.db.begin_write()?;
        for closed in &closed_versions {
            write_fact_version(&write_txn, closed)?;
        }
        write_fact_version(&write_txn, &new_fact)?;
        write_txn.commit()?;
        Ok(new_fact)
    }

    pub fn query_facts(&self, query: FactQuery) -> anyhow::Result<Vec<FactQueryResult>> {
        validate_query(&query)?;
        let read_txn = self.db.begin_read()?;
        let mut candidates = if let (Some(subject), Some(relation)) =
            (query.subject.as_ref(), query.relation.as_ref())
        {
            read_indexed_candidates(&read_txn, subject, relation)?
        } else {
            read_all_fact_versions(&read_txn)?
        };

        if let Some(subject) = &query.subject {
            candidates.retain(|fact| fact.subject == *subject);
        }
        if let Some(relation) = &query.relation {
            candidates.retain(|fact| fact.relation == *relation);
        }

        let as_of_tx_ms = query.as_of_tx_ms.unwrap_or(u64::MAX);
        let latest = latest_versions_as_of(candidates, as_of_tx_ms);
        let mut results = Vec::new();
        for fact in latest {
            if !matches_valid_time(&fact, query.valid_at_ms) {
                continue;
            }
            let stale_reason = stale_reason(&fact);
            let is_current = stale_reason.is_none();
            if query.current_only && !is_current {
                continue;
            }
            if !query.include_stale && !is_current && query.valid_at_ms.is_none() {
                continue;
            }
            results.push(FactQueryResult {
                fact,
                is_current,
                stale_reason,
            });
        }
        results.sort_by(|a, b| {
            a.fact
                .subject
                .cmp(&b.fact.subject)
                .then(a.fact.relation.cmp(&b.fact.relation))
                .then(a.fact.valid_from_ms.cmp(&b.fact.valid_from_ms))
                .then(a.fact.fact_id.cmp(&b.fact.fact_id))
        });
        Ok(results)
    }
}

fn write_fact_version(write_txn: &WriteTransaction, fact: &FactVersion) -> anyhow::Result<()> {
    let version_key = fact.version_key();
    let index_key = fact_index_key(fact);
    let json = serde_json::to_vec(fact).context("serialize Gaia Console Memory fact version")?;
    {
        let mut table = write_txn.open_table(FACT_VERSIONS)?;
        if table.get(version_key.as_str())?.is_some() {
            bail!("Gaia Console Memory fact version {version_key} already exists");
        }
        table.insert(version_key.as_str(), json.as_slice())?;
    }
    {
        let mut table = write_txn.open_table(FACT_INDEX_SUBJECT_RELATION)?;
        table.insert(index_key.as_str(), version_key.as_bytes())?;
    }
    Ok(())
}

fn read_indexed_candidates(
    read_txn: &redb::ReadTransaction,
    subject: &EntityId,
    relation: &str,
) -> anyhow::Result<Vec<FactVersion>> {
    let index = read_txn.open_table(FACT_INDEX_SUBJECT_RELATION)?;
    let facts = read_txn.open_table(FACT_VERSIONS)?;
    let prefix = fact_index_prefix(subject, relation);
    let end = format!("{prefix}{KEY_RANGE_END}");
    let mut out = Vec::new();
    for entry in index.range(prefix.as_str()..end.as_str())? {
        let (key, value) = entry?;
        if !key.value().starts_with(&prefix) {
            break;
        }
        let version_key = std::str::from_utf8(value.value())
            .context("Gaia Console Memory fact index value is not utf-8")?;
        if let Some(guard) = facts.get(version_key)? {
            out.push(
                serde_json::from_slice(guard.value())
                    .context("deserialize indexed Gaia Console Memory fact version")?,
            );
        }
    }
    Ok(out)
}

fn read_all_fact_versions(read_txn: &redb::ReadTransaction) -> anyhow::Result<Vec<FactVersion>> {
    let table = read_txn.open_table(FACT_VERSIONS)?;
    let mut out = Vec::new();
    for entry in table.iter()? {
        let (_, value) = entry?;
        out.push(
            serde_json::from_slice(value.value())
                .context("deserialize Gaia Console Memory fact version")?,
        );
    }
    Ok(out)
}

fn latest_versions_as_of(versions: Vec<FactVersion>, as_of_tx_ms: u64) -> Vec<FactVersion> {
    let mut latest: HashMap<FactId, FactVersion> = HashMap::new();
    for fact in versions {
        if fact.tx_from_ms > as_of_tx_ms {
            continue;
        }
        if fact.tx_to_ms.is_some_and(|tx_to| as_of_tx_ms >= tx_to) {
            continue;
        }
        match latest.get(&fact.fact_id) {
            Some(existing) if existing.tx_from_ms >= fact.tx_from_ms => {}
            _ => {
                latest.insert(fact.fact_id.clone(), fact);
            }
        }
    }
    latest.into_values().collect()
}

fn matches_valid_time(fact: &FactVersion, valid_at_ms: Option<u64>) -> bool {
    match valid_at_ms {
        Some(valid_at_ms) => {
            fact.valid_from_ms <= valid_at_ms
                && fact
                    .valid_to_ms
                    .map(|valid_to_ms| valid_at_ms < valid_to_ms)
                    .unwrap_or(true)
        }
        None => fact.valid_to_ms.is_none(),
    }
}

fn stale_reason(fact: &FactVersion) -> Option<String> {
    if fact.tx_to_ms.is_some() {
        Some("transaction_closed".to_string())
    } else if fact.valid_to_ms.is_some() {
        Some("validity_closed".to_string())
    } else {
        None
    }
}

fn validate_fact_write(write: &FactWrite) -> anyhow::Result<()> {
    validate_key_part(&write.subject.0, "fact subject")?;
    validate_key_part(&write.relation, "fact relation")?;
    if write.relation.trim().is_empty() {
        bail!("fact relation must not be empty");
    }
    if !(0.0..=1.0).contains(&write.confidence) {
        bail!("fact confidence must be within 0.0..=1.0");
    }
    Ok(())
}

fn validate_query(query: &FactQuery) -> anyhow::Result<()> {
    if let Some(subject) = &query.subject {
        validate_key_part(&subject.0, "query subject")?;
    }
    if let Some(relation) = &query.relation {
        validate_key_part(relation, "query relation")?;
    }
    Ok(())
}

fn validate_key_part(value: &str, label: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{label} must not be empty");
    }
    if value.contains(KEY_SEP) {
        bail!("{label} must not contain the internal key separator");
    }
    Ok(())
}

fn version_key(fact_id: &FactId, tx_from_ms: u64) -> String {
    format!("{}{}{:020}", fact_id.0, KEY_SEP, tx_from_ms)
}

fn fact_index_prefix(subject: &EntityId, relation: &str) -> String {
    format!("{}{}{}{}", subject.0, KEY_SEP, relation, KEY_SEP)
}

fn fact_index_key(fact: &FactVersion) -> String {
    format!(
        "{}{}{}{:020}",
        fact_index_prefix(&fact.subject, &fact.relation),
        fact.fact_id.0,
        KEY_SEP,
        fact.tx_from_ms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> GaiaConsoleMemoryStore {
        GaiaConsoleMemoryStore::open(dir.path().join("gaia_console_memory.redb")).unwrap()
    }

    fn literal_value(result: &FactQueryResult) -> &str {
        match &result.fact.object {
            FactObject::Literal(value) => value,
            other => panic!("expected literal fact object, got {other:?}"),
        }
    }

    #[test]
    fn graph_stores_and_queries_company_and_user_facts() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);

        store
            .upsert_entity(&Entity::new(
                "company:sentinel",
                "company",
                "Project Sentinel",
                1_000,
            ))
            .unwrap();
        store
            .upsert_entity(&Entity::new("user:operator", "user", "Operator", 1_000))
            .unwrap();

        store
            .insert_fact(FactWrite::literal(
                "company:sentinel",
                "headquarters",
                "Nuremberg",
                1_000,
                1_010,
            ))
            .unwrap();
        store
            .insert_fact(FactWrite::literal(
                "user:operator",
                "prefers",
                "evidence-first plans",
                1_100,
                1_110,
            ))
            .unwrap();

        let company = store
            .query_facts(FactQuery::at(
                "company:sentinel",
                "headquarters",
                1_000,
                1_200,
            ))
            .unwrap();
        assert_eq!(company.len(), 1);
        assert_eq!(literal_value(&company[0]), "Nuremberg");

        let user = store
            .query_facts(FactQuery::current("user:operator", "prefers"))
            .unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(literal_value(&user[0]), "evidence-first plans");
        assert!(user[0].is_current);
    }

    #[test]
    fn supersede_keeps_old_fact_queryable_and_current_query_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);

        store
            .insert_fact(FactWrite::literal(
                "company:sentinel",
                "operating_mode",
                "prototype",
                1_000,
                1_000,
            ))
            .unwrap();

        let replacement = store
            .supersede_fact(FactWrite::literal(
                "company:sentinel",
                "operating_mode",
                "production-grade",
                2_000,
                2_000,
            ))
            .unwrap();
        assert_eq!(literal_from_fact(&replacement), "production-grade");

        let current = store
            .query_facts(FactQuery::current("company:sentinel", "operating_mode"))
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(literal_value(&current[0]), "production-grade");
        assert!(current[0].is_current);

        let historical_after_supersede = store
            .query_facts(FactQuery::at(
                "company:sentinel",
                "operating_mode",
                1_500,
                2_500,
            ))
            .unwrap();
        assert_eq!(historical_after_supersede.len(), 1);
        assert_eq!(literal_value(&historical_after_supersede[0]), "prototype");
        assert!(!historical_after_supersede[0].is_current);
        assert_eq!(
            historical_after_supersede[0].stale_reason.as_deref(),
            Some("validity_closed")
        );

        let historical_before_supersede = store
            .query_facts(FactQuery::at(
                "company:sentinel",
                "operating_mode",
                1_500,
                1_500,
            ))
            .unwrap();
        assert_eq!(historical_before_supersede.len(), 1);
        assert_eq!(literal_value(&historical_before_supersede[0]), "prototype");
        assert!(historical_before_supersede[0].is_current);
    }

    fn literal_from_fact(fact: &FactVersion) -> &str {
        match &fact.object {
            FactObject::Literal(value) => value,
            other => panic!("expected literal fact object, got {other:?}"),
        }
    }
}
