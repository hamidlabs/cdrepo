use crate::shell;
use anyhow::{Context, Result};
use colored::Colorize;
use std::path::PathBuf;

const FISH_CD_FUNCTION: &str = r#"# cdrepo: Override cd to support GitHub repo URLs
function cd --description 'Change directory (with cdrepo GitHub support)'
    if test (count $argv) -eq 0
        builtin cd
        return $status
    end

    set -l target $argv[1]

    # If inside a cdrepo mount and going up, return to origin instead of mount parent
    if string match -q "$HOME/.cdrepo/mnt/*" "$PWD"
        if test "$target" = ".." -o "$target" = "../"
            set -l mnt_base "$HOME/.cdrepo/mnt"
            set -l rel (string replace "$mnt_base/" "" "$PWD")
            set -l parts (string split "/" "$rel")
            if test (count $parts) -le 2
                if set -q __cdrepo_origin; and test -d "$__cdrepo_origin"
                    builtin cd "$__cdrepo_origin"
                else
                    builtin cd "$HOME"
                end
                return $status
            end
        end

        if test "$target" = "-"
            if set -q __cdrepo_origin; and test -d "$__cdrepo_origin"
                builtin cd "$__cdrepo_origin"
                return $status
            end
        end
    end

    set -l is_repo 0

    if test -e "$target"; or test "$target" = "-"; or string match -q '~*' "$target"; or string match -q '/*' "$target"; or string match -q '.*' "$target"
        set is_repo 0
    else if string match -rq '^https?://github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        set is_repo 1
    else if string match -rq '^github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        set is_repo 1
    else if string match -rq '^git@github\.com:[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        set is_repo 1
    else if string match -rq '^[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+$' "$target"
        set is_repo 1
    end

    if test $is_repo -eq 1
        set -g __cdrepo_origin "$PWD"
        set -l mount_path (command cdrepo mount "$target" 2>/dev/null)
        set -l rc $status
        if test $rc -eq 0; and test -d "$mount_path"
            builtin cd "$mount_path"
            return $status
        else
            echo "cdrepo: failed to mount $target" >&2
            set -e __cdrepo_origin
            return $rc
        end
    else
        builtin cd $argv
        return $status
    end
end
"#;

/// Fully automated installer. Detects shell, injects hooks, verifies deps.
/// Zero manual steps required.
pub fn run_install() -> Result<()> {
    println!("{}", "cdrepo installer".bold());
    println!();

    // Step 1: Check FUSE availability
    print_step(1, "Checking FUSE availability");
    check_fuse()?;
    print_ok("FUSE is available");

    // Step 2: Check GitHub authentication
    print_step(2, "Checking GitHub authentication");
    match crate::auth::get_token() {
        Ok(_) => {
            match crate::auth::whoami() {
                Ok(user) => print_ok(&format!("Authenticated as {}", user.bold())),
                Err(_) => print_ok("Token found"),
            }
        }
        Err(_) => {
            print_warn("No GitHub token found — attempting gh auth login");
            run_gh_auth()?;
        }
    }

    // Step 3: Detect shells
    print_step(3, "Detecting shells");
    let shells = shell::detect_shells();
    if shells.is_empty() {
        anyhow::bail!("Could not detect shell. Supported: bash, zsh, fish");
    }
    for s in &shells {
        print_ok(&format!("Detected {}", s.bold()));
    }

    // Step 4: Create directories
    print_step(4, "Creating directories");
    let mnt_dir = mount_base_dir();
    std::fs::create_dir_all(&mnt_dir)
        .context("Failed to create mount directory")?;
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cdrepo");
    std::fs::create_dir_all(&cache_dir)
        .context("Failed to create cache directory")?;
    print_ok("~/.cdrepo/mnt/ and cache ready");

    // Step 5: Install shell hooks for ALL detected shells
    print_step(5, "Installing shell hooks");
    for s in &shells {
        install_shell_hook(s)?;
    }

    println!();
    println!("{}", "Installation complete!".green().bold());
    println!();
    println!("Restart your shell, then browse any GitHub repo:");
    println!("  {} {}", "cd".cyan(), "https://github.com/antonmedv/fx".cyan().bold());
    println!("  {} {}", "cd".cyan(), "github.com/rust-lang/rust".cyan().bold());
    println!("  {} {}", "cd".cyan(), "git@github.com:user/repo.git".cyan().bold());
    println!();

    Ok(())
}

fn check_fuse() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Check /dev/fuse
        if !std::path::Path::new("/dev/fuse").exists() {
            // Try to load the module
            let _ = std::process::Command::new("sudo")
                .args(["modprobe", "fuse"])
                .status();

            if !std::path::Path::new("/dev/fuse").exists() {
                anyhow::bail!(
                    "FUSE is not available. Install it with:\n  \
                     Ubuntu/Debian: sudo apt install fuse3\n  \
                     Fedora:        sudo dnf install fuse3\n  \
                     Arch:          sudo pacman -S fuse3"
                );
            }
        }

        // Check fusermount3 or fusermount
        let has_fusermount = std::process::Command::new("which")
            .arg("fusermount3")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || std::process::Command::new("which")
                .arg("fusermount")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

        if !has_fusermount {
            anyhow::bail!(
                "fusermount not found. Install fuse3:\n  \
                 Ubuntu/Debian: sudo apt install fuse3\n  \
                 Fedora:        sudo dnf install fuse3\n  \
                 Arch:          sudo pacman -S fuse3"
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        let has_macfuse = std::path::Path::new("/Library/Filesystems/macfuse.fs").exists()
            || std::path::Path::new("/usr/local/lib/libfuse.dylib").exists();
        if !has_macfuse {
            anyhow::bail!(
                "macFUSE is not installed. Install it with:\n  \
                 brew install macfuse\n\
                 Or download from: https://osxfuse.github.io/"
            );
        }
    }

    Ok(())
}

fn run_gh_auth() -> Result<()> {
    // Check if gh is installed
    let has_gh = std::process::Command::new("which")
        .arg("gh")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_gh {
        anyhow::bail!(
            "GitHub CLI (gh) is not installed.\n  \
             Install: https://cli.github.com/\n  \
             Or set GH_TOKEN environment variable."
        );
    }

    println!("  Running: gh auth login");
    let status = std::process::Command::new("gh")
        .args(["auth", "login"])
        .status()
        .context("Failed to run gh auth login")?;

    if !status.success() {
        anyhow::bail!("GitHub authentication failed");
    }

    print_ok("Authenticated with GitHub");
    Ok(())
}

fn install_shell_hook(shell_name: &str) -> Result<()> {
    let rc_path = shell::shell_rc_path(shell_name)
        .context(format!("Could not determine RC file for {shell_name}"))?;

    // For fish, write cd function override + PATH config
    if shell_name == "fish" {
        // Write cd.fish function file (overrides fish's built-in cd)
        let fish_functions_dir = dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config"))
            .join("fish")
            .join("functions");
        std::fs::create_dir_all(&fish_functions_dir)?;
        let cd_fish = fish_functions_dir.join("cd.fish");
        std::fs::write(&cd_fish, FISH_CD_FUNCTION)?;
        print_ok(&format!("Wrote {}", cd_fish.display()));

        // Write conf.d for PATH setup
        if let Some(parent) = rc_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = format!(
            "{}\nif test -d \"$HOME/.cdrepo/bin\"\n    fish_add_path \"$HOME/.cdrepo/bin\"\nend\n{}\n",
            shell::SHELL_MARKER_BEGIN,
            shell::SHELL_MARKER_END
        );
        std::fs::write(&rc_path, content)?;
        print_ok(&format!("Wrote {}", rc_path.display()));
        return Ok(());
    }

    // For bash/zsh, append to existing RC file
    let existing = std::fs::read_to_string(&rc_path).unwrap_or_default();

    // Check if already installed
    if existing.contains(shell::SHELL_MARKER_BEGIN) {
        // Replace existing block
        let re_start = existing.find(shell::SHELL_MARKER_BEGIN).unwrap();
        let re_end = existing
            .find(shell::SHELL_MARKER_END)
            .map(|i| i + shell::SHELL_MARKER_END.len())
            .unwrap_or(existing.len());

        let mut new_content = String::new();
        new_content.push_str(&existing[..re_start]);
        new_content.push_str(&shell::shell_block(shell_name));
        if re_end < existing.len() {
            new_content.push_str(&existing[re_end..]);
        }
        std::fs::write(&rc_path, new_content)?;
        print_ok(&format!("Updated hook in {}", rc_path.display()));
    } else {
        // Append
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
        content.push_str(&shell::shell_block(shell_name));
        std::fs::write(&rc_path, content)?;
        print_ok(&format!("Added hook to {}", rc_path.display()));
    }

    Ok(())
}

/// Uninstall: remove shell hooks.
pub fn run_uninstall() -> Result<()> {
    println!("{}", "cdrepo uninstaller".bold());
    println!();

    // Remove shell hook from all known RC files
    let home = dirs::home_dir().context("Could not find home directory")?;
    let rc_files = vec![
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".zshrc"),
    ];

    for rc in &rc_files {
        if rc.exists() {
            let content = std::fs::read_to_string(rc)?;
            if content.contains(shell::SHELL_MARKER_BEGIN) {
                let start = content.find(shell::SHELL_MARKER_BEGIN).unwrap();
                let end = content
                    .find(shell::SHELL_MARKER_END)
                    .map(|i| i + shell::SHELL_MARKER_END.len() + 1) // +1 for trailing newline
                    .unwrap_or(content.len());

                let mut new_content = String::new();
                new_content.push_str(&content[..start]);
                if end < content.len() {
                    new_content.push_str(&content[end..]);
                }
                std::fs::write(rc, new_content)?;
                print_ok(&format!("Removed hook from {}", rc.display()));
            }
        }
    }

    // Remove fish config
    if let Some(fish_conf) = dirs::config_dir().map(|d| d.join("fish/conf.d/cdrepo.fish")) {
        if fish_conf.exists() {
            std::fs::remove_file(&fish_conf)?;
            print_ok(&format!("Removed {}", fish_conf.display()));
        }
    }

    // Unmount all active mounts
    let mnt_dir = mount_base_dir();
    if mnt_dir.exists() {
        for entry in std::fs::read_dir(&mnt_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let path = entry.path();
                for repo_entry in std::fs::read_dir(&path).into_iter().flatten() {
                    if let Ok(re) = repo_entry {
                        let mount_path = re.path();
                        let _ = std::process::Command::new("fusermount")
                            .args(["-uz", &mount_path.display().to_string()])
                            .status();
                    }
                }
            }
        }
        std::fs::remove_dir_all(&mnt_dir).ok();
        print_ok("Removed mount directory");
    }

    // Remove cache
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cdrepo");
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir).ok();
        print_ok("Removed cache");
    }

    println!();
    println!("{}", "Uninstalled cdrepo.".green().bold());
    println!("Restart your shell to complete removal.");
    Ok(())
}

pub fn mount_base_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cdrepo")
        .join("mnt")
}

fn print_step(n: u8, msg: &str) {
    println!("  [{}] {}", n.to_string().cyan(), msg);
}

fn print_ok(msg: &str) {
    println!("      {} {}", "OK".green().bold(), msg);
}

fn print_warn(msg: &str) {
    println!("      {} {}", "!!".yellow().bold(), msg);
}
