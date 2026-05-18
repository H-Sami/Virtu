//! Plan executor (slice 5.7 partial).
//!
//! This file is the bridge between the read-only planner and the
//! mutating writers that arrive in Milestone 6. For now it only knows how
//! to execute the very first step of a [`Plan`], which is always
//! [`StepKind::Snapshot`]. That step calls [`snapshot::capture`] and
//! returns the resulting [`SnapshotManifest`] so subsequent writers (when
//! they land) can mutate through [`config::atomic_write::snapshot_then_write`].
//!
//! No other plan steps are executed yet. Calling [`execute_plan`] on a plan
//! with more than the snapshot step will print pending titles for the
//! remaining steps and return without mutating anything.

use anyhow::{anyhow, Result};
use std::path::Path;

use crate::detect::SystemProfile;
use crate::engine::planner::Plan;
use crate::engine::step::{PlannedStep, StepKind};
use crate::snapshot::{capture, FileSystem, SnapshotError, SnapshotManifest};

/// Execute the snapshot step at the front of a [`Plan`].
///
/// Returns the captured snapshot id paired with a freshly read manifest. The
/// manifest is the value Phase-6 writers will thread through their
/// `snapshot_then_write` calls.
pub fn execute_snapshot_step(
    plan: &Plan,
    host: &SystemProfile,
    filesystem: &impl FileSystem,
    snapshots_root: &Path,
) -> Result<(String, SnapshotManifest), SnapshotError> {
    if !plan
        .steps
        .first()
        .map(|step| step.kind == StepKind::Snapshot)
        .unwrap_or(false)
    {
        return Err(SnapshotError::Io {
            path: snapshots_root.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "execute_snapshot_step: plan does not start with StepKind::Snapshot",
            ),
        });
    }

    let id = capture(plan, host, filesystem, snapshots_root)?;
    let snapshot_dir = snapshots_root.join(&id);
    let manifest_path = snapshot_dir.join(crate::snapshot::MANIFEST_FILENAME);
    let manifest_bytes = filesystem
        .read(&manifest_path)
        .map_err(|source| SnapshotError::Io {
            path: manifest_path.clone(),
            source,
        })?;
    let manifest_str = String::from_utf8(manifest_bytes).map_err(|err| SnapshotError::Io {
        path: manifest_path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
    })?;
    let manifest: SnapshotManifest =
        toml::from_str(&manifest_str).map_err(|source| SnapshotError::ManifestParse {
            id: id.clone(),
            source,
        })?;
    Ok((id, manifest))
}

/// Print the plan's pending steps. Currently a no-op for non-snapshot steps.
/// Milestone 6 replaces this with real writers.
pub async fn execute_plan(plan: &[PlannedStep]) -> Result<()> {
    let snapshot_count = plan
        .iter()
        .filter(|step| step.kind == StepKind::Snapshot)
        .count();
    if snapshot_count == 0 {
        return Err(anyhow!(
            "Plan has no snapshot step; refusing to execute any other step."
        ));
    }
    for step in plan {
        println!("pending: {}", step.title);
    }
    Ok(())
}
