#!/bin/zsh
set -euo pipefail

SCRIPT_PATH="${(%):-%x}"
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
DEFAULT_MESSAGE="Deploy mac DMG"

show_failure() {
  local exit_code=$?

  if (( exit_code == 0 )); then
    return
  fi

  set +e
  echo ""
  echo "Deploy failed (exit code $exit_code)."
  echo "Review the output above for the cause."
  if [[ -t 0 ]]; then
    read -r "?Press Return to close this window."
  fi
}

trap show_failure EXIT

cd "$SCRIPT_DIR"

# Finder starts .command files with a minimal PATH. Add the usual Homebrew and
# Rust locations, then let the project's .nvmrc select the intended Node version.
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"

if [[ -s "$HOME/.nvm/nvm.sh" ]]; then
  set +u
  if ! source "$HOME/.nvm/nvm.sh"; then
    set -u
    echo "Unable to initialize NVM from $HOME/.nvm/nvm.sh." >&2
    exit 1
  fi
  if ! nvm use --silent; then
    set -u
    echo "Unable to select the Node.js version declared in $SCRIPT_DIR/.nvmrc." >&2
    exit 1
  fi
  set -u
fi

for required_command in node npm cargo rustc git hdiutil; do
  if ! command -v "$required_command" >/dev/null 2>&1; then
    echo "Missing required command: $required_command" >&2
    echo "Install the project prerequisites described in README.md, then try again." >&2
    exit 1
  fi
done

node scripts/check-node.mjs

if command -v osascript >/dev/null 2>&1; then
  MESSAGE="$(
    osascript <<'APPLESCRIPT'
try
  set dialogResult to display dialog "Commit message for this deploy:" default answer "Deploy mac DMG" buttons {"Cancel", "Deploy"} default button "Deploy"
  return text returned of dialogResult
on error number -128
  return ""
end try
APPLESCRIPT
  )"
else
  MESSAGE="$DEFAULT_MESSAGE"
fi

if [[ -z "${MESSAGE//[[:space:]]/}" ]]; then
  echo "Deploy canceled: no commit message provided."
  exit 0
fi

echo "Running macOS deploy from $SCRIPT_DIR"
echo "Commit message: $MESSAGE"
echo ""

npm run deploy:mac -- -m "$MESSAGE"

echo ""
echo "Deploy finished. You can close this window."
