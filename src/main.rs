mod auth;
mod fs;
mod github;
mod install;
mod mount;
mod shell;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

#[derive(Parser)]
#[command(
    name = "cdrepo",
    about = "Browse GitHub repos from your terminal. Just cd owner/repo.",
    version,
    after_help = "Examples:\n  \
        cdrepo install              Auto-configure everything\n  \
        cd cli/cli                  Browse github.com/cli/cli\n  \
        cd rust-lang/rust           Browse github.com/rust-lang/rust\n  \
        cd https://github.com/x/y  Works with full URLs too"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Auto-configure shell hooks, verify auth, check FUSE — zero manual steps
    Install,

    /// Remove shell hooks, unmount all repos, clean cache
    Uninstall,

    /// Mount a GitHub repo and print the mount path (used by shell hook)
    Mount {
        /// Repository: owner/repo, github.com/owner/repo, or full URL
        repo: String,
    },

    /// Unmount a previously mounted repo
    Unmount {
        /// Mount path or owner/repo
        path: String,
    },

    /// Print shell integration code (used internally by shell hook)
    Init {
        /// Shell name: bash, zsh, or fish
        shell: String,
    },

    /// Show authentication status
    Auth,

    /// List currently mounted repos
    List,

    /// Clear the local cache
    ClearCache,

    /// Internal: run FUSE daemon for a repo (spawned by mount command)
    #[command(hide = true)]
    Daemon {
        /// Repository: owner/repo
        repo: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .with_target(false)
        .without_time()
        .init();

    if let Err(e) = run().await {
        eprintln!("{} {e:#}", "error:".red().bold());
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Install => {
            install::run_install()?;
        }

        Commands::Uninstall => {
            install::run_uninstall()?;
        }

        Commands::Mount { repo } => {
            let spec = github::RepoSpec::parse(&repo)?;
            let mount_path = mount::mount_repo(&spec).await?;
            // Print ONLY the path — shell hook reads this via $()
            print!("{}", mount_path.display());
        }

        Commands::Unmount { path } => {
            mount::unmount_repo(&path)?;
        }

        Commands::Init { shell } => {
            let code = match shell.as_str() {
                "bash" => shell::generate_bash(),
                "zsh" => shell::generate_zsh(),
                "fish" => shell::generate_fish(),
                other => anyhow::bail!("Unsupported shell: {other}. Use bash, zsh, or fish."),
            };
            print!("{code}");
        }

        Commands::Auth => {
            match auth::get_token() {
                Ok(_) => {
                    match auth::whoami() {
                        Ok(user) => println!("Authenticated as {}", user.green().bold()),
                        Err(_) => println!("{}", "Token found (could not verify username)".yellow()),
                    }
                }
                Err(e) => {
                    eprintln!("{}", "Not authenticated".red().bold());
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::List => {
            let mnt_dir = install::mount_base_dir();
            if !mnt_dir.exists() {
                println!("No repos mounted.");
                return Ok(());
            }

            let mut found = false;
            for owner in std::fs::read_dir(&mnt_dir)? {
                let owner = owner?;
                if !owner.file_type()?.is_dir() {
                    continue;
                }
                for repo in std::fs::read_dir(owner.path())? {
                    let repo = repo?;
                    let path = repo.path();
                    let owner_name = owner.file_name();
                    let repo_name = repo.file_name();
                    println!(
                        "  {}/{} -> {}",
                        owner_name.to_string_lossy().cyan(),
                        repo_name.to_string_lossy().cyan().bold(),
                        path.display().to_string().dimmed()
                    );
                    found = true;
                }
            }
            if !found {
                println!("No repos mounted.");
            }
        }

        Commands::Daemon { repo } => {
            // This runs as a detached background process.
            // Fully synchronous — no tokio needed for FUSE.
            mount::run_daemon(&repo)?;
        }

        Commands::ClearCache => {
            let cache_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("cdrepo");
            if cache_dir.exists() {
                std::fs::remove_dir_all(&cache_dir)?;
                println!("Cache cleared: {}", cache_dir.display());
            } else {
                println!("No cache to clear.");
            }
        }
    }

    Ok(())
}
