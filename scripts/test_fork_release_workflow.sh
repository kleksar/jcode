#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fork_workflow="$repo_root/.github/workflows/fork-release.yml"
official_workflow="$repo_root/.github/workflows/release.yml"
installer="$repo_root/scripts/install.sh"
readme="$repo_root/README.md"

require_literal() {
  local file="$1"
  local text="$2"
  if ! grep -Fq -- "$text" "$file"; then
    printf 'missing required release contract in %s: %s\n' "$file" "$text" >&2
    exit 1
  fi
}

require_literal "$fork_workflow" "github.repository == 'kleksar/jcode'"
require_literal "$fork_workflow" 'scripts/build_linux_compat.sh dist'
# These are literal workflow and shell source contracts, not expressions to expand.
# shellcheck disable=SC2016
require_literal "$fork_workflow" 'gh release create "${GITHUB_REF_NAME}"'
require_literal "$fork_workflow" 'SHA256SUMS'

for asset in \
  jcode-linux-x86_64 \
  jcode-linux-aarch64 \
  jcode-macos-aarch64 \
  jcode-macos-x86_64; do
  require_literal "$fork_workflow" "$asset"
done

require_literal "$official_workflow" "if: github.repository == '1jehuang/jcode'"
# shellcheck disable=SC2016
require_literal "$installer" 'REPO="${JCODE_REPO:-kleksar/jcode}"'
# shellcheck disable=SC2016
require_literal "$installer" 'RELEASE_METADATA_BASE="${JCODE_RELEASE_METADATA_BASE:-}"'
require_literal "$readme" 'https://raw.githubusercontent.com/kleksar/jcode/custom/ui-stable/scripts/install.sh'

printf 'fork release workflow contract tests passed\n'
