#!/usr/bin/env bash
#
# update-formula.sh — Downloads release archives, computes SHA256 checksums,
# and rewrites Formula/squad.rb with the new version and hashes.
#
# Required environment variable:
#   VERSION  — the git tag, e.g. "v0.3.1"
#
set -euo pipefail

: "${VERSION:?ERROR: VERSION environment variable is required (e.g. v0.3.1)}"

# Strip leading 'v' for the formula version field
CLEAN_VERSION="${VERSION#v}"

REPO="fractalsociety/coordinate"
BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

FORMULA_FILE="Formula/squad.rb"

# ---------------------------------------------------------------------------
# Download each release asset and compute its SHA256
# ---------------------------------------------------------------------------
declare -A TARGETS=(
  [aarch64-apple-darwin]="squad-aarch64-apple-darwin.tar.gz"
  [x86_64-apple-darwin]="squad-x86_64-apple-darwin.tar.gz"
  [x86_64-unknown-linux-musl]="squad-x86_64-unknown-linux-musl.tar.gz"
)

declare -A SHAS

for target in "${!TARGETS[@]}"; do
  asset="${TARGETS[$target]}"
  url="${BASE_URL}/${asset}"

  echo "Downloading ${url} ..."
  sha=$(curl -fsSL "$url" | sha256sum | awk '{print $1}')
  SHAS[$target]="$sha"
  echo "  SHA256: ${sha}"
done

SHA_ARM="${SHAS[aarch64-apple-darwin]}"
SHA_INTEL="${SHAS[x86_64-apple-darwin]}"
SHA_LINUX="${SHAS[x86_64-unknown-linux-musl]}"

# ---------------------------------------------------------------------------
# Rewrite the formula file
# ---------------------------------------------------------------------------
cat > "$FORMULA_FILE" <<FORMULA
class Squad < Formula
  desc "Multi-AI-agent terminal collaboration daemon with MCP server and workflow engine"
  homepage "https://github.com/fractalsociety/coordinate"
  version "${CLEAN_VERSION}"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-aarch64-apple-darwin.tar.gz"
      sha256 "${SHA_ARM}"
    end

    on_intel do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-x86_64-apple-darwin.tar.gz"
      sha256 "${SHA_INTEL}"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/fractalsociety/coordinate/releases/download/v#{version}/squad-x86_64-unknown-linux-musl.tar.gz"
      sha256 "${SHA_LINUX}"
    end
  end

  def install
    bin.install "squad"
  end

  def caveats
    <<~EOS
      Slash commands are auto-installed when you run \`squad init\`.

      To remove slash commands before uninstalling:
        squad cleanup
    EOS
  end

  test do
    assert_match "Usage:", shell_output("#{bin}/squad help")
  end
end
FORMULA

echo ""
echo "Formula updated to version ${CLEAN_VERSION}"
echo "  aarch64-apple-darwin:       ${SHA_ARM}"
echo "  x86_64-apple-darwin:        ${SHA_INTEL}"
echo "  x86_64-unknown-linux-musl:  ${SHA_LINUX}"
