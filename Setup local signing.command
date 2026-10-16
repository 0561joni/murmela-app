#!/bin/zsh
# Double-click in Finder to run the one-time signing setup.
cd "$(dirname "$0")"
zsh scripts/setup-local-signing.sh
echo ""
read -r "?Press Return to close this window."
