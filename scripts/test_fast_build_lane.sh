#!/usr/bin/env bash
set -euo pipefail
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/cache" "$tmp/one/scripts" "$tmp/two/scripts"
cp "$repo_root/scripts/fast_build.sh" "$tmp/one/scripts/"
cp "$repo_root/scripts/fast_build.sh" "$tmp/two/scripts/"
cat >"$tmp/bin/cargo-runner" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == --print-fast-build-fingerprint ]]; then
  { printf 'fake-runner-v1\nflags=%s\ntoolchain=%s\nwrapper=%s\n' "${RUSTFLAGS:-}" "${TEST_TOOLCHAIN:-}" "${RUSTC_WRAPPER:-}"; } | shasum -a 256 | awk '{print "fast_build_runner_fingerprint=" $1}'
  exit 0
fi
printf 'gate=%s target=%s args=' "${JCODE_DEV_CARGO_GATE_HELD:-0}" "${CARGO_TARGET_DIR:-}" >>"${TEST_CARGO_LOG:?}"
printf '%q ' "$@" >>"$TEST_CARGO_LOG"; printf '\n' >>"$TEST_CARGO_LOG"
[[ "${TEST_CARGO_SLEEP:-0}" == 0 ]] || sleep "$TEST_CARGO_SLEEP"
EOF
chmod +x "$tmp/bin/cargo-runner"
for d in "$tmp/one" "$tmp/two"; do
  cat >"$d/Cargo.toml" <<'EOF'
[package]
name = "lane-fixture"
version = "0.0.0"
EOF
  printf 'lock-v1\n' >"$d/Cargo.lock"
  git -C "$d" init -q; git -C "$d" config user.email test@example.invalid; git -C "$d" config user.name test
  git -C "$d" remote add origin https://example.invalid/jcode.git
  git -C "$d" add . && git -C "$d" commit -qm fixture
done
run() { local d=$1; shift; JCODE_CACHE_DIR="$tmp/cache" JCODE_FAST_CARGO="$tmp/bin/cargo-runner" TEST_CARGO_LOG="$tmp/cargo.log" "$d/scripts/fast_build.sh" "$@"; }
key() { run "$1" status | sed -n 's/^compatibility_key=//p'; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "expected $1 to contain $2"; }
[[ "$(key "$tmp/one")" == "$(key "$tmp/two")" ]] || fail 'worktree path changed key'
[[ "$(key "$tmp/one")" != "$(RUSTFLAGS=-Copt-level=1 key "$tmp/two")" ]] || fail 'effective flags did not change key'
[[ "$(key "$tmp/one")" != "$(TEST_TOOLCHAIN=mutated key "$tmp/two")" ]] || fail 'effective toolchain did not change key'
: >"$tmp/cargo.log"
run "$tmp/one" cargo check --target wasm32-wasip1
contains "$tmp/cargo.log" 'gate=1'
contains "$tmp/cargo.log" 'args=check --target wasm32-wasip1'
# Fixed grammar requires the filter immediately after test and preserves argv.
for bad in 'focused test' 'focused test --test integration_suite'; do
  if run "$tmp/one" $bad >/dev/null 2>&1; then fail "accepted $bad"; fi
done
run "$tmp/one" focused test exact_name --test integration_suite -- --nocapture
contains "$tmp/cargo.log" 'args=test exact_name --test integration_suite -- --nocapture'
# Explicit candidate package works; no selected package must not fall back to workspace check.
: >"$tmp/cargo.log"
JCODE_FAST_CHANGED_PACKAGES=lane-fixture run "$tmp/one" candidate test exact_name
contains "$tmp/cargo.log" 'args=check -p lane-fixture'
git -C "$tmp/one" clean -fdq
if run "$tmp/one" candidate test exact_name >/dev/null 2>&1; then fail 'empty candidate silently checked workspace'; fi
# An untracked package maps to its nearest manifest.
mkdir -p "$tmp/one/crates/untracked"
printf '[package]\nname = "untracked-fixture"\nversion = "0.0.0"\n' >"$tmp/one/crates/untracked/Cargo.toml"
printf 'x\n' >"$tmp/one/crates/untracked/src.rs"
: >"$tmp/cargo.log"; run "$tmp/one" candidate test exact_name
contains "$tmp/cargo.log" 'args=check -p untracked-fixture'
# A paused mkdir winner is ownerless but active: contender honors its timeout.
: >"$tmp/cargo.log"
JCODE_FAST_TEST_PAUSE_BEFORE_OWNER=3 JCODE_CACHE_DIR="$tmp/cache" JCODE_FAST_CARGO="$tmp/bin/cargo-runner" TEST_CARGO_LOG="$tmp/cargo.log" "$tmp/one/scripts/fast_build.sh" cargo check >/dev/null 2>&1 & holder=$!
sleep 1
if JCODE_FAST_BUILD_LOCK_TIMEOUT=1 JCODE_FAST_BUILD_LOCK_GRACE=1 run "$tmp/two" cargo check >"$tmp/ownerless.out" 2>&1; then fail 'contender removed ownerless active lock'; fi
contains "$tmp/ownerless.out" 'timed out'
wait "$holder"
# A symlink lock component is rejected and the target is untouched.
lock=$(run "$tmp/one" status | sed -n 's/^lock_dir=//p')
mkdir -p "$(dirname "$lock")" "$tmp/safe"; : >"$tmp/safe/sentinel"; ln -s "$tmp/safe" "$lock"
if run "$tmp/one" cargo check >/dev/null 2>&1; then fail 'accepted symlink lock'; fi
[[ -f "$tmp/safe/sentinel" ]] || fail 'symlink target was modified'
rm "$lock"
# Different effective keys acquire independently, while wrapper receives skip-gate proof.
: >"$tmp/cargo.log"
RUSTFLAGS=-Ca JCODE_FAST_CARGO="$tmp/bin/cargo-runner" JCODE_CACHE_DIR="$tmp/cache" TEST_CARGO_LOG="$tmp/cargo.log" TEST_CARGO_SLEEP=2 "$tmp/one/scripts/fast_build.sh" cargo check >/dev/null & first=$!
sleep 1
RUSTFLAGS=-Cb JCODE_FAST_CARGO="$tmp/bin/cargo-runner" JCODE_CACHE_DIR="$tmp/cache" TEST_CARGO_LOG="$tmp/cargo.log" "$tmp/two/scripts/fast_build.sh" cargo check
wait "$first"
[[ $(grep -c 'gate=1' "$tmp/cargo.log") -eq 2 ]] || fail 'wrapper did not receive nested gate bypass'
echo 'fast build lane tests passed'
