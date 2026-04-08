#!/usr/bin/env bash
# cdrepo installer — build + configure, zero manual steps
# Works both ways:
#   git clone ... && cd cdrepo && ./install.sh   (local build)
#   curl -sSL .../install.sh | bash              (remote install)
set -euo pipefail

BINARY="cdrepo"
INSTALL_DIR="${CDREPO_INSTALL_DIR:-$HOME/.cdrepo/bin}"
REMOTE_REPO="cdrepo/cdrepo"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

info()  { printf "${CYAN}${BOLD}  ▸${NC} %s\n" "$*"; }
ok()    { printf "${GREEN}${BOLD}  ✓${NC} %s\n" "$*"; }
err()   { printf "${RED}${BOLD}  ✗${NC} %s\n" "$*" >&2; exit 1; }
step()  { printf "\n${BOLD}[%s]${NC} %s\n" "$1" "$2"; }

main() {
    printf "\n${BOLD}cdrepo${NC} ${DIM}— browse GitHub repos without cloning${NC}\n"

    # ── Step 1: Build or download the binary ────────────────────────
    step "1/3" "Building cdrepo"

    if [ -f "Cargo.toml" ] && grep -q 'name = "cdrepo"' Cargo.toml 2>/dev/null; then
        # Running from cloned repo — build locally
        info "Detected local repo, building from source"

        if ! command -v cargo &>/dev/null; then
            err "Rust is required. Install it: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        fi

        cargo build --release --quiet 2>&1 | tail -1
        mkdir -p "$INSTALL_DIR"
        cp "target/release/$BINARY" "$INSTALL_DIR/$BINARY"
        chmod +x "$INSTALL_DIR/$BINARY"
        ok "Built and installed to $INSTALL_DIR/$BINARY"
    else
        # Remote install — try download release, fallback to clone+build
        info "Downloading cdrepo"

        local platform
        case "$(uname -s)-$(uname -m)" in
            Linux-x86_64|Linux-amd64)    platform="linux-x86_64" ;;
            Linux-aarch64|Linux-arm64)   platform="linux-aarch64" ;;
            Darwin-x86_64|Darwin-amd64)  platform="macos-x86_64" ;;
            Darwin-aarch64|Darwin-arm64) platform="macos-aarch64" ;;
            *) err "Unsupported platform: $(uname -s)-$(uname -m)" ;;
        esac

        local version=""
        if command -v gh &>/dev/null; then
            version="$(gh release view --repo "$REMOTE_REPO" --json tagName -q .tagName 2>/dev/null || true)"
        fi
        if [ -z "$version" ] && command -v curl &>/dev/null; then
            version="$(curl -sSL "https://api.github.com/repos/${REMOTE_REPO}/releases/latest" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*: "//;s/".*//' || true)"
        fi

        if [ -n "$version" ]; then
            local tmpdir; tmpdir="$(mktemp -d)"; trap 'rm -rf "$tmpdir"' EXIT
            local url="https://github.com/${REMOTE_REPO}/releases/download/${version}/cdrepo-${platform}.tar.gz"
            curl -sSL "$url" -o "$tmpdir/cdrepo.tar.gz" 2>/dev/null && \
                tar -xzf "$tmpdir/cdrepo.tar.gz" -C "$tmpdir" && \
                mkdir -p "$INSTALL_DIR" && \
                cp "$tmpdir/$BINARY" "$INSTALL_DIR/$BINARY" && \
                chmod +x "$INSTALL_DIR/$BINARY" && \
                ok "Downloaded $version to $INSTALL_DIR/$BINARY" || {
                    info "Download failed, building from source"
                    version=""
                }
        fi

        if [ -z "$version" ]; then
            # Fallback: clone and build
            if ! command -v cargo &>/dev/null; then
                err "Rust is required. Install it: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            fi
            local tmpdir; tmpdir="$(mktemp -d)"; trap 'rm -rf "$tmpdir"' EXIT
            info "Cloning and building from source"
            git clone --depth 1 "https://github.com/${REMOTE_REPO}.git" "$tmpdir/cdrepo" 2>/dev/null
            cd "$tmpdir/cdrepo"
            cargo build --release --quiet
            mkdir -p "$INSTALL_DIR"
            cp "target/release/$BINARY" "$INSTALL_DIR/$BINARY"
            chmod +x "$INSTALL_DIR/$BINARY"
            ok "Built and installed to $INSTALL_DIR/$BINARY"
        fi
    fi

    # ── Step 2: Add to PATH for ALL shells ──────────────────────────
    step "2/3" "Configuring PATH"

    export PATH="$INSTALL_DIR:$PATH"

    # Bash
    for rc in "$HOME/.bashrc" "$HOME/.bash_profile"; do
        if [ -f "$rc" ]; then
            if ! grep -q '.cdrepo/bin' "$rc" 2>/dev/null; then
                printf '\nexport PATH="$HOME/.cdrepo/bin:$PATH"\n' >> "$rc"
                ok "Added PATH to $rc"
            else
                ok "PATH already in $rc"
            fi
            break
        fi
    done

    # Zsh
    if [ -f "$HOME/.zshrc" ]; then
        if ! grep -q '.cdrepo/bin' "$HOME/.zshrc" 2>/dev/null; then
            printf '\nexport PATH="$HOME/.cdrepo/bin:$PATH"\n' >> "$HOME/.zshrc"
            ok "Added PATH to ~/.zshrc"
        else
            ok "PATH already in ~/.zshrc"
        fi
    fi

    # Fish
    if [ -d "$HOME/.config/fish" ] || command -v fish &>/dev/null; then
        mkdir -p "$HOME/.config/fish/conf.d"
        if ! grep -q '.cdrepo/bin' "$HOME/.config/fish/conf.d/cdrepo.fish" 2>/dev/null; then
            # Write/overwrite the conf.d file with PATH
            cat > "$HOME/.config/fish/conf.d/cdrepo.fish" << 'FISHCONF'
# >>> cdrepo >>>
if test -d "$HOME/.cdrepo/bin"
    fish_add_path "$HOME/.cdrepo/bin"
end
# <<< cdrepo <<<
FISHCONF
            ok "Added PATH to fish conf.d"
        else
            ok "PATH already in fish conf.d"
        fi
    fi

    # ── Step 3: Run cdrepo install (auth, FUSE, shell hooks) ────────
    step "3/3" "Configuring cdrepo"

    "$INSTALL_DIR/$BINARY" install

    printf "\n${GREEN}${BOLD}Done!${NC} Restart your shell and try:\n"
    printf "  ${CYAN}cd https://github.com/antonmedv/fx${NC}\n"
    printf "  ${CYAN}ls${NC}\n"
    printf "  ${CYAN}cat README.md${NC}\n\n"
}

main "$@"
