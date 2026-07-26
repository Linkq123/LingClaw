use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, MAIN_DB, OpenFlags, OptionalExtension};

use super::{Database, StorageError, schema};

fn open_read_only(path: &Path) -> Result<Connection, StorageError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn scalar_i64(connection: &Connection, sql: &str) -> Result<i64, StorageError> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

fn table_count(connection: &Connection, table: &str) -> Result<u64, StorageError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Ok(0);
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    Ok(u64::try_from(scalar_i64(connection, &sql)?).unwrap_or_default())
}

fn database_size(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|path| fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f = bytes as f64;
    if bytes_f >= GIB {
        format!("{:.2} GiB", bytes_f / GIB)
    } else if bytes_f >= MIB {
        format!("{:.2} MiB", bytes_f / MIB)
    } else if bytes_f >= KIB {
        format!("{:.2} KiB", bytes_f / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn verify_database(connection: &Connection) -> Result<(i64, i64, String), StorageError> {
    let application_id = scalar_i64(connection, "PRAGMA application_id")?;
    if application_id != schema::APPLICATION_ID {
        return Err(StorageError::new(format!(
            "Unexpected SQLite application_id {application_id}; this is not a LingClaw database"
        )));
    }
    let schema_version = scalar_i64(connection, "PRAGMA user_version")?;
    let integrity: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StorageError::new(format!(
            "SQLite integrity check failed: {integrity}"
        )));
    }
    let foreign_key_violation = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .optional()?;
    if let Some((table, row_id, parent)) = foreign_key_violation {
        let row_id = row_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Err(StorageError::new(format!(
            "SQLite foreign key check failed in table '{table}' row {row_id} referencing '{parent}'"
        )));
    }
    Ok((application_id, schema_version, integrity))
}

fn print_status(path: &Path) -> Result<(), StorageError> {
    if !path.exists() {
        return Err(StorageError::new(format!(
            "LingClaw database does not exist: {}",
            path.display()
        )));
    }
    let connection = open_read_only(path)?;
    let (application_id, schema_version, integrity) = verify_database(&connection)?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let migration_state = connection
        .query_row(
            "SELECT value FROM storage_metadata WHERE key='legacy_json_migration'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|_| "complete")
        .unwrap_or("not recorded");
    let migrations = table_count(&connection, "schema_migrations")?;

    println!("LingClaw SQLite storage");
    println!("  Path:             {}", path.display());
    println!("  Application ID:   {application_id:#x}");
    println!(
        "  Schema:           {schema_version} (current {})",
        schema::SCHEMA_VERSION
    );
    println!("  Journal mode:     {}", journal_mode.to_ascii_uppercase());
    println!("  Integrity:        {integrity}");
    println!("  Schema migrations:{migrations:>5}");
    println!("  JSON migration:   {migration_state}");
    println!("  Size (DB+WAL):    {}", format_bytes(database_size(path)));
    println!("  Entities:");
    for (label, table) in [
        ("sessions", "sessions"),
        ("messages", "session_messages"),
        ("todos", "session_todos"),
        ("sub-agent snapshots", "session_subagent_snapshots"),
        ("usage days", "session_usage_days"),
        ("groups", "groups"),
        ("group messages", "group_messages"),
        ("group runs", "group_runs"),
        ("group votes", "group_votes"),
    ] {
        println!("    {label:<20} {}", table_count(&connection, table)?);
    }
    Ok(())
}

pub(super) fn create_backup(source: &Path, destination: &Path) -> Result<(), StorageError> {
    if !source.exists() {
        return Err(StorageError::new(format!(
            "LingClaw database does not exist: {}",
            source.display()
        )));
    }
    if destination.exists() {
        return Err(StorageError::new(format!(
            "Backup destination already exists: {}",
            destination.display()
        )));
    }
    if source == destination {
        return Err(StorageError::new(
            "Backup destination must differ from the live database",
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            StorageError::new(format!(
                "Failed to create backup directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let source_connection = open_read_only(source)?;
    verify_database(&source_connection)?;
    let mut destination_options = fs::OpenOptions::new();
    destination_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        destination_options.mode(0o600);
    }
    let destination_file = destination_options.open(destination).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            StorageError::new(format!(
                "Backup destination already exists: {}",
                destination.display()
            ))
        } else {
            StorageError::new(format!(
                "Failed to reserve backup destination {}: {error}",
                destination.display()
            ))
        }
    })?;
    drop(destination_file);
    if let Err(error) = source_connection.backup(MAIN_DB, destination, None) {
        let _ = fs::remove_file(destination);
        return Err(StorageError::new(format!(
            "SQLite online backup failed: {error}"
        )));
    }
    let verification = open_read_only(destination).and_then(|connection| {
        verify_database(&connection)?;
        Ok(())
    });
    if let Err(error) = verification {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(destination, permissions)?;
    }
    Ok(())
}

fn default_backup_path(database_path: &Path) -> Result<PathBuf, StorageError> {
    let home = database_path
        .parent()
        .ok_or_else(|| StorageError::new("SQLite database has no parent directory"))?;
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    Ok(home
        .join("backups")
        .join(format!("lingclaw-{timestamp}.db")))
}

fn db_command(args: &[String]) -> Result<(), StorageError> {
    let database_path = Database::default_path()?;
    match args.first().map(String::as_str) {
        Some("status") if args.len() == 1 => print_status(&database_path),
        Some("backup") if args.len() <= 2 => {
            let destination = match args.get(1) {
                Some(path) => PathBuf::from(path),
                None => default_backup_path(&database_path)?,
            };
            create_backup(&database_path, &destination)?;
            println!("SQLite backup created: {}", destination.display());
            Ok(())
        }
        _ => Err(StorageError::new(
            "Usage: lingclaw db status | lingclaw db backup [PATH]",
        )),
    }
}

pub(crate) fn handle_db_cli(args: &[String]) {
    if let Err(error) = db_command(args) {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
}
