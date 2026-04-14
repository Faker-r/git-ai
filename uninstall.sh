#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

# ============================================================
# Ensure HOME is set when running via MDMs (e.g. JAMF) or
# other environments where HOME may be unbound.
# ============================================================
UNINSTALL_USER=""

if [ -z "${HOME:-}" ]; then
    if command -v scutil >/dev/null 2>&1; then
        CURRENT_USER=$( /usr/sbin/scutil <<< "show State:/Users/ConsoleUser" | awk '/Name :/ { print $3 }' || true )
        if [ -n "${CURRENT_USER:-}" ] && [ "$CURRENT_USER" != "loginwindow" ] && [ "$CURRENT_USER" != "_mbsetupuser" ]; then
            export HOME=$( /usr/bin/dscl . -read "/Users/$CURRENT_USER" NFSHomeDirectory | awk '{print $2}' )
            UNINSTALL_USER="$CURRENT_USER"
        else
            echo "Error: No console user logged in. Deferring uninstallation." >&2
            exit 1
        fi
    elif id -un >/dev/null 2>&1; then
        UNINSTALL_USER="$(id -un)"
        export HOME=$(getent passwd "$UNINSTALL_USER" | cut -d: -f6)
        if [ -z "$HOME" ]; then
            export HOME="/root"
        fi
    else
        export HOME="/root"
    fi
fi

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

error()   { echo -e "${RED}Error: $1${NC}" >&2; exit 1; }
warn()    { echo -e "${YELLOW}Warning: $1${NC}" >&2; }
success() { echo -e "${GREEN}$1${NC}"; }

INSTALL_DIR="$HOME/.git-ai/bin"
GIT_AI_BINARY="${INSTALL_DIR}/git-ai"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEV_BINARY_CANDIDATES=(
    "$HOME/.git-ai-local-dev/gitwrap/bin/git-ai"
    "$SCRIPT_DIR/target/debug/git-ai"
    "$SCRIPT_DIR/target/release/git-ai"
)

resolve_git_ai_binary() {
    if [ -x "$GIT_AI_BINARY" ]; then
        echo "$GIT_AI_BINARY"
        return
    fi

    for candidate in "${DEV_BINARY_CANDIDATES[@]}"; do
        if [ -x "$candidate" ]; then
            echo "$candidate"
            return
        fi
    done

    local path_binary=""
    path_binary=$(command -v git-ai 2>/dev/null || true)
    if [ -n "$path_binary" ] && [ -x "$path_binary" ]; then
        echo "$path_binary"
        return
    fi

    echo ""
}

echo "Uninstalling git-ai..."
echo ""

# ============================================================
# Step 1: Run uninstall-hooks while the binary is still present.
# This removes IDE/agent hooks, skills, and git client prefs.
# Must happen before we delete the binary in step 4.
#
# Strip ~/.git-ai/bin from PATH before invoking the binary so
# that git-ai's internal git detection finds the real system
# git rather than its own shim.
# ============================================================
UNINSTALL_HOOKS_BINARY="$(resolve_git_ai_binary)"
if [ -n "$UNINSTALL_HOOKS_BINARY" ]; then
    echo "Removing IDE/agent hooks..."
    CLEAN_PATH=$(printf '%s' "${PATH:-}" | tr ':' '\n' | awk '!/\.git-ai/' | tr '\n' ':' | sed 's/:$//')
    if PATH="$CLEAN_PATH" "$UNINSTALL_HOOKS_BINARY" uninstall-hooks --dry-run=false 2>&1; then
        success "IDE/agent hooks removed."
    else
        warn "uninstall-hooks reported errors. Continuing with remaining steps."
    fi
else
    echo "git-ai binary not found in install or dev locations — skipping uninstall-hooks."
fi

echo ""

# ============================================================
# Step 2: Strip PATH entries from shell config files.
# The installer appends a two-line block identifiable by the
# comment "# Added by git-ai installer on <date>" followed by
# the export/fish_add_path line containing ".git-ai/bin".
# Local-dev setup may also add "# git-ai local dev" blocks with
# ".git-ai-local-dev/gitwrap/bin" or "target/gitwrap/bin".
# For fish shells, also note that fish_add_path -g modifies
# a universal variable; see the note at the end of this script.
# ============================================================
SHELL_CONFIGS=(
    "$HOME/.bashrc"
    "$HOME/.bash_profile"
    "$HOME/.zshrc"
    "$HOME/.profile"
    "$HOME/.config/fish/config.fish"
)

SHELLS_CLEANED=""
FISH_CLEANED=false

for config_file in "${SHELL_CONFIGS[@]}"; do
    [ -f "$config_file" ] || continue
    if grep -qsE '\.git-ai/bin|git-ai-local-dev/gitwrap/bin|target/gitwrap/bin' "$config_file"; then
        sed -i.bak '/# Added by git-ai installer/d' "$config_file"
        sed -i.bak '/# git-ai local dev/d' "$config_file"
        sed -i.bak '/\.git-ai\/bin/d' "$config_file"
        sed -i.bak '/git-ai-local-dev\/gitwrap\/bin/d' "$config_file"
        sed -i.bak '/target\/gitwrap\/bin/d' "$config_file"
        rm -f "${config_file}.bak"
        SHELLS_CLEANED="${SHELLS_CLEANED}  ✓ ${config_file}\n"
        if [[ "$config_file" == *"fish"* ]]; then
            FISH_CLEANED=true
        fi
    fi
done

if [ -n "$SHELLS_CLEANED" ]; then
    echo "Removed PATH entries from:"
    printf '%b' "$SHELLS_CLEANED"
    echo ""
else
    echo "No shell config PATH entries found — nothing to remove."
    echo ""
fi

# ============================================================
# Step 2a: Remove git-ai paths from current process PATH so
# this shell session reflects uninstall immediately.
# ============================================================
CURRENT_PATH_CLEAN=$(printf '%s' "${PATH:-}" | tr ':' '\n' | awk '!/(^|\/)\.git-ai\/bin$|git-ai-local-dev\/gitwrap\/bin|target\/gitwrap\/bin/' | tr '\n' ':' | sed 's/:$//')
export PATH="$CURRENT_PATH_CLEAN"

# ============================================================
# Step 3: Remove the ~/.local/bin/git-ai symlink.
# ============================================================
LOCAL_BIN="$HOME/.local/bin/git-ai"
if [ -L "$LOCAL_BIN" ] || [ -f "$LOCAL_BIN" ]; then
    rm -f "$LOCAL_BIN"
    success "Removed $LOCAL_BIN"
fi

# ============================================================
# Step 4: Remove ~/.git-ai/ — contains the binary, git and
# git-og shims, config.json, internal state, and libexec symlink.
# ============================================================
GIT_AI_DIR="$HOME/.git-ai"
if [ -d "$GIT_AI_DIR" ]; then
    rm -rf "$GIT_AI_DIR"
    success "Removed $GIT_AI_DIR"
else
    echo "$GIT_AI_DIR not found — nothing to remove."
fi

# Remove local-dev install directory if present.
GIT_AI_LOCAL_DEV_DIR="$HOME/.git-ai-local-dev"
if [ -d "$GIT_AI_LOCAL_DEV_DIR" ]; then
    rm -rf "$GIT_AI_LOCAL_DEV_DIR"
    success "Removed $GIT_AI_LOCAL_DEV_DIR"
else
    echo "$GIT_AI_LOCAL_DEV_DIR not found — nothing to remove."
fi

# ============================================================
# Step 5: Remove stale git.path overrides in editor settings
# when they still point to git-ai wrapper paths.
# ============================================================
EDITOR_SETTINGS_FILES=(
    "$HOME/Library/Application Support/Cursor/User/settings.json"
    "$HOME/Library/Application Support/Code/User/settings.json"
    "$HOME/Library/Application Support/Code - Insiders/User/settings.json"
)

if command -v python3 >/dev/null 2>&1; then
    for settings_file in "${EDITOR_SETTINGS_FILES[@]}"; do
        [ -f "$settings_file" ] || continue
        SETTINGS_UPDATED=$(python3 - "$settings_file" <<'PY'
import json
import sys

path = sys.argv[1]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("error")
    raise SystemExit(0)

git_path = data.get("git.path")
if isinstance(git_path, str) and (".git-ai" in git_path or "git-ai-local-dev" in git_path or "gitwrap/bin/git" in git_path):
    data.pop("git.path", None)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=4)
        f.write("\n")
    print("updated")
else:
    print("nochange")
PY
)
        case "$SETTINGS_UPDATED" in
            updated)
                success "Removed stale git.path from $settings_file"
                ;;
            error)
                warn "Could not parse $settings_file to clean git.path."
                ;;
        esac
    done
else
    warn "python3 not found; skipping editor settings cleanup for git.path."
fi

echo ""
success "git-ai has been uninstalled."
echo ""
echo "Next steps:"
echo "  • Restart your terminal (or run: source ~/.zshrc / source ~/.bashrc)"

if [ "$FISH_CLEANED" = true ]; then
    echo ""
    echo -e "${YELLOW}Note (fish shell): fish_add_path -g also stores the path in a universal"
    echo "variable. To fully remove it, run this once in a fish session:"
    echo "  set -e fish_user_paths (contains -i \"$INSTALL_DIR\" \$fish_user_paths)${NC}"
fi
