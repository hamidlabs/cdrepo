# cdrepo

**Browse any GitHub repo from your terminal. No cloning. Just `cd`.**

```bash
cd https://github.com/torvalds/linux
ls
cat Makefile
```

---

## The Story

While working on a project, I constantly needed to peek at code from other repos — check an API signature, read a config file, understand how a library structures things. Every single time, the workflow was the same: open browser, navigate to GitHub, find the file, read it there. Or worse — clone the entire repo just to read one file, then delete it later.

I never wanted to leave my terminal. What if I could just `cd` into a repo link and browse it like a local directory?

So I built **cdrepo**.

---

## Demo

```
~/projects $ cd https://github.com/antonmedv/fx
fx $ ls
Dockerfile  go.mod  internal/  LICENSE  main.go  README.md  ...

fx $ cat main.go
package main

import (
    ...

fx $ cd internal/
internal $ ls
complete/  engine/  fuzzy/  jsonpath/  pretty/  theme/  ...

fx/internal $ cd ..
~/projects $                    ← back where you started
```

**That's it.** No clone, no cleanup, no disk space. Files are fetched on-demand from GitHub's API and cached by SHA.

---

## Features

- **Just `cd` a link** — HTTPS, SSH, `github.com/owner/repo`, or just `owner/repo`
- **Zero config** — one install command sets up everything
- **Private repos** — uses your existing `gh` auth, no extra tokens
- **Fast** — entire repo tree fetched in a single API call, files loaded lazily
- **Cached** — content-addressable cache (SHA-keyed), same content never downloaded twice
- **Normal shell** — `ls`, `cat`, `grep`, `vim`, `tree` all work. It's a real filesystem
- **Smart `cd ..`** — navigates within the repo, exits back to your original directory at the root
- **Auto-recovery** — stale mounts are detected and remounted transparently
- **Multi-shell** — bash, zsh, and fish supported out of the box

## Install

```bash
git clone https://github.com/hamidlabs/cdrepo.git
cd cdrepo
./install.sh
```

That's it. Restart your shell and start browsing.

**Requirements:**
- Rust toolchain (for building)
- FUSE (`fuse3` on Linux, `macFUSE` on macOS)
- GitHub CLI (`gh`) — for authentication

### What the installer does

1. Builds the `cdrepo` binary (release mode)
2. Installs it to `~/.cdrepo/bin/`
3. Adds `~/.cdrepo/bin` to PATH for all detected shells
4. Injects `cd` hook into bash/zsh/fish
5. Verifies FUSE is available
6. Verifies GitHub authentication

No manual configuration. No editing dotfiles.

## Usage

```bash
# HTTPS URL (just paste from browser)
cd https://github.com/rust-lang/rust

# Short form
cd github.com/cli/cli

# Just owner/repo
cd cli/cli

# SSH format
cd git@github.com:user/private-repo.git

# Browse files normally
ls
cat src/main.rs
tree src/ | head -20

# Navigate
cd src/lib/
cd ..

# Exit — cd .. at repo root returns you to where you were
cd ..

# Manage mounts
cdrepo list              # show mounted repos
cdrepo unmount cli/cli   # manually unmount
cdrepo clear-cache       # clear downloaded file cache
cdrepo auth              # check auth status
```

## Supported Formats

| Format | Example |
|--------|---------|
| HTTPS URL | `cd https://github.com/owner/repo` |
| Short URL | `cd github.com/owner/repo` |
| Owner/Repo | `cd cli/cli` |
| SSH | `cd git@github.com:owner/repo.git` |
| With branch | `cd https://github.com/owner/repo/tree/develop` |
| With path | `cd https://github.com/owner/repo/tree/main/src` |

## How It Works

```
 cd https://github.com/user/repo
              │
              ▼
    ┌─────────────────┐
    │   Shell Hook     │  fish/bash/zsh intercepts cd,
    │   (cd override)  │  detects GitHub URL pattern
    └────────┬────────┘
             │
             ▼
    ┌─────────────────┐
    │  cdrepo mount    │  spawns background daemon
    └────────┬────────┘
             │
             ▼
    ┌─────────────────┐     ┌──────────────────┐
    │  cdrepo daemon   │────▶│  GitHub REST API  │
    │  (background)    │     │  (authenticated)  │
    └────────┬────────┘     └──────────────────┘
             │                        │
             │  1. GET /repos/{owner}/{repo}/git/trees/{sha}?recursive=1
             │     → entire repo tree in ONE call
             │
             │  2. GET /repos/{owner}/{repo}/git/blobs/{sha}
             │     → file content on demand (when you cat/read a file)
             │
             ▼
    ┌─────────────────┐
    │   FUSE Mount     │  read-only filesystem at
    │   (~/.cdrepo/    │  ~/.cdrepo/mnt/owner/repo/
    │    mnt/owner/    │
    │    repo/)        │
    └────────┬────────┘
             │
             ▼
        Normal shell
     ls, cat, grep, vim
       all just work
```

### Key Design Decisions

**FUSE over custom TUI** — By mounting a real filesystem, every tool in your shell works: `grep -r`, `vim`, `tree`, `wc -l`. No special commands to learn.

**Blocking HTTP in FUSE daemon** — The daemon uses `reqwest::blocking` instead of async. FUSE callbacks are synchronous, and mixing async runtimes with `fork()` causes deadlocks. Keeping the daemon fully synchronous is simpler and more reliable.

**Spawn, don't fork** — The `mount` command spawns `cdrepo daemon` as a separate process. Earlier versions used `fork()`, which broke tokio's runtime threads in the child process, causing the terminal to hang on any file read.

**SHA-keyed cache** — Git objects are immutable. A blob SHA always maps to the same content. So we cache aggressively: tree structures in `~/.cache/cdrepo/trees/`, file content in `~/.cache/cdrepo/blobs/`. Same file across repos? Cached once.

**Fish function file** — Fish's `cd` is an autoloaded function, not a builtin. You can't override it with `eval`. Instead, cdrepo writes `~/.config/fish/functions/cd.fish` which takes priority over the built-in.

## Architecture

```
src/
├── main.rs      CLI entry point (clap) — routes to subcommands
├── auth.rs      GitHub token resolution (GH_TOKEN → gh auth token → hosts.yml)
├── github.rs    Blocking GitHub API client, tree/blob fetching, SHA-based disk cache
├── fs.rs        FUSE filesystem — readdir, read, lookup, getattr, access
├── mount.rs     Mount (spawn daemon) / unmount / stale mount recovery
├── shell.rs     Shell hook generation for bash/zsh/fish
└── install.rs   Zero-touch installer — FUSE check, auth, shell hook injection
```

## Uninstall

```bash
cdrepo uninstall
```

Removes shell hooks, unmounts all repos, and cleans the cache.

## License

MIT
