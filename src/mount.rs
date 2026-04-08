use crate::fs::RepoFs;
use crate::github::{BlockingGitHubClient, RepoSpec, RepoTree};
use crate::install::mount_base_dir;
use anyhow::{Context, Result};
use fuser::{Config, MountOption};
use std::path::PathBuf;
use tracing::debug;

/// Mount a GitHub repo by spawning `cdrepo daemon` as a detached background process.
/// Returns the mount path immediately.
pub async fn mount_repo(spec: &RepoSpec) -> Result<PathBuf> {
    // Verify auth works before spawning daemon
    let _ = crate::auth::get_token()?;

    let mount_path = mount_base_dir().join(&spec.owner).join(&spec.repo);

    // Check if already mounted and alive
    if is_mounted(&mount_path) {
        if is_mount_alive(&mount_path) {
            debug!("already mounted and alive: {}", mount_path.display());
            return Ok(final_path(&mount_path, spec));
        }
        // Stale mount — clean it up and remount
        debug!("stale mount detected, cleaning up: {}", mount_path.display());
        let _ = std::process::Command::new("fusermount3")
            .args(["-uz", &mount_path.display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::process::Command::new("fusermount")
            .args(["-uz", &mount_path.display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        std::fs::remove_dir(&mount_path).ok();
        // Small delay for kernel to release the mount point
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    std::fs::create_dir_all(&mount_path).context("failed to create mount point")?;

    // Spawn `cdrepo daemon owner/repo` as a fully detached background process.
    // The daemon creates its own blocking HTTP client and tokio-free FUSE loop.
    let exe = std::env::current_exe().context("cannot find cdrepo binary")?;
    let repo_arg = format!("{}/{}", spec.owner, spec.repo);

    let child = std::process::Command::new(&exe)
        .args(["daemon", &repo_arg])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn cdrepo daemon")?;

    debug!("spawned daemon pid={}", child.id());

    // Wait for mount to appear
    wait_for_mount(&mount_path).await?;

    Ok(final_path(&mount_path, spec))
}

/// Run the FUSE daemon (called by `cdrepo daemon`). Blocks until unmounted.
/// Fully synchronous — no tokio runtime needed.
pub fn run_daemon(repo: &str) -> Result<()> {
    let spec = RepoSpec::parse(repo)?;
    let token = crate::auth::get_token()?;
    let client = BlockingGitHubClient::new(&token)?;

    let mount_path = mount_base_dir().join(&spec.owner).join(&spec.repo);
    std::fs::create_dir_all(&mount_path).context("failed to create mount point")?;

    // Fetch repo tree (blocking HTTP)
    let (_tree_sha, entries) = client.fetch_tree(&spec)?;
    let repo_tree = RepoTree::new(entries);
    let repo_fs = RepoFs::new(repo_tree, client, spec);

    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RO,
        MountOption::NoSuid,
        MountOption::NoExec,
        MountOption::FSName("cdrepo".to_string()),
    ];

    // This blocks until the filesystem is unmounted
    fuser::mount2(repo_fs, &mount_path, &config).context("FUSE mount failed")?;

    // Cleanup mount point after unmount
    std::fs::remove_dir(&mount_path).ok();

    Ok(())
}

/// Unmount a repo.
pub fn unmount_repo(path: &str) -> Result<()> {
    let path = PathBuf::from(path);

    let mnt_base = mount_base_dir();
    let mount_point = if path.starts_with(&mnt_base) {
        let rel = path.strip_prefix(&mnt_base)?;
        let parts: Vec<_> = rel.components().take(2).collect();
        if parts.len() == 2 {
            mnt_base
                .join(parts[0].as_os_str())
                .join(parts[1].as_os_str())
        } else {
            path.clone()
        }
    } else {
        path.clone()
    };

    let result = std::process::Command::new("fusermount3")
        .args(["-uz", &mount_point.display().to_string()])
        .status();

    let success = match result {
        Ok(s) => s.success(),
        Err(_) => std::process::Command::new("fusermount")
            .args(["-uz", &mount_point.display().to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
    };

    if success {
        std::fs::remove_dir(&mount_point).ok();
        debug!("unmounted: {}", mount_point.display());
    }

    Ok(())
}

fn final_path(mount_path: &PathBuf, spec: &RepoSpec) -> PathBuf {
    match &spec.subpath {
        Some(sub) => mount_path.join(sub),
        None => mount_path.clone(),
    }
}

fn is_mounted(path: &PathBuf) -> bool {
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        let path_str = path.display().to_string();
        return mounts.lines().any(|line| line.contains(&path_str));
    }
    false
}

/// Check if a FUSE mount is actually responding (not "Transport endpoint is not connected").
fn is_mount_alive(path: &PathBuf) -> bool {
    // Try to stat the directory — if the daemon is dead this returns an error
    std::fs::read_dir(path).is_ok()
}

async fn wait_for_mount(path: &PathBuf) -> Result<()> {
    for _ in 0..80 {
        // 8 seconds max
        if is_mounted(path) {
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("Timed out waiting for FUSE mount at {}", path.display())
}
