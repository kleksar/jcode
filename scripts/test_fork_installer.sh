#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fake_bin="$tmp/bin"
mkdir -p "$fake_bin"
printf 'not a real jcode archive\n' > "$tmp/payload"

cat > "$fake_bin/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) printf '%s\n' "$TEST_OS" ;;
  -m) printf '%s\n' "$TEST_ARCH" ;;
  *) exit 2 ;;
esac
EOF

cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
output=""
while (($#)); do
  case "$1" in
    -o) output="$2"; shift 2 ;;
    http://*|https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
printf '%s\n' "$url" >> "$TEST_URL_LOG"
case "$url" in
  https://github.com/kleksar/jcode/releases/latest)
    printf '%s' 'https://github.com/kleksar/jcode/releases/tag/v0.84.1'
    ;;
  https://github.com/kleksar/jcode/releases/download/v0.84.1/*.tar.gz)
    cp "$TEST_PAYLOAD" "$output"
    ;;
  https://github.com/kleksar/jcode/releases/download/v0.84.1/SHA256SUMS)
    asset="$(tail -n 2 "$TEST_URL_LOG" | head -n 1)"
    asset="${asset##*/}"
    printf '%064d  %s\n' 0 "$asset"
    ;;
  *) exit 22 ;;
esac
EOF
chmod +x "$fake_bin/uname" "$fake_bin/curl"

run_case() {
  local os="$1"
  local arch="$2"
  local artifact="$3"
  local case_dir="$tmp/${os}-${arch}"
  mkdir -p "$case_dir/home" "$case_dir/install"
  : > "$case_dir/urls"

  if HOME="$case_dir/home" \
    PATH="$fake_bin:/usr/bin:/bin" \
    TEST_OS="$os" \
    TEST_ARCH="$arch" \
    TEST_PAYLOAD="$tmp/payload" \
    TEST_URL_LOG="$case_dir/urls" \
    JCODE_INSTALL_DIR="$case_dir/install" \
    JCODE_NO_TELEMETRY=1 \
    bash "$repo_root/scripts/install.sh" >"$case_dir/out" 2>"$case_dir/err"; then
    printf 'installer accepted a mismatched checksum for %s/%s\n' "$os" "$arch" >&2
    exit 1
  fi

  grep -Fqx "https://github.com/kleksar/jcode/releases/download/v0.84.1/$artifact.tar.gz" "$case_dir/urls"
  grep -Fq "SHA-256 verification failed for $artifact.tar.gz" "$case_dir/err"
  if grep -Ev '^https://github[.]com/kleksar/jcode/' "$case_dir/urls" | grep -q .; then
    printf 'installer contacted a URL outside kleksar/jcode:\n' >&2
    grep -Ev '^https://github[.]com/kleksar/jcode/' "$case_dir/urls" >&2
    exit 1
  fi
  test ! -e "$case_dir/install/jcode"
}

run_case Darwin arm64 jcode-macos-aarch64

printf 'fork installer tests passed: macOS Apple Silicon mapping and checksum failure\n'
