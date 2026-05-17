use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AtomicWritePlan {
    pub target: PathBuf,
    pub backup: PathBuf,
}

pub fn plan_atomic_write(target: &Path, snapshot_dir: &Path) -> AtomicWritePlan {
    let backup_name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.bak".to_string());

    AtomicWritePlan {
        target: target.to_path_buf(),
        backup: snapshot_dir.join(backup_name),
    }
}

pub fn write_with_backup(target: &Path, content: &[u8], snapshot_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("creating snapshot dir {}", snapshot_dir.display()))?;

    if target.exists() {
        let plan = plan_atomic_write(target, snapshot_dir);
        std::fs::copy(target, &plan.backup).with_context(|| {
            format!(
                "backing up {} to {}",
                target.display(),
                plan.backup.display()
            )
        })?;
    }

    let parent = target
        .parent()
        .with_context(|| format!("target has no parent: {}", target.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(content)?;
    temp.flush()?;
    temp.persist(target)
        .map_err(|error| error.error)
        .with_context(|| format!("persisting atomic write to {}", target.display()))?;
    Ok(())
}
