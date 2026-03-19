use super::*;

fn unique_temp_workspace(prefix: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

#[tokio::test]
async fn append_daily_memory_entry_creates_new_file_with_header() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-new");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");
    let memory_path = memory_dir.join("2026-03-19.md");

    append_daily_memory_entry(&memory_path, "2026-03-19", "09:30", "first summary")
        .await
        .expect("memory entry should be written");

    let content = tokio::fs::read_to_string(&memory_path)
        .await
        .expect("memory file should be readable");
    assert_eq!(content, "# 2026-03-19\n\n\n---\n\n## 09:30 Local\n\nfirst summary");

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn append_daily_memory_entry_appends_without_overwriting_existing_content() {
    let workspace = unique_temp_workspace("lingclaw-command-memory-append");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir)
        .await
        .expect("memory dir should be created");
    let memory_path = memory_dir.join("2026-03-19.md");

    tokio::fs::write(&memory_path, "# 2026-03-19\n\n\n---\n\n## 08:00 Local\n\nexisting summary")
        .await
        .expect("seed memory file should be written");

    append_daily_memory_entry(&memory_path, "2026-03-19", "09:30", "next summary")
        .await
        .expect("memory entry should append");

    let content = tokio::fs::read_to_string(&memory_path)
        .await
        .expect("memory file should be readable");
    assert!(content.contains("## 08:00 Local\n\nexisting summary"));
    assert!(content.contains("## 09:30 Local\n\nnext summary"));
    assert_eq!(content.matches("# 2026-03-19").count(), 1);

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}