#![doc = "Local persistence scaffolding for missive."]

pub mod migrations;
pub mod paths;
pub mod repository;

pub use migrations::{
    AppliedMigration, CURRENT_SCHEMA_VERSION, Migration, MigrationReport, SQLITE_APPLICATION_ID,
    applied_migrations, embedded_migrations, migrate_connection, migrate_database,
    open_sqlite_database, schema_version,
};
pub use paths::{
    DEFAULT_DATABASE_FILE, ENV_HOME, ENV_MISSIVE_HOME, ENV_XDG_CACHE_HOME, ENV_XDG_DATA_HOME,
    ENV_XDG_STATE_HOME, ProcessLock, ProcessLockKind, StatePathResolver, StatePathSource,
    StatePaths, StatePlatform,
};
pub use repository::{
    AdapterBindingId, AgentRecord, AgentSource, AgentUpsert, ContextRecord, ContextState,
    ContextUpsert, EventInsert, EventRecord, GatewayJobId, GatewayJobRecord, GatewayJobState,
    GatewayJobUpsert, GroupMemberRecord, GroupMemberUpsert, GroupRecord, GroupUpsert, Store,
    StoreTransaction, TaskRecord, TaskSource, TaskState, TaskUpsert,
};

/// Cargo package name for this crate.
pub const CRATE_NAME: &str = "missive-store";

/// Short description of this crate's target responsibility.
pub const CRATE_PURPOSE: &str = "state paths, process locks, SQLite migrations and repository APIs";

/// Returns metadata for this crate.
#[must_use]
pub const fn crate_info() -> missive_core::CrateInfo {
    missive_core::CrateInfo::new(CRATE_NAME, CRATE_PURPOSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_info_describes_store_layer() {
        let info = crate_info();

        assert_eq!(info.name(), CRATE_NAME);
        assert!(info.purpose().contains("SQLite"));
    }
}
