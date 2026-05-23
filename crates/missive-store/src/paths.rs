//! Local state path resolution and process-level lock files.
//!
//! This module deliberately resolves paths without opening databases or running
//! migrations. Later repository APIs can depend on this contract to keep runtime
//! state outside the source tree by default and to coordinate state mutation with
//! the gateway daemon.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use missive_core::{LoadedConfig, MissiveConfig, MissiveError, Result, StorageConfig};

/// Environment variable that overrides all local state roots.
pub const ENV_MISSIVE_HOME: &str = "MISSIVE_HOME";

/// XDG data-home environment variable.
pub const ENV_XDG_DATA_HOME: &str = "XDG_DATA_HOME";

/// XDG state-home environment variable.
pub const ENV_XDG_STATE_HOME: &str = "XDG_STATE_HOME";

/// XDG cache-home environment variable.
pub const ENV_XDG_CACHE_HOME: &str = "XDG_CACHE_HOME";

/// Home directory environment variable used for platform fallbacks.
pub const ENV_HOME: &str = "HOME";

/// Default SQLite database filename inside a profile state directory.
pub const DEFAULT_DATABASE_FILE: &str = "missive.sqlite3";

const PROFILE_DIR: &str = "profiles";
const DATA_DIR: &str = "data";
const STATE_DIR: &str = "state";
const CACHE_DIR: &str = "cache";
const LOCKS_DIR: &str = "locks";
const PROJECT_DIR: &str = "missive";

/// Platform family used by state path fallback rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatePlatform {
    /// Linux and other freedesktop/XDG-style Unix environments.
    Linux,
    /// macOS fallback directories under `~/Library`.
    MacOs,
    /// Other platforms use the Linux/XDG fallback unless future work adds a
    /// platform-specific service/runtime policy.
    Other,
}

impl StatePlatform {
    /// Detects the platform family for the running process.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Source of the resolved state roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatePathSource {
    /// `MISSIVE_HOME` supplied all roots.
    MissiveHome,
    /// XDG environment variables or XDG home fallbacks supplied roots.
    Xdg,
    /// Platform-specific non-XDG fallback supplied roots.
    PlatformFallback,
}

/// Resolved local directories and database path for one selected profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    profile: String,
    source: StatePathSource,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    locks_dir: PathBuf,
    database_path: PathBuf,
}

impl StatePaths {
    fn new(
        profile: String,
        source: StatePathSource,
        data_dir: PathBuf,
        state_dir: PathBuf,
        cache_dir: PathBuf,
        database_path: PathBuf,
    ) -> Self {
        let locks_dir = state_dir.join(LOCKS_DIR);
        Self {
            profile,
            source,
            data_dir,
            state_dir,
            cache_dir,
            locks_dir,
            database_path,
        }
    }

    /// Selected profile name used for these paths.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Source class used to resolve these paths.
    #[must_use]
    pub const fn source(&self) -> StatePathSource {
        self.source
    }

    /// Directory for durable data such as cached Agent Cards and future exports.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Directory for mutable runtime state such as SQLite databases and journals.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Directory for disposable caches.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Directory that contains process lock files.
    #[must_use]
    pub fn locks_dir(&self) -> &Path {
        &self.locks_dir
    }

    /// Resolved SQLite database path for the selected profile.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Lock-file path for a process lock kind.
    #[must_use]
    pub fn lock_path(&self, kind: ProcessLockKind) -> PathBuf {
        self.locks_dir.join(kind.file_name())
    }

    /// Creates the directory tree required before opening stores, caches, or locks.
    pub fn ensure_directories(&self) -> Result<()> {
        create_dir_all(&self.data_dir, "creating data directory")?;
        create_dir_all(&self.state_dir, "creating state directory")?;
        create_dir_all(&self.cache_dir, "creating cache directory")?;
        create_dir_all(&self.locks_dir, "creating lock directory")?;

        if let Some(parent) = self.database_path.parent() {
            create_dir_all(parent, "creating database parent directory")?;
        }

        Ok(())
    }
}

/// Process lock files used to coordinate local state and gateway operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLockKind {
    /// Exclusive lock for local state mutations and future migrations.
    StateMutation,
    /// Exclusive lock for a running gateway daemon profile.
    Gateway,
}

impl ProcessLockKind {
    /// Stable lock filename.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::StateMutation => "state.lock",
            Self::Gateway => "gateway.lock",
        }
    }

    /// Human-readable label used in diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::StateMutation => "state mutation",
            Self::Gateway => "gateway",
        }
    }
}

impl fmt::Display for ProcessLockKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Held process lock. The lock is released when this value is dropped.
#[derive(Debug)]
pub struct ProcessLock {
    kind: ProcessLockKind,
    path: PathBuf,
    file: File,
}

impl ProcessLock {
    /// Acquires an exclusive process lock, blocking until it becomes available.
    pub fn acquire(paths: &StatePaths, kind: ProcessLockKind) -> Result<Self> {
        let path = paths.lock_path(kind);
        let mut file = open_lock_file(paths, kind, &path)?;
        FileExt::lock_exclusive(&file).map_err(|error| {
            MissiveError::io(format!("locking {} lock {}", kind, path.display()), error)
                .with_help("Check permissions on the missive state directory.")
        })?;
        annotate_lock_file(&mut file, kind, &path)?;

        Ok(Self { kind, path, file })
    }

    /// Tries to acquire an exclusive process lock without blocking.
    pub fn try_acquire(paths: &StatePaths, kind: ProcessLockKind) -> Result<Self> {
        let path = paths.lock_path(kind);
        let mut file = open_lock_file(paths, kind, &path)?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            if is_lock_contended(&error) {
                MissiveError::storage(format!(
                    "{} lock is already held at {}",
                    kind,
                    path.display()
                ))
                .with_source(error)
                .with_help(
                    "Wait for the other missive process to finish, or inspect the process that owns the lock.",
                )
            } else {
                MissiveError::io(format!("locking {} lock {}", kind, path.display()), error)
                    .with_help("Check permissions on the missive state directory.")
            }
        })?;
        annotate_lock_file(&mut file, kind, &path)?;

        Ok(Self { kind, path, file })
    }

    /// Kind of lock held by this guard.
    #[must_use]
    pub const fn kind(&self) -> ProcessLockKind {
        self.kind
    }

    /// Filesystem path of the held lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Testable state path resolver.
#[derive(Debug, Clone)]
pub struct StatePathResolver {
    env: BTreeMap<String, String>,
    platform: StatePlatform,
}

impl Default for StatePathResolver {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            platform: StatePlatform::current(),
        }
    }
}

impl StatePathResolver {
    /// Creates a resolver with no environment overrides and the current platform.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a resolver from the current process environment and platform.
    #[must_use]
    pub fn from_process() -> Self {
        Self::new().with_env(env::vars())
    }

    /// Sets an environment map for deterministic tests.
    #[must_use]
    pub fn with_env<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env = env
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
        self
    }

    /// Sets the platform fallback rules for deterministic tests.
    #[must_use]
    pub const fn with_platform(mut self, platform: StatePlatform) -> Self {
        self.platform = platform;
        self
    }

    /// Resolves paths for the selected profile in a loaded configuration.
    pub fn resolve_loaded(&self, loaded: &LoadedConfig) -> Result<StatePaths> {
        self.resolve_config(&loaded.config, &loaded.selected_profile)
    }

    /// Resolves paths for a selected profile in a configuration.
    pub fn resolve_config(&self, config: &MissiveConfig, profile: &str) -> Result<StatePaths> {
        let profile_config = config.profile(profile)?;
        let storage = profile_config.storage.as_ref().unwrap_or(&config.storage);
        self.resolve_storage(profile, storage)
    }

    /// Resolves paths from a storage config without requiring a full config.
    pub fn resolve_storage(&self, profile: &str, storage: &StorageConfig) -> Result<StatePaths> {
        validate_profile_segment(profile)?;

        let roots = self.resolve_roots()?;
        let data_dir = roots.data_root.join(PROFILE_DIR).join(profile);
        let state_dir = roots.state_root.join(PROFILE_DIR).join(profile);
        let cache_dir = roots.cache_root.join(PROFILE_DIR).join(profile);
        let database_path = if let Some(path) = &storage.database_path {
            self.resolve_database_path(path, &state_dir)?
        } else {
            state_dir.join(DEFAULT_DATABASE_FILE)
        };

        Ok(StatePaths::new(
            profile.to_owned(),
            roots.source,
            data_dir,
            state_dir,
            cache_dir,
            database_path,
        ))
    }

    fn resolve_roots(&self) -> Result<StateRoots> {
        if let Some(root) = self.env_path(ENV_MISSIVE_HOME)? {
            return Ok(StateRoots {
                source: StatePathSource::MissiveHome,
                data_root: root.join(DATA_DIR),
                state_root: root.join(STATE_DIR),
                cache_root: root.join(CACHE_DIR),
            });
        }

        match self.platform {
            StatePlatform::Linux | StatePlatform::Other => self.resolve_xdg_roots(),
            StatePlatform::MacOs => self.resolve_macos_roots(),
        }
    }

    fn resolve_xdg_roots(&self) -> Result<StateRoots> {
        let home = self.home_dir()?;
        let data_home = self
            .env_path(ENV_XDG_DATA_HOME)?
            .unwrap_or_else(|| home.join(".local").join("share"));
        let state_home = self
            .env_path(ENV_XDG_STATE_HOME)?
            .unwrap_or_else(|| home.join(".local").join("state"));
        let cache_home = self
            .env_path(ENV_XDG_CACHE_HOME)?
            .unwrap_or_else(|| home.join(".cache"));

        Ok(StateRoots {
            source: StatePathSource::Xdg,
            data_root: data_home.join(PROJECT_DIR),
            state_root: state_home.join(PROJECT_DIR),
            cache_root: cache_home.join(PROJECT_DIR),
        })
    }

    fn resolve_macos_roots(&self) -> Result<StateRoots> {
        if self.env.contains_key(ENV_XDG_DATA_HOME)
            || self.env.contains_key(ENV_XDG_STATE_HOME)
            || self.env.contains_key(ENV_XDG_CACHE_HOME)
        {
            return self.resolve_xdg_roots();
        }

        let home = self.home_dir()?;
        let application_support = home
            .join("Library")
            .join("Application Support")
            .join(PROJECT_DIR);
        let cache_root = home.join("Library").join("Caches").join(PROJECT_DIR);

        Ok(StateRoots {
            source: StatePathSource::PlatformFallback,
            data_root: application_support.join(DATA_DIR),
            state_root: application_support.join(STATE_DIR),
            cache_root,
        })
    }

    fn resolve_database_path(&self, path: &Path, state_dir: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }

        let path_text = path.to_string_lossy();
        if path_text == "~" {
            return self.home_dir();
        }
        if let Some(stripped) = path_text.strip_prefix("~/") {
            return Ok(self.home_dir()?.join(stripped));
        }
        if path_text.starts_with('~') {
            return Err(MissiveError::config(format!(
                "storage.database_path {:?} uses unsupported home expansion syntax",
                path.display()
            ))
            .with_help("Use ~/path for the current home directory, an absolute path, or a path relative to the profile state directory."));
        }

        validate_relative_database_path(path)?;
        Ok(state_dir.join(path))
    }

    fn env_path(&self, key: &str) -> Result<Option<PathBuf>> {
        let Some(value) = self.env.get(key) else {
            return Ok(None);
        };
        if value.trim().is_empty() {
            return Err(MissiveError::config(format!("{key} is set but empty"))
                .with_help("Unset the variable or set it to an absolute directory path."));
        }

        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(MissiveError::config(format!(
                "{key} must be an absolute directory path"
            ))
            .with_help("Use an absolute path so missive never writes runtime state into the source tree by accident."));
        }

        Ok(Some(path))
    }

    fn home_dir(&self) -> Result<PathBuf> {
        self.env_path(ENV_HOME)?.ok_or_else(|| {
            MissiveError::config(format!(
                "{ENV_HOME} is required when {ENV_MISSIVE_HOME} is not set"
            ))
            .with_help("Set MISSIVE_HOME to an absolute directory, or provide HOME for platform fallback state paths.")
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateRoots {
    source: StatePathSource,
    data_root: PathBuf,
    state_root: PathBuf,
    cache_root: PathBuf,
}

fn open_lock_file(paths: &StatePaths, kind: ProcessLockKind, path: &Path) -> Result<File> {
    create_dir_all(paths.locks_dir(), "creating lock directory")?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| {
            MissiveError::io(format!("opening {} lock {}", kind, path.display()), error)
                .with_help("Check permissions on the missive state directory.")
        })
}

fn annotate_lock_file(file: &mut File, kind: ProcessLockKind, path: &Path) -> Result<()> {
    let pid = std::process::id();
    file.set_len(0).map_err(|error| {
        MissiveError::io(
            format!("truncating {} lock {}", kind, path.display()),
            error,
        )
    })?;
    file.seek(SeekFrom::Start(0)).map_err(|error| {
        MissiveError::io(format!("rewinding {} lock {}", kind, path.display()), error)
    })?;
    writeln!(file, "kind={kind}").map_err(|error| {
        MissiveError::io(format!("writing {} lock {}", kind, path.display()), error)
    })?;
    writeln!(file, "pid={pid}").map_err(|error| {
        MissiveError::io(format!("writing {} lock {}", kind, path.display()), error)
    })?;
    file.sync_data().map_err(|error| {
        MissiveError::io(format!("syncing {} lock {}", kind, path.display()), error)
    })?;

    Ok(())
}

fn create_dir_all(path: &Path, action: &str) -> Result<()> {
    fs::create_dir_all(path)
        .map_err(|error| MissiveError::io(format!("{action} {}", path.display()), error))
}

fn is_lock_contended(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists
    )
}

fn validate_relative_database_path(path: &Path) -> Result<()> {
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(MissiveError::config(format!(
                "storage.database_path {:?} cannot escape the profile state directory",
                path.display()
            ))
            .with_help("Use a simple relative filename, a nested relative path without '..', or an absolute path."));
        }
    }

    Ok(())
}

fn validate_profile_segment(profile: &str) -> Result<()> {
    let valid_len = !profile.is_empty() && profile.len() <= 63;
    let bytes = profile.as_bytes();
    let valid_edges = bytes
        .first()
        .zip(bytes.last())
        .is_some_and(|(first, last)| {
            is_ascii_lower_alphanumeric(*first) && is_ascii_lower_alphanumeric(*last)
        });
    let valid_body = bytes
        .iter()
        .all(|byte| is_ascii_lower_alphanumeric(*byte) || matches!(*byte, b'-' | b'_' | b'.'));

    if !(valid_len && valid_edges && valid_body) {
        return Err(MissiveError::config(format!(
            "profile {profile:?} cannot be used as a state path segment"
        ))
        .with_help("Use a validated missive profile name such as default, ci, or local-dev."));
    }

    Ok(())
}

const fn is_ascii_lower_alphanumeric(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9')
}

#[cfg(test)]
mod tests {
    use missive_core::{LoadedConfig, MissiveConfig, ProfileConfig, StorageBackend};
    use tempfile::tempdir;

    use super::*;

    fn default_config() -> MissiveConfig {
        let config = MissiveConfig::default();
        config.validate().expect("default config should validate");
        config
    }

    #[test]
    fn missive_home_overrides_all_state_roots() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("missive-home");
        let config = default_config();

        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, root.display().to_string())])
            .with_platform(StatePlatform::Linux)
            .resolve_config(&config, "default")
            .expect("paths resolve");

        assert_eq!(paths.source(), StatePathSource::MissiveHome);
        assert_eq!(paths.profile(), "default");
        assert_eq!(
            paths.data_dir(),
            root.join(DATA_DIR).join(PROFILE_DIR).join("default")
        );
        assert_eq!(
            paths.state_dir(),
            root.join(STATE_DIR).join(PROFILE_DIR).join("default")
        );
        assert_eq!(
            paths.cache_dir(),
            root.join(CACHE_DIR).join(PROFILE_DIR).join("default")
        );
        assert_eq!(paths.locks_dir(), paths.state_dir().join(LOCKS_DIR));
        assert_eq!(
            paths.database_path(),
            paths.state_dir().join(DEFAULT_DATABASE_FILE)
        );
    }

    #[test]
    fn linux_xdg_environment_paths_are_profile_specific() {
        let temp = tempdir().expect("tempdir");
        let config = default_config();
        let data_home = temp.path().join("data-home");
        let state_home = temp.path().join("state-home");
        let cache_home = temp.path().join("cache-home");

        let paths = StatePathResolver::new()
            .with_platform(StatePlatform::Linux)
            .with_env([
                (ENV_HOME, temp.path().join("home").display().to_string()),
                (ENV_XDG_DATA_HOME, data_home.display().to_string()),
                (ENV_XDG_STATE_HOME, state_home.display().to_string()),
                (ENV_XDG_CACHE_HOME, cache_home.display().to_string()),
            ])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        assert_eq!(paths.source(), StatePathSource::Xdg);
        assert_eq!(
            paths.data_dir(),
            data_home
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
        assert_eq!(
            paths.state_dir(),
            state_home
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
        assert_eq!(
            paths.cache_dir(),
            cache_home
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
    }

    #[test]
    fn linux_fallback_uses_home_xdg_defaults() {
        let config = default_config();
        let home = PathBuf::from("/home/example");

        let paths = StatePathResolver::new()
            .with_platform(StatePlatform::Linux)
            .with_env([(ENV_HOME, home.display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        assert_eq!(paths.source(), StatePathSource::Xdg);
        assert_eq!(
            paths.data_dir(),
            home.join(".local")
                .join("share")
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
        assert_eq!(
            paths.state_dir(),
            home.join(".local")
                .join("state")
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
        assert_eq!(
            paths.cache_dir(),
            home.join(".cache")
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
    }

    #[test]
    fn macos_fallback_uses_library_directories() {
        let config = default_config();
        let home = PathBuf::from("/Users/example");

        let paths = StatePathResolver::new()
            .with_platform(StatePlatform::MacOs)
            .with_env([(ENV_HOME, home.display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        let app_support = home
            .join("Library")
            .join("Application Support")
            .join(PROJECT_DIR);
        assert_eq!(paths.source(), StatePathSource::PlatformFallback);
        assert_eq!(
            paths.data_dir(),
            app_support.join(DATA_DIR).join(PROFILE_DIR).join("default")
        );
        assert_eq!(
            paths.state_dir(),
            app_support
                .join(STATE_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
        assert_eq!(
            paths.cache_dir(),
            home.join("Library")
                .join("Caches")
                .join(PROJECT_DIR)
                .join(PROFILE_DIR)
                .join("default")
        );
    }

    #[test]
    fn selected_profile_changes_state_directory() {
        let mut config = MissiveConfig::default();
        config
            .profiles
            .insert("ci".to_owned(), ProfileConfig::default());
        config.default_profile = "ci".to_owned();
        config.validate().expect("config validates");
        let loaded = LoadedConfig {
            config,
            source: missive_core::ConfigSource {
                kind: missive_core::ConfigSourceKind::BuiltInDefault,
                path: None,
            },
            selected_profile: "ci".to_owned(),
        };
        let temp = tempdir().expect("tempdir");

        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, temp.path().display().to_string())])
            .resolve_loaded(&loaded)
            .expect("paths resolve");

        assert_eq!(paths.profile(), "ci");
        assert!(
            paths
                .state_dir()
                .ends_with(Path::new("profiles").join("ci"))
        );
        assert_eq!(
            paths.database_path(),
            paths.state_dir().join(DEFAULT_DATABASE_FILE)
        );
    }

    #[test]
    fn explicit_database_path_can_be_absolute_home_or_profile_relative() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let state_root = temp.path().join("state-root");
        let storage = StorageConfig {
            backend: StorageBackend::Sqlite,
            database_path: Some(PathBuf::from("relative/state.sqlite3")),
        };

        let relative_paths = StatePathResolver::new()
            .with_env([
                (ENV_HOME, home.display().to_string()),
                (ENV_XDG_STATE_HOME, state_root.display().to_string()),
            ])
            .with_platform(StatePlatform::Linux)
            .resolve_storage("default", &storage)
            .expect("paths resolve");
        assert_eq!(
            relative_paths.database_path(),
            relative_paths.state_dir().join("relative/state.sqlite3")
        );

        let storage = StorageConfig {
            backend: StorageBackend::Sqlite,
            database_path: Some(PathBuf::from("~/custom.sqlite3")),
        };
        let home_paths = StatePathResolver::new()
            .with_env([
                (ENV_HOME, home.display().to_string()),
                (ENV_XDG_STATE_HOME, state_root.display().to_string()),
            ])
            .with_platform(StatePlatform::Linux)
            .resolve_storage("default", &storage)
            .expect("paths resolve");
        assert_eq!(home_paths.database_path(), home.join("custom.sqlite3"));

        let absolute = temp.path().join("absolute.sqlite3");
        let storage = StorageConfig {
            backend: StorageBackend::Sqlite,
            database_path: Some(absolute.clone()),
        };
        let absolute_paths = StatePathResolver::new()
            .with_env([
                (ENV_HOME, home.display().to_string()),
                (ENV_XDG_STATE_HOME, state_root.display().to_string()),
            ])
            .with_platform(StatePlatform::Linux)
            .resolve_storage("default", &storage)
            .expect("paths resolve");
        assert_eq!(absolute_paths.database_path(), absolute);
    }

    #[test]
    fn relative_database_path_cannot_escape_profile_state_dir() {
        let temp = tempdir().expect("tempdir");
        let storage = StorageConfig {
            backend: StorageBackend::Sqlite,
            database_path: Some(PathBuf::from("../outside.sqlite3")),
        };

        let error = StatePathResolver::new()
            .with_env([(ENV_HOME, temp.path().display().to_string())])
            .with_platform(StatePlatform::Linux)
            .resolve_storage("default", &storage)
            .expect_err("relative traversal should fail");

        assert_eq!(error.category(), missive_core::ErrorCategory::Config);
        assert!(error.to_string().contains("cannot escape"));
    }

    #[test]
    fn resolve_does_not_create_runtime_directories() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("missive-home");
        let config = default_config();

        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, root.display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        assert!(!paths.data_dir().exists());
        assert!(!paths.state_dir().exists());
        assert!(!paths.cache_dir().exists());
    }

    #[test]
    fn default_resolution_does_not_point_at_source_tree() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let config = default_config();

        let paths = StatePathResolver::new()
            .with_platform(StatePlatform::Linux)
            .with_env([(ENV_HOME, home.display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        assert!(!paths.state_dir().starts_with(source_tree));
        assert!(!paths.database_path().starts_with(source_tree));
    }

    #[test]
    fn ensure_directories_creates_only_resolved_tree() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("missive-home");
        let config = default_config();
        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, root.display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        paths.ensure_directories().expect("create state dirs");

        assert!(paths.data_dir().is_dir());
        assert!(paths.state_dir().is_dir());
        assert!(paths.cache_dir().is_dir());
        assert!(paths.locks_dir().is_dir());
    }

    #[test]
    fn concurrent_lock_acquisition_is_exclusive() {
        let temp = tempdir().expect("tempdir");
        let config = default_config();
        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, temp.path().display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        let first = ProcessLock::try_acquire(&paths, ProcessLockKind::StateMutation)
            .expect("first lock succeeds");
        let second = ProcessLock::try_acquire(&paths, ProcessLockKind::StateMutation)
            .expect_err("second lock should be contended");

        assert_eq!(second.category(), missive_core::ErrorCategory::Storage);
        assert!(second.to_string().contains("already held"));
        drop(first);

        let reacquired = ProcessLock::try_acquire(&paths, ProcessLockKind::StateMutation)
            .expect("lock can be reacquired after drop");
        assert_eq!(reacquired.kind(), ProcessLockKind::StateMutation);
        assert_eq!(
            reacquired.path(),
            paths.lock_path(ProcessLockKind::StateMutation)
        );
    }

    #[test]
    fn gateway_and_state_locks_are_independent() {
        let temp = tempdir().expect("tempdir");
        let config = default_config();
        let paths = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, temp.path().display().to_string())])
            .resolve_config(&config, "default")
            .expect("paths resolve");

        let state = ProcessLock::try_acquire(&paths, ProcessLockKind::StateMutation)
            .expect("state lock succeeds");
        let gateway = ProcessLock::try_acquire(&paths, ProcessLockKind::Gateway)
            .expect("gateway lock succeeds independently");

        assert_eq!(
            state.path(),
            paths.lock_path(ProcessLockKind::StateMutation)
        );
        assert_eq!(gateway.path(), paths.lock_path(ProcessLockKind::Gateway));
    }

    #[test]
    fn relative_environment_roots_are_rejected() {
        let config = default_config();
        let error = StatePathResolver::new()
            .with_env([(ENV_MISSIVE_HOME, "relative/path")])
            .resolve_config(&config, "default")
            .expect_err("relative env roots fail");

        assert_eq!(error.category(), missive_core::ErrorCategory::Config);
        assert!(error.to_string().contains("absolute"));
    }
}
