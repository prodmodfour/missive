//! SQLite migration strategy for the local missive store.
//!
//! Migrations are versioned SQL files embedded from `crates/missive-store/migrations`.
//! The runner bootstraps a small `schema_migrations` ledger, verifies checksums
//! for already-applied migrations, and applies pending migrations in version
//! order inside SQLite transactions. Repository APIs call this module before
//! reading or mutating profile state.

use std::collections::BTreeMap;
use std::path::Path;

use missive_core::{MissiveError, Result};
use rusqlite::{Connection, OptionalExtension, params};

/// Current schema version created by the embedded migrations.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// SQLite application id reserved for missive databases (`miss` as big-endian ASCII).
pub const SQLITE_APPLICATION_ID: i32 = 0x6d69_7373;

const MIGRATION_LEDGER_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    checksum TEXT NOT NULL,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
"#;

const INITIAL_SCHEMA_SQL: &str = include_str!("../migrations/0001_initial_schema.sql");
const GATEWAY_SESSIONS_SQL: &str = include_str!("../migrations/0002_gateway_sessions.sql");
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: INITIAL_SCHEMA_SQL,
    },
    Migration {
        version: 2,
        name: "gateway_sessions",
        sql: GATEWAY_SESSIONS_SQL,
    },
];

/// An embedded store schema migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

impl Migration {
    /// Monotonic migration version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Stable migration name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// SQL body applied for this migration.
    #[must_use]
    pub const fn sql(&self) -> &'static str {
        self.sql
    }

    /// Stable checksum recorded in the local migration ledger.
    #[must_use]
    pub fn checksum(&self) -> String {
        checksum_sql(self.sql)
    }
}

/// A migration row recorded in `schema_migrations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    version: u32,
    name: String,
    checksum: String,
    applied_at: String,
}

impl AppliedMigration {
    /// Recorded migration version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Recorded migration name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Recorded checksum.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    /// UTC timestamp recorded by SQLite when the migration was applied.
    #[must_use]
    pub fn applied_at(&self) -> &str {
        &self.applied_at
    }
}

/// Result summary from a migration run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    current_version: u32,
    applied: Vec<AppliedMigration>,
}

impl MigrationReport {
    /// Latest schema version known after the run.
    #[must_use]
    pub const fn current_version(&self) -> u32 {
        self.current_version
    }

    /// Migrations applied during this run. Empty means the database was already current.
    #[must_use]
    pub fn applied(&self) -> &[AppliedMigration] {
        &self.applied
    }
}

/// Returns the embedded migrations in the order they are applied.
#[must_use]
pub const fn embedded_migrations() -> &'static [Migration] {
    MIGRATIONS
}

/// Opens a SQLite database file with missive connection pragmas enabled.
pub fn open_sqlite_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).map_err(|error| {
        MissiveError::storage(format!("opening SQLite database {}", path.display()))
            .with_source(error)
            .with_help("Check the profile state directory and storage.database_path permissions.")
    })?;
    configure_connection(&connection)?;
    Ok(connection)
}

/// Opens a SQLite database file and applies all pending embedded migrations.
pub fn migrate_database(path: &Path) -> Result<MigrationReport> {
    let mut connection = open_sqlite_database(path)?;
    migrate_connection(&mut connection)
}

/// Applies all pending embedded migrations to an existing connection.
pub fn migrate_connection(connection: &mut Connection) -> Result<MigrationReport> {
    configure_connection(connection)?;
    bootstrap_migration_ledger(connection)?;
    verify_existing_migrations(connection)?;

    let existing = applied_migration_map(connection)?;
    let mut applied = Vec::new();

    for migration in MIGRATIONS {
        if existing.contains_key(&migration.version()) {
            continue;
        }
        applied.push(apply_migration(connection, migration)?);
    }

    set_user_version(connection, CURRENT_SCHEMA_VERSION)?;

    Ok(MigrationReport {
        current_version: schema_version(connection)?.unwrap_or_default(),
        applied,
    })
}

/// Returns the latest schema version recorded by `PRAGMA user_version`.
pub fn schema_version(connection: &Connection) -> Result<Option<u32>> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| storage_error("reading SQLite user_version", error))?;

    if version <= 0 {
        Ok(None)
    } else {
        u32::try_from(version).map(Some).map_err(|error| {
            MissiveError::storage(format!(
                "SQLite user_version {version} cannot be represented as a missive schema version"
            ))
            .with_source(error)
        })
    }
}

/// Reads the migration ledger. Returns an empty list when the ledger has not been bootstrapped.
pub fn applied_migrations(connection: &Connection) -> Result<Vec<AppliedMigration>> {
    if !schema_migration_table_exists(connection)? {
        return Ok(Vec::new());
    }
    read_applied_migrations(connection)
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| storage_error("configuring SQLite busy timeout", error))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| storage_error("enabling SQLite foreign keys", error))?;
    connection
        .pragma_update(None, "application_id", SQLITE_APPLICATION_ID)
        .map_err(|error| storage_error("setting SQLite application_id", error))?;

    Ok(())
}

fn bootstrap_migration_ledger(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(MIGRATION_LEDGER_SQL)
        .map_err(|error| storage_error("bootstrapping schema_migrations", error))
}

fn verify_existing_migrations(connection: &Connection) -> Result<()> {
    for applied in applied_migrations(connection)? {
        let Some(migration) = MIGRATIONS
            .iter()
            .find(|migration| migration.version() == applied.version())
        else {
            return Err(MissiveError::storage(format!(
                "database contains unknown future migration version {} ({})",
                applied.version(),
                applied.name()
            ))
            .with_help(
                "Open this database with a newer missive binary, or use a profile database created by this version.",
            ));
        };

        if applied.name() != migration.name() || applied.checksum() != migration.checksum() {
            return Err(MissiveError::storage(format!(
                "migration {} checksum mismatch for {}",
                applied.version(),
                applied.name()
            ))
            .with_help(
                "Do not edit applied migration files; create a new migration for schema changes.",
            ));
        }
    }

    Ok(())
}

fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<AppliedMigration> {
    let checksum = migration.checksum();
    let transaction = connection
        .transaction()
        .map_err(|error| storage_error("starting SQLite migration transaction", error))?;

    transaction
        .execute_batch(migration.sql())
        .map_err(|error| {
            storage_error(
                &format!(
                    "applying SQLite migration {:04}_{}",
                    migration.version(),
                    migration.name()
                ),
                error,
            )
        })?;
    transaction
        .execute(
            "INSERT INTO schema_migrations (version, name, checksum) VALUES (?1, ?2, ?3)",
            params![migration.version(), migration.name(), checksum],
        )
        .map_err(|error| storage_error("recording SQLite migration", error))?;
    transaction
        .commit()
        .map_err(|error| storage_error("committing SQLite migration transaction", error))?;

    let applied = connection
        .query_row(
            "SELECT version, name, checksum, applied_at FROM schema_migrations WHERE version = ?1",
            params![migration.version()],
            read_applied_migration_row,
        )
        .map_err(|error| storage_error("reading applied SQLite migration", error))?;

    Ok(applied)
}

fn set_user_version(connection: &Connection, version: u32) -> Result<()> {
    connection
        .pragma_update(None, "user_version", version)
        .map_err(|error| storage_error("setting SQLite user_version", error))
}

fn applied_migration_map(connection: &Connection) -> Result<BTreeMap<u32, AppliedMigration>> {
    Ok(applied_migrations(connection)?
        .into_iter()
        .map(|migration| (migration.version(), migration))
        .collect())
}

fn read_applied_migrations(connection: &Connection) -> Result<Vec<AppliedMigration>> {
    let mut statement = connection
        .prepare(
            "SELECT version, name, checksum, applied_at FROM schema_migrations ORDER BY version",
        )
        .map_err(|error| storage_error("preparing schema_migrations query", error))?;
    let rows = statement
        .query_map([], read_applied_migration_row)
        .map_err(|error| storage_error("querying schema_migrations", error))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| storage_error("reading schema_migrations rows", error))
}

fn read_applied_migration_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppliedMigration> {
    Ok(AppliedMigration {
        version: row.get(0)?,
        name: row.get(1)?,
        checksum: row.get(2)?,
        applied_at: row.get(3)?,
    })
}

fn schema_migration_table_exists(connection: &Connection) -> Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map(|value| value.is_some())
        .map_err(|error| storage_error("checking for schema_migrations", error))
}

fn checksum_sql(sql: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in sql.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn storage_error(action: &str, error: rusqlite::Error) -> MissiveError {
    MissiveError::storage(format!("{action}: {error}"))
        .with_source(error)
        .with_help("Inspect the SQLite database path, migrations, and profile state permissions.")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rusqlite::params;
    use tempfile::tempdir;

    use super::*;

    const REQUIRED_TABLES: &[&str] = &[
        "agents",
        "contexts",
        "tasks",
        "messages",
        "artifacts",
        "events",
        "groups",
        "group_members",
        "auth_refs",
        "push_configs",
        "gateway_jobs",
        "gateway_sessions",
        "adapter_bindings",
    ];

    #[test]
    fn fresh_database_migration_succeeds_against_temp_file() {
        let temp = tempdir().expect("tempdir");
        let database_path = temp.path().join("missive.sqlite3");

        let report = migrate_database(&database_path).expect("migration succeeds");

        assert_eq!(report.current_version(), CURRENT_SCHEMA_VERSION);
        assert_eq!(report.applied().len(), 2);
        assert_eq!(report.applied()[0].version(), 1);
        assert_eq!(report.applied()[1].version(), 2);
        assert!(database_path.exists());
    }

    #[test]
    fn migration_creates_required_tables_and_indexes() {
        let mut connection = Connection::open_in_memory().expect("in-memory db");
        migrate_connection(&mut connection).expect("migration succeeds");

        let tables = schema_names(&connection, "table");
        for table in REQUIRED_TABLES {
            assert!(tables.contains(*table), "missing table {table}");
        }
        assert!(tables.contains("schema_migrations"));

        let indexes = schema_names(&connection, "index");
        assert!(indexes.contains("idx_tasks_agent_state"));
        assert!(indexes.contains("idx_events_context_task"));
        assert!(indexes.contains("idx_gateway_jobs_state_next_run"));
        assert!(indexes.contains("idx_gateway_sessions_source_agent"));
    }

    #[test]
    fn migration_is_idempotent_and_records_checksum() {
        let mut connection = Connection::open_in_memory().expect("in-memory db");

        let first = migrate_connection(&mut connection).expect("first migration succeeds");
        let second = migrate_connection(&mut connection).expect("second migration succeeds");
        let applied = applied_migrations(&connection).expect("ledger reads");

        assert_eq!(first.applied().len(), 2);
        assert!(second.applied().is_empty());
        assert_eq!(applied.len(), 2);
        assert_eq!(applied[0].name(), "initial_schema");
        assert_eq!(applied[0].checksum(), embedded_migrations()[0].checksum());
        assert_eq!(applied[1].name(), "gateway_sessions");
        assert_eq!(applied[1].checksum(), embedded_migrations()[1].checksum());
        assert_eq!(schema_version(&connection).expect("version"), Some(2));
    }

    #[test]
    fn migration_detects_checksum_mismatch() {
        let mut connection = Connection::open_in_memory().expect("in-memory db");
        migrate_connection(&mut connection).expect("migration succeeds");
        connection
            .execute(
                "UPDATE schema_migrations SET checksum = 'fnv1a64:0000000000000000' WHERE version = 1",
                [],
            )
            .expect("tamper checksum");

        let error = migrate_connection(&mut connection).expect_err("checksum mismatch fails");

        assert_eq!(error.category(), missive_core::ErrorCategory::Storage);
        assert!(error.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn foreign_keys_and_json_constraints_are_active() {
        let mut connection = Connection::open_in_memory().expect("in-memory db");
        migrate_connection(&mut connection).expect("migration succeeds");

        let missing_agent = connection
            .execute(
                "INSERT INTO tasks (task_id, agent_alias, state) VALUES (?1, ?2, ?3)",
                params!["task-1", "missing", "submitted"],
            )
            .expect_err("missing agent should violate foreign key");
        assert!(missing_agent.to_string().contains("FOREIGN KEY"));

        connection
            .execute(
                "INSERT INTO agents (alias, base_url) VALUES (?1, ?2)",
                params!["echo", "http://127.0.0.1:8080"],
            )
            .expect("agent insert succeeds");
        let invalid_json = connection
            .execute(
                "INSERT INTO messages (message_id, agent_alias, direction, content_json) VALUES (?1, ?2, ?3, ?4)",
                params!["msg-1", "echo", "request", "not-json"],
            )
            .expect_err("invalid message JSON should fail");
        assert!(invalid_json.to_string().contains("CHECK"));
    }

    #[test]
    fn storage_docs_cover_required_tables_and_retention_notes() {
        let docs = include_str!("../../../docs/storage.md");

        for table in REQUIRED_TABLES {
            assert!(docs.contains(table), "storage docs should mention {table}");
        }
        assert!(docs.contains("Retention notes"));
    }

    fn schema_names(connection: &Connection, schema_type: &str) -> BTreeSet<String> {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type = ?1 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("prepare schema query");
        statement
            .query_map(params![schema_type], |row| row.get::<_, String>(0))
            .expect("query schema")
            .collect::<rusqlite::Result<BTreeSet<_>>>()
            .expect("schema rows")
    }
}
