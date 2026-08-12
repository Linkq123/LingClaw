mod admin;
mod group;
mod legacy;
mod schema;
mod session;

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

#[cfg(unix)]
use std::ffi::OsString;

#[cfg(not(test))]
use std::{
    future::Future,
    task::{Context, Poll, Wake, Waker},
};

use rusqlite::OptionalExtension;
use tokio::sync::{Mutex, watch};
use tokio_rusqlite::Connection;

pub(crate) use admin::handle_db_cli;
pub(crate) use legacy::{migrate_legacy_json_if_needed, preflight_legacy_storage_path_conflicts};
pub(crate) use session::{SessionDeleteOutcome, SessionModelPreferences, SessionUsageSnapshot};

pub(crate) const GROUP_MISSING_SESSIONS_ERROR_PREFIX: &str = "Group references missing sessions: ";

static GLOBAL_DATABASE: OnceLock<Database> = OnceLock::new();

#[cfg(unix)]
fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = OsString::from(path.as_os_str());
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(unix)]
fn prepare_private_database_files(path: &Path) -> Result<(), StorageError> {
    use std::{
        fs::OpenOptions,
        io::ErrorKind,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => drop(file),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(StorageError::new(error.to_string())),
    }

    for (file, required) in [
        (path.to_path_buf(), true),
        (sqlite_sidecar_path(path, "-wal"), false),
        (sqlite_sidecar_path(path, "-shm"), false),
        (sqlite_sidecar_path(path, "-journal"), false),
    ] {
        let metadata = match std::fs::metadata(&file) {
            Ok(metadata) => metadata,
            Err(error) if !required && error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(StorageError::new(error.to_string())),
        };
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&file, permissions)
            .map_err(|error| StorageError::new(error.to_string()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_private_database_files(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(not(test))]
struct ThreadWaker(std::thread::Thread);

#[cfg(not(test))]
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(not(test))]
fn park_on_connection<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(not(test))]
fn block_on_connection<F: Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(|| park_on_connection(future))
    } else {
        park_on_connection(future)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StorageMode {
    Healthy,
    Protected,
}

#[derive(Clone, Debug)]
pub(crate) struct StorageStatus {
    pub(crate) mode: StorageMode,
    pub(crate) reason: Option<String>,
}

impl Default for StorageStatus {
    fn default() -> Self {
        Self {
            mode: StorageMode::Healthy,
            reason: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StorageError {
    message: String,
}

impl StorageError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

impl From<tokio_rusqlite::Error> for StorageError {
    fn from(error: tokio_rusqlite::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub(crate) struct RuntimeInstanceLock {
    file: std::fs::File,
}

impl RuntimeInstanceLock {
    pub(crate) fn acquire(database_path: &Path) -> Result<Self, StorageError> {
        use std::io::Write as _;

        let path = database_path.with_file_name("lingclaw.runtime.lock");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        lock_runtime_file(&file).map_err(|error| {
            StorageError::new(format!(
                "another LingClaw process is already using {}: {error}",
                database_path.display()
            ))
        })?;
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { file })
    }
}

impl Drop for RuntimeInstanceLock {
    fn drop(&mut self) {
        unlock_runtime_file(&self.file);
    }
}

#[cfg(unix)]
fn lock_runtime_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;

    // SAFETY: flock only borrows the valid file descriptor for this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn unlock_runtime_file(file: &std::fs::File) {
    use std::os::fd::AsRawFd as _;

    // SAFETY: flock only borrows the valid file descriptor for this call.
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(windows)]
fn lock_runtime_file(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;

    // SAFETY: LockFile only borrows the valid file handle for this call.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::LockFile(file.as_raw_handle() as _, 0, 0, 1, 0)
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn unlock_runtime_file(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle as _;

    // SAFETY: UnlockFile only borrows the valid file handle for this call.
    let _ = unsafe {
        windows_sys::Win32::Storage::FileSystem::UnlockFile(file.as_raw_handle() as _, 0, 0, 1, 0)
    };
}

fn normalized_schema_sql(sql: &str) -> String {
    // SQLite preserves the textual shape used by ALTER TABLE. In particular,
    // an appended column can leave the closing parenthesis attached to the
    // final default literal, while a freshly-created table keeps a newline
    // before it. Treat that punctuation-only whitespace as equivalent while
    // continuing to compare every schema object and SQL token.
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(" )", ")")
}

fn schema_signature(
    connection: &rusqlite::Connection,
) -> Result<BTreeMap<(String, String), String>, StorageError> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite_%' \
           AND sql IS NOT NULL \
         ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut signature = BTreeMap::new();
    for row in rows {
        let (object_type, name, sql) = row?;
        signature.insert((object_type, name), normalized_schema_sql(&sql));
    }
    Ok(signature)
}

fn schema_difference(
    expected: &BTreeMap<(String, String), String>,
    actual: &BTreeMap<(String, String), String>,
) -> String {
    let missing = expected
        .keys()
        .filter(|key| !actual.contains_key(*key))
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let unexpected = actual
        .keys()
        .filter(|key| !expected.contains_key(*key))
        .map(|(_, name)| name.as_str())
        .collect::<Vec<_>>();
    let changed = expected
        .iter()
        .filter_map(|(key, sql)| {
            actual
                .get(key)
                .filter(|actual_sql| *actual_sql != sql)
                .map(|_| key.1.as_str())
        })
        .collect::<Vec<_>>();
    let mut details = Vec::new();
    if !missing.is_empty() {
        details.push(format!("missing: {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        details.push(format!("unexpected: {}", unexpected.join(", ")));
    }
    if !changed.is_empty() {
        details.push(format!("definition changed: {}", changed.join(", ")));
    }
    details.join("; ")
}

fn validate_current_schema(connection: &rusqlite::Connection) -> Result<(), StorageError> {
    let application_id =
        connection.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?;
    let user_version =
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if application_id != schema::APPLICATION_ID || user_version != schema::SCHEMA_VERSION {
        return Err(StorageError::new(format!(
            "LingClaw SQLite header changed during startup (application_id={application_id:#x}, user_version={user_version})"
        )));
    }

    let reference = rusqlite::Connection::open_in_memory()?;
    reference.execute_batch(schema::INITIAL_SCHEMA)?;
    let expected = schema_signature(&reference)?;
    let actual = schema_signature(connection)?;
    if actual != expected {
        return Err(StorageError::new(format!(
            "LingClaw SQLite schema {} does not match the registered definition ({})",
            schema::SCHEMA_VERSION,
            schema_difference(&expected, &actual)
        )));
    }

    let mut statement =
        connection.prepare("SELECT version, name FROM schema_migrations ORDER BY version")?;
    let migrations = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let expected_migrations = vec![
        (1, "initial_core_storage".to_string()),
        (2, "plan_lifecycle".to_string()),
        (3, "durable_plan_feedback".to_string()),
        (4, "plan_initial_submission_marker".to_string()),
        (5, "plan_stale_override_audit".to_string()),
        (6, "session_working_directories".to_string()),
    ];
    if migrations != expected_migrations {
        return Err(StorageError::new(format!(
            "LingClaw SQLite migration ledger does not match schema {}",
            schema::SCHEMA_VERSION
        )));
    }
    Ok(())
}

fn validate_database_integrity(connection: &rusqlite::Connection) -> Result<(), StorageError> {
    let check: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(StorageError::new(format!(
            "SQLite quick check failed: {check}"
        )));
    }
    let foreign_key_violation = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .optional()?;
    if let Some((table, row_id, parent)) = foreign_key_violation {
        return Err(StorageError::new(format!(
            "SQLite foreign key check failed in table '{table}' row {row_id} referencing '{parent}'"
        )));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Database {
    connection: Connection,
    path: Arc<PathBuf>,
    operation_gate: Arc<Mutex<()>>,
    status: Arc<RwLock<StorageStatus>>,
    status_tx: watch::Sender<StorageStatus>,
}

impl Database {
    pub(crate) async fn open(path: PathBuf) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| StorageError::new(error.to_string()))?;
        }
        prepare_private_database_files(&path)?;
        let connection = Connection::open(&path).await?;
        let (application_id, user_version, user_object_count) = connection
            .call(|connection| {
                let application_id = connection
                    .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?;
                let user_version =
                    connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
                let user_object_count = connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((application_id, user_version, user_object_count))
            })
            .await
            .map_err(StorageError::from)?;
        if application_id != 0 && application_id != schema::APPLICATION_ID {
            return Err(StorageError::new(format!(
                "SQLite application_id {application_id:#x} does not belong to LingClaw"
            )));
        }
        if user_version > schema::SCHEMA_VERSION {
            return Err(StorageError::new(format!(
                "LingClaw database schema {user_version} is newer than this binary supports ({})",
                schema::SCHEMA_VERSION
            )));
        }
        if user_version > 0 && application_id == 0 {
            return Err(StorageError::new(
                "SQLite database has a schema version but no LingClaw application_id",
            ));
        }
        if application_id == 0 && user_version == 0 && user_object_count > 0 {
            return Err(StorageError::new(
                "SQLite database has no LingClaw application_id and is not empty",
            ));
        }
        if user_version == 0 && user_object_count > 0 {
            return Err(StorageError::new(
                "LingClaw SQLite database contains an unversioned schema",
            ));
        }
        if user_version > 0 && user_version < schema::SCHEMA_VERSION {
            let source = path.clone();
            let destination = next_schema_backup_path(&path, user_version)?;
            tokio::task::spawn_blocking(move || admin::create_backup(&source, &destination))
                .await
                .map_err(|error| {
                    StorageError::new(format!("Failed to create schema backup: {error}"))
                })??;
            connection
                .call(move |connection| migrate_schema(connection, user_version))
                .await
                .map_err(|error| StorageError::new(error.to_string()))?;
        }
        let create_schema = user_version == 0;
        let initial_status = StorageStatus::default();
        let (status_tx, _) = watch::channel(initial_status.clone());
        let database = Self {
            connection,
            path: Arc::new(path),
            operation_gate: Arc::new(Mutex::new(())),
            status: Arc::new(RwLock::new(initial_status)),
            status_tx,
        };
        database.initialize(create_schema).await?;
        database.apply_private_permissions()?;
        Ok(database)
    }

    pub(crate) fn default_path() -> Result<PathBuf, StorageError> {
        crate::config_dir_path()
            .map(|path| path.join("lingclaw.db"))
            .ok_or_else(|| StorageError::new("Unable to resolve the LingClaw home directory"))
    }

    pub(crate) fn path(&self) -> &Path {
        self.path.as_ref().as_path()
    }

    pub(crate) fn install_global(&self) -> Result<(), StorageError> {
        if let Some(existing) = GLOBAL_DATABASE.get() {
            if existing.path() == self.path() {
                return Ok(());
            }
            return Err(StorageError::new(
                "LingClaw storage was already initialized with a different database",
            ));
        }
        GLOBAL_DATABASE
            .set(self.clone())
            .map_err(|_| StorageError::new("Failed to install LingClaw storage"))
    }

    #[cfg(not(test))]
    pub(crate) fn global() -> Result<&'static Self, StorageError> {
        GLOBAL_DATABASE
            .get()
            .ok_or_else(|| StorageError::new("LingClaw storage is not initialized"))
    }

    pub(crate) fn status(&self) -> StorageStatus {
        self.status
            .read()
            .map(|status| status.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    #[cfg(not(test))]
    pub(crate) fn is_writable(&self) -> bool {
        self.status().mode == StorageMode::Healthy
    }

    pub(crate) fn protect(&self, reason: impl Into<String>) {
        protect_shared_status(&self.status, &self.status_tx, reason.into());
    }

    #[cfg(not(test))]
    pub(crate) fn subscribe_status(&self) -> watch::Receiver<StorageStatus> {
        self.status_tx.subscribe()
    }

    pub(crate) fn ensure_writable(&self) -> Result<(), StorageError> {
        ensure_shared_status_writable(&self.status)
    }

    async fn initialize(&self, create_schema: bool) -> Result<(), StorageError> {
        self.connection
            .call(move |connection| -> Result<(), StorageError> {
                connection.busy_timeout(std::time::Duration::from_secs(5))?;
                connection.execute_batch(
                    "PRAGMA foreign_keys = ON;\nPRAGMA journal_mode = WAL;\nPRAGMA synchronous = NORMAL;",
                )?;
                if create_schema {
                    let transaction = connection
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    transaction.pragma_update(None, "application_id", schema::APPLICATION_ID)?;
                    transaction.execute_batch(schema::INITIAL_SCHEMA)?;
                    let applied_at = crate::now_epoch() as i64;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (1, 'initial_core_storage', ?1)",
                        [applied_at],
                    )?;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (2, 'plan_lifecycle', ?1)",
                        [applied_at],
                    )?;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (3, 'durable_plan_feedback', ?1)",
                        [applied_at],
                    )?;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (4, 'plan_initial_submission_marker', ?1)",
                        [applied_at],
                    )?;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (5, 'plan_stale_override_audit', ?1)",
                        [applied_at],
                    )?;
                    transaction.execute(
                        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (6, 'session_working_directories', ?1)",
                        [applied_at],
                    )?;
                    transaction.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;
                    transaction.commit()?;
                } else {
                    // Managed Session homes are derived from the current
                    // LingClaw home rather than being portable data. A copied
                    // or restored database may therefore contain the old
                    // machine's absolute path and index key even though the
                    // Session itself is healthy. Reconcile them atomically
                    // before accepting the database so workspace lookups keep
                    // using the indexed key on the new machine.
                    let transaction = connection.transaction_with_behavior(
                        rusqlite::TransactionBehavior::Immediate,
                    )?;
                    reconcile_managed_working_directories(&transaction)?;
                    validate_current_schema(&transaction)?;
                    validate_database_integrity(&transaction)?;
                    transaction.commit()?;
                    return Ok(());
                }
                validate_database_integrity(connection)?;
                Ok(())
            })
            .await
            .map_err(|error| StorageError::new(error.to_string()))
    }

    fn apply_private_permissions(&self) -> Result<(), StorageError> {
        prepare_private_database_files(self.path())
    }

    pub(crate) async fn checkpoint(&self) -> Result<(), StorageError> {
        self.call(|connection| {
            connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn metadata(&self, key: &str) -> Result<Option<String>, StorageError> {
        let key = key.to_string();
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT value FROM storage_metadata WHERE key=?1",
                    [&key],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    pub(crate) async fn set_metadata(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let key = key.to_string();
        let value = value.to_string();
        self.call(move |connection| {
            connection.execute(
                "INSERT INTO storage_metadata(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn entity_counts(&self) -> Result<(u64, u64), StorageError> {
        self.read(|connection| {
            let sessions = connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })?;
            let groups = connection.query_row("SELECT COUNT(*) FROM groups", [], |row| {
                row.get::<_, i64>(0)
            })?;
            Ok((
                u64::try_from(sessions).unwrap_or_default(),
                u64::try_from(groups).unwrap_or_default(),
            ))
        })
        .await
    }

    /// Process-bound plan states cannot survive a restart because their Agent
    /// run reservations live only in memory. Convert them to an explicit
    /// stopped state before Sessions are loaded so the UI always offers a
    /// valid recovery path.
    pub(crate) async fn recover_interrupted_plans(&self) -> Result<(usize, usize), StorageError> {
        let recovered_at = crate::now_epoch() as i64;
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let planning = transaction.execute(
                r#"UPDATE session_plans
                   SET status='stopped', updated_at=MAX(updated_at, ?1),
                       finished_at=?1, approved_at=NULL
                   WHERE status='planning'"#,
                [recovered_at],
            )?;
            let executing = transaction.execute(
                r#"UPDATE session_plans
                   SET status='stopped', updated_at=MAX(updated_at, ?1), finished_at=?1
                   WHERE status='executing'"#,
                [recovered_at],
            )?;
            transaction.commit()?;
            Ok((planning, executing))
        })
        .await
    }

    async fn run<F, R>(&self, require_writable: bool, operation: F) -> Result<R, StorageError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R, StorageError> + Send + 'static,
        R: Send + 'static,
    {
        let _operation_guard = self.operation_gate.lock().await;
        if require_writable {
            self.ensure_writable()?;
        }
        let status = self.status.clone();
        let status_tx = self.status_tx.clone();
        match self
            .connection
            .call(move |connection| {
                if require_writable {
                    ensure_shared_status_writable(&status)?;
                }
                match operation(connection) {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        let protected_reason =
                            tokio_rusqlite::Error::<StorageError>::Error(error.clone()).to_string();
                        protect_shared_status(&status, &status_tx, protected_reason);
                        Err(error)
                    }
                }
            })
            .await
        {
            Ok(value) => Ok(value),
            Err(error) => {
                let storage_error = StorageError::new(error.to_string());
                self.protect(storage_error.to_string());
                Err(storage_error)
            }
        }
    }

    pub(super) async fn call<F, R>(&self, operation: F) -> Result<R, StorageError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R, StorageError> + Send + 'static,
        R: Send + 'static,
    {
        self.run(true, operation).await
    }

    pub(super) async fn read<F, R>(&self, operation: F) -> Result<R, StorageError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R, StorageError> + Send + 'static,
        R: Send + 'static,
    {
        self.run(false, operation).await
    }

    #[cfg(not(test))]
    pub(super) fn blocking_read<F, R>(&self, operation: F) -> Result<R, StorageError>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<R, StorageError> + Send + 'static,
        R: Send + 'static,
    {
        block_on_connection(self.read(operation))
    }
}

fn reconcile_managed_working_directories(
    connection: &rusqlite::Connection,
) -> Result<(), StorageError> {
    let session_ids = {
        let mut statement = connection
            .prepare("SELECT id FROM sessions WHERE workspace_kind='managed' ORDER BY id")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for session_id in session_ids {
        crate::session_store::validate_session_id(&session_id).map_err(StorageError::new)?;
        let path = crate::session_workspace_path(&session_id);
        let path_text = path.to_str().ok_or_else(|| {
            StorageError::new(format!(
                "Managed workspace for Session '{session_id}' is not valid UTF-8"
            ))
        })?;
        let key = crate::working_directory_key(&path).map_err(StorageError::new)?;
        connection.execute(
            r#"UPDATE sessions
               SET working_directory=?1, working_directory_key=?2
               WHERE id=?3 AND workspace_kind='managed'
                 AND (working_directory<>?1 OR working_directory_key<>?2)"#,
            rusqlite::params![path_text, key, session_id],
        )?;
    }
    Ok(())
}

fn migrate_schema(
    connection: &mut rusqlite::Connection,
    from_version: i64,
) -> Result<(), StorageError> {
    if !matches!(from_version, 1..=5) {
        return Err(StorageError::new(format!(
            "No SQLite schema migration is registered from version {from_version} to {}",
            schema::SCHEMA_VERSION
        )));
    }

    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if from_version == 1 {
        let legacy_plans = {
            let mut statement = transaction.prepare(
                r#"SELECT p.session_id, p.plan_id, p.original_user_message_index,
                          p.assistant_plan_message_index, p.created_at, m.content
                   FROM session_pending_plans p
                   LEFT JOIN session_messages m
                     ON m.session_id = p.session_id
                    AND m.position = p.assistant_plan_message_index
                   ORDER BY p.session_id"#,
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };

        transaction.execute_batch(schema::PLAN_LIFECYCLE_SCHEMA)?;
        for (session_id, plan_id, user_index, assistant_index, created_at, content) in legacy_plans
        {
            let markdown = content.ok_or_else(|| {
                StorageError::new(format!(
                    "Legacy plan '{plan_id}' references a missing assistant message"
                ))
            })?;
            let artifact = crate::plan::legacy_artifact(&markdown);
            crate::plan::validate_persisted_legacy_artifact(&artifact).map_err(|error| {
                StorageError::new(format!("Invalid legacy plan '{plan_id}': {error}"))
            })?;
            let artifact_json = serde_json::to_string(&artifact)
                .map_err(|error| StorageError::new(error.to_string()))?;
            transaction.execute(
                r#"INSERT INTO session_plans(
                    session_id, plan_id, original_user_message_index, current_revision,
                    status, created_at, updated_at, approved_at, finished_at,
                    execution_attempt, evidence_truncated, stale_override_json
                ) VALUES (?1, ?2, ?3, 1, 'ready', ?4, ?4, NULL, NULL, 0, 0, NULL)"#,
                rusqlite::params![session_id, plan_id, user_index, created_at],
            )?;
            transaction.execute(
                r#"INSERT INTO session_plan_revisions(
                    session_id, plan_id, revision, assistant_plan_message_index,
                    artifact_json, evidence_json, created_at
                ) VALUES (?1, ?2, 1, ?3, ?4, '[]', ?5)"#,
                rusqlite::params![
                    session_id,
                    plan_id,
                    assistant_index,
                    artifact_json,
                    created_at
                ],
            )?;
            transaction.execute(
                r#"INSERT INTO session_plan_progress(
                    session_id, plan_id, position, step_id, title, status, note, deviation_reason
                ) VALUES (?1, ?2, 0, 'legacy-plan', 'Execute the approved plan', 'pending', '', NULL)"#,
                rusqlite::params![session_id, plan_id],
            )?;
        }
        transaction.execute("DROP TABLE session_pending_plans", [])?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (2, 'plan_lifecycle', ?1)",
            [crate::now_epoch() as i64],
        )?;
    }

    if from_version <= 2 {
        transaction.execute_batch(schema::PLAN_FEEDBACK_SCHEMA)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (3, 'durable_plan_feedback', ?1)",
            [crate::now_epoch() as i64],
        )?;
    }
    if from_version <= 3 {
        transaction.execute_batch(schema::PLAN_INITIAL_SUBMISSION_SCHEMA)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (4, 'plan_initial_submission_marker', ?1)",
            [crate::now_epoch() as i64],
        )?;
    }
    if from_version <= 4 {
        transaction.execute_batch(schema::PLAN_STALE_OVERRIDE_AUDIT_SCHEMA)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (5, 'plan_stale_override_audit', ?1)",
            [crate::now_epoch() as i64],
        )?;
    }
    transaction.execute_batch(schema::SESSION_WORKSPACE_SCHEMA)?;
    let session_ids = {
        let mut statement = transaction.prepare("SELECT id FROM sessions ORDER BY id")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for session_id in session_ids {
        let path = crate::session_workspace_path(&session_id);
        let path_text = path.to_str().ok_or_else(|| {
            StorageError::new(format!(
                "Managed workspace for Session '{session_id}' is not valid UTF-8"
            ))
        })?;
        let key = crate::working_directory_key(&path).map_err(StorageError::new)?;
        transaction.execute(
            "UPDATE sessions SET workspace_kind='managed', working_directory=?1, working_directory_key=?2 WHERE id=?3",
            rusqlite::params![path_text, key, session_id],
        )?;
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version, name, applied_at) VALUES (6, 'session_working_directories', ?1)",
        [crate::now_epoch() as i64],
    )?;
    transaction.pragma_update(None, "user_version", schema::SCHEMA_VERSION)?;
    // Validate the exact schema, migration ledger, and integrity while the
    // migration is still rollback-capable. A post-commit validation failure
    // must never leave the user's primary database permanently half-upgraded.
    validate_current_schema(&transaction)?;
    validate_database_integrity(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn protect_shared_status(
    status: &RwLock<StorageStatus>,
    status_tx: &watch::Sender<StorageStatus>,
    reason: String,
) {
    let mut status = status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if status.mode == StorageMode::Healthy {
        status.mode = StorageMode::Protected;
        status.reason = Some(reason);
        status_tx.send_replace(status.clone());
    }
}

fn ensure_shared_status_writable(status: &RwLock<StorageStatus>) -> Result<(), StorageError> {
    let status = status
        .read()
        .map(|status| status.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
    if status.mode == StorageMode::Protected {
        return Err(StorageError::new(status.reason.unwrap_or_else(|| {
            "LingClaw storage is in protected mode".to_string()
        })));
    }
    Ok(())
}

fn next_schema_backup_path(path: &Path, version: i64) -> Result<PathBuf, StorageError> {
    let home = path
        .parent()
        .ok_or_else(|| StorageError::new("SQLite database has no parent directory"))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let directory = home.join("backups");
    for suffix in 0..1000_u32 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let candidate =
            directory.join(format!("lingclaw-schema-v{version}-{timestamp}{suffix}.db"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(StorageError::new(
        "Unable to allocate a unique schema backup path",
    ))
}

#[cfg(test)]
#[path = "../tests/storage_tests.rs"]
mod storage_tests;
