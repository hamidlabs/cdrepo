/// Generate shell hook code for automatic cd interception.

pub fn generate_bash() -> String {
    r#"
# cdrepo shell integration (bash)
# Intercepts cd to detect GitHub repo links and mount them via FUSE.

__cdrepo_cd() {
    if [ $# -eq 0 ]; then
        builtin cd
        return
    fi

    local target="$1"
    shift

    if __cdrepo_is_repo "$target"; then
        local mount_path
        mount_path="$(cdrepo mount "$target" 2>/dev/null)"
        local rc=$?
        if [ $rc -eq 0 ] && [ -d "$mount_path" ]; then
            builtin cd "$mount_path" "$@"
        else
            echo "cdrepo: $mount_path" >&2
            return $rc
        fi
    else
        builtin cd "$target" "$@"
    fi
}

__cdrepo_is_repo() {
    local target="$1"

    # Skip if it's an existing local path, or standard cd targets
    if [ -e "$target" ] || [ "$target" = "-" ] || [ "$target" = "~" ] || [[ "$target" == ~* ]] || [[ "$target" == /* ]] || [[ "$target" == .* ]]; then
        return 1
    fi

    # Match GitHub URLs and SSH format
    # https://github.com/owner/repo
    # http://github.com/owner/repo
    # github.com/owner/repo
    # git@github.com:owner/repo.git
    if [[ "$target" =~ ^https?://github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]] ||
       [[ "$target" =~ ^github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]] ||
       [[ "$target" =~ ^git@github\.com:[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]]; then
        return 0
    fi

    return 1
}

# Exit hook: unmount when leaving a cdrepo mount
__cdrepo_exit_hook() {
    local prev="$OLDPWD"
    if [[ "$prev" == "$HOME/.cdrepo/mnt/"* ]] && [[ "$PWD" != "$HOME/.cdrepo/mnt/"* ]]; then
        cdrepo unmount "$prev" 2>/dev/null &
    fi
}

alias cd='__cdrepo_cd'
PROMPT_COMMAND="__cdrepo_exit_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#
    .to_string()
}

pub fn generate_zsh() -> String {
    r#"
# cdrepo shell integration (zsh)
# Intercepts cd to detect GitHub repo links and mount them via FUSE.

__cdrepo_cd() {
    if [ $# -eq 0 ]; then
        builtin cd
        return
    fi

    local target="$1"
    shift

    if __cdrepo_is_repo "$target"; then
        local mount_path
        mount_path="$(cdrepo mount "$target" 2>/dev/null)"
        local rc=$?
        if [ $rc -eq 0 ] && [ -d "$mount_path" ]; then
            builtin cd "$mount_path" "$@"
        else
            echo "cdrepo: $mount_path" >&2
            return $rc
        fi
    else
        builtin cd "$target" "$@"
    fi
}

__cdrepo_is_repo() {
    local target="$1"

    if [ -e "$target" ] || [ "$target" = "-" ] || [ "$target" = "~" ] || [[ "$target" == ~* ]] || [[ "$target" == /* ]] || [[ "$target" == .* ]]; then
        return 1
    fi

    if [[ "$target" =~ ^https?://github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]] ||
       [[ "$target" =~ ^github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]] ||
       [[ "$target" =~ ^git@github\.com:[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+ ]]; then
        return 0
    fi

    return 1
}

__cdrepo_chpwd() {
    if [[ "$OLDPWD" == "$HOME/.cdrepo/mnt/"* ]] && [[ "$PWD" != "$HOME/.cdrepo/mnt/"* ]]; then
        cdrepo unmount "$OLDPWD" 2>/dev/null &
    fi
}

alias cd='__cdrepo_cd'
autoload -U add-zsh-hook
add-zsh-hook chpwd __cdrepo_chpwd
"#
    .to_string()
}

pub fn generate_fish() -> String {
    r#"
# cdrepo shell integration (fish)
# Intercepts cd to detect GitHub repo links and mount them via FUSE.

function __cdrepo_is_repo
    set -l target $argv[1]

    if test -e "$target"; or test "$target" = "-"; or string match -q '~*' "$target"; or string match -q '/*' "$target"; or string match -q '.*' "$target"
        return 1
    end

    if string match -rq '^https?://github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        return 0
    else if string match -rq '^github\.com/[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        return 0
    else if string match -rq '^git@github\.com:[a-zA-Z0-9._-]+/[a-zA-Z0-9._-]+' "$target"
        return 0
    end

    return 1
end

# Override fish's built-in cd function
functions -e cd 2>/dev/null
function cd --description 'Change directory (with cdrepo GitHub support)'
    if test (count $argv) -eq 0
        builtin cd
        return $status
    end

    if __cdrepo_is_repo $argv[1]
        set -l mount_path (command cdrepo mount $argv[1] 2>/dev/null)
        set -l rc $status
        if test $rc -eq 0; and test -d "$mount_path"
            builtin cd "$mount_path"
            return $status
        else
            echo "cdrepo: $mount_path" >&2
            return $rc
        end
    else
        builtin cd $argv
        return $status
    end
end

function __cdrepo_on_pwd_change --on-variable PWD
    if string match -q "$HOME/.cdrepo/mnt/*" "$__cdrepo_prev_pwd"; and not string match -q "$HOME/.cdrepo/mnt/*" "$PWD"
        command cdrepo unmount "$__cdrepo_prev_pwd" 2>/dev/null &
    end
    set -g __cdrepo_prev_pwd "$PWD"
end
"#
    .to_string()
}

/// Detect all shells the user has configured.
/// Returns all shells that should get hooks installed.
pub fn detect_shells() -> Vec<String> {
    let mut shells = Vec::new();

    // Check $SHELL (login shell)
    if let Ok(shell) = std::env::var("SHELL") {
        let shell_name = shell.rsplit('/').next().unwrap_or(&shell);
        match shell_name {
            "bash" | "zsh" | "fish" => {
                shells.push(shell_name.to_string());
            }
            _ => {}
        }
    }

    // Check if fish config exists (fish often runs inside bash)
    if let Some(config_dir) = dirs::config_dir() {
        if config_dir.join("fish/config.fish").exists() && !shells.contains(&"fish".to_string()) {
            shells.push("fish".to_string());
        }
    }

    // Check if zsh config exists
    if let Some(home) = dirs::home_dir() {
        if home.join(".zshrc").exists() && !shells.contains(&"zsh".to_string()) {
            shells.push("zsh".to_string());
        }
    }

    // Fallback: check parent process
    if shells.is_empty() {
        #[cfg(target_os = "linux")]
        {
            if let Ok(ppid_comm) = std::fs::read_to_string(format!("/proc/{}/comm", unsafe {
                libc::getppid()
            })) {
                let name = ppid_comm.trim();
                match name {
                    "bash" | "zsh" | "fish" => shells.push(name.to_string()),
                    _ => {}
                }
            }
        }
    }

    shells
}

/// Detect the current user's shell (for backwards compat).
pub fn detect_shell() -> Option<String> {
    detect_shells().into_iter().next()
}

/// Get the shell RC file path for a given shell.
pub fn shell_rc_path(shell: &str) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    match shell {
        "bash" => {
            let bashrc = home.join(".bashrc");
            if bashrc.exists() {
                Some(bashrc)
            } else {
                Some(home.join(".bash_profile"))
            }
        }
        "zsh" => Some(home.join(".zshrc")),
        "fish" => Some(
            dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("fish")
                .join("conf.d")
                .join("cdrepo.fish"),
        ),
        _ => None,
    }
}

pub const SHELL_MARKER_BEGIN: &str = "# >>> cdrepo >>>";
pub const SHELL_MARKER_END: &str = "# <<< cdrepo <<<";

pub fn shell_block(shell: &str) -> String {
    match shell {
        "bash" | "zsh" | "fish" => {}
        _ => return String::new(),
    };
    format!("{SHELL_MARKER_BEGIN}\neval \"$(cdrepo init {shell})\"\n{SHELL_MARKER_END}\n")
}
