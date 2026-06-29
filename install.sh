#!/usr/bin/env bash
# install.sh — Build and install squad binaries from source
set -euo pipefail

BOLD="\033[1m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
RESET="\033[0m"

info()    { echo -e "${BOLD}${GREEN}✓${RESET} $*"; }
warning() { echo -e "${BOLD}${YELLOW}⚠${RESET} $*"; }
error()   { echo -e "${BOLD}${RED}✗${RESET} $*" >&2; exit 1; }

echo -e "${BOLD}squad installer${RESET}"
echo ""

# 1. Require cargo
if ! command -v cargo &>/dev/null; then
    error "cargo not found. Install Rust from https://rustup.rs then re-run this script."
fi

CARGO_VERSION=$(cargo --version)
info "Found $CARGO_VERSION"

# 2. Build & install
echo ""
echo "Installing squad …"
cargo install --path "$(dirname "$0")" --locked

# 3. Install slash commands for detected AI tools
echo ""
echo "Installing /squad slash command for detected AI tools…"
squad setup || warning "squad setup failed (non-fatal)"

# 4. PATH check (renumbered)
CARGO_BIN="$HOME/.cargo/bin"
case ":${PATH}:" in
    *":${CARGO_BIN}:"*)
        info "$CARGO_BIN is already in PATH"
        ;;
    *)
        warning "$CARGO_BIN is not in your PATH."
        echo ""
        echo "  Add the following line to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
        echo ""
        echo '    export PATH="$HOME/.cargo/bin:$PATH"'
        echo ""
        echo "  Then restart your shell or run:  source ~/.zshrc"
        ;;
esac

# 5. Success message
echo ""
info "Installation complete!"
echo ""
echo -e "${BOLD}Quick Start:${RESET}"
echo ""
echo "  1. Go to your project directory:"
echo "       cd my-project"
echo ""
echo "  2. Initialize a squad workspace:"
echo "       squad init"
echo ""
echo "  3. In any AI CLI terminal, use the slash command:"
echo "       /squad manager"
echo ""
echo "  Run 'squad help' for all commands."
