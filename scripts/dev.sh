#!/bin/bash

set -euo pipefail

# Parse arguments
BUILD_TYPE="debug"
if [[ "$#" -gt 0 && "$1" == "--release" ]]; then
    BUILD_TYPE="release"
fi

# Clean up old dev-symlinks.sh PATH export if present
_detect_shell_profile() {
    if [[ "${SHELL:-}" == */zsh ]]; then
        if [[ -f "$HOME/.zshrc" ]]; then
            echo "$HOME/.zshrc"
        else
            echo "$HOME/.zprofile"
        fi
    elif [[ "${SHELL:-}" == */bash ]]; then
        if [[ "$(uname)" == "Darwin" ]]; then
            if [[ -f "$HOME/.bash_profile" ]]; then
                echo "$HOME/.bash_profile"
            else
                echo "$HOME/.bashrc"
            fi
        else
            if [[ -f "$HOME/.bashrc" ]]; then
                echo "$HOME/.bashrc"
            else
                echo "$HOME/.bash_profile"
            fi
        fi
    else
        echo "$HOME/.profile"
    fi
}

_PROFILE="$(_detect_shell_profile)"
if [[ -f "$_PROFILE" ]] && grep -q '\.git-ai-local-dev/gitwrap/bin' "$_PROFILE"; then
    sed -i.bak '/# git-ai local dev/d' "$_PROFILE"
    sed -i.bak '/\.git-ai-local-dev\/gitwrap\/bin/d' "$_PROFILE"
    rm -f "$_PROFILE.bak"
    echo "Cleaned up old git-ai local dev PATH export from $_PROFILE"
fi

# Build the binary first, so we can bootstrap the environment using *our* binary
# rather than the upstream release. This matters because the upstream install.sh
# runs `install-hooks` from whichever binary it places — if we curl the released
# installer, the released binary's hook-installer logic runs and writes agent
# hooks to disk before we ever overwrite the binary, defeating any local changes
# that disable specific agent installers.
echo "Building $BUILD_TYPE binary..."
if [[ "$BUILD_TYPE" == "release" ]]; then
    cargo build --release
else
    cargo build
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCAL_BIN="$REPO_ROOT/target/$BUILD_TYPE/git-ai"

# Bootstrap environment if ~/.git-ai isn't set up or PATH isn't wired in the profile.
# Use the in-repo install.sh with GIT_AI_LOCAL_BINARY so it seats *our* freshly
# built binary instead of downloading a release, and runs our install-hooks.
if [[ ! -d "$HOME/.git-ai/bin" ]] || [[ ! -f "$HOME/.git-ai/config.json" ]] || \
   { [[ -f "$_PROFILE" ]] && ! grep -q '\.git-ai/bin' "$_PROFILE"; } || \
   { [[ ! -f "$_PROFILE" ]]; }; then
    echo "Bootstrapping git-ai environment with local binary..."
    GIT_AI_LOCAL_BINARY="$LOCAL_BIN" bash "$REPO_ROOT/install.sh"
    echo "Done!"
    exit 0
fi

# Already bootstrapped: just swap the binary and re-run install-hooks.
# Install binary via temp file + atomic mv to avoid macOS code signature cache
# issues: direct cp reuses the inode, causing syspolicyd to fail validating the
# changed binary, leaving the process stuck in launched-suspended state unkillably.
echo "Installing binary to ~/.git-ai/bin/git-ai..."
TMP_BIN="$HOME/.git-ai/bin/git-ai.tmp.$$"
cp "$LOCAL_BIN" "$TMP_BIN"
mv -f "$TMP_BIN" "$HOME/.git-ai/bin/git-ai"
chmod +x "$HOME/.git-ai/bin/git-ai"

# Run install hooks
echo "Running install hooks..."
~/.git-ai/bin/git-ai install

echo "Done!"
