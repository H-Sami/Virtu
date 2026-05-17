use anyhow::Result;
use std::path::PathBuf;

pub fn list_snapshots() -> Result<()> {
    let snapshot_dir = virtu_home().join("snapshots");
    if !snapshot_dir.exists() {
        println!("No Virtu snapshots found.");
        return Ok(());
    }

    println!("Available Virtu snapshots:");
    for entry in std::fs::read_dir(snapshot_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            println!("  {}", entry.file_name().to_string_lossy());
        }
    }
    Ok(())
}

pub async fn rollback_to(snapshot_id: &str) -> Result<()> {
    let snapshot_path = virtu_home().join("snapshots").join(snapshot_id);
    if !snapshot_path.exists() {
        anyhow::bail!("Snapshot `{snapshot_id}` does not exist");
    }

    println!("Rollback is scaffolded but not implemented yet: {snapshot_id}");
    Ok(())
}

fn virtu_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".virtu")
}
