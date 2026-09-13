#!/usr/bin/env bash
# Shared, serialized incremental Cargo lane for sibling Jcode worktrees.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
cache_root="${JCODE_CACHE_DIR:-${HOME:?HOME must be set}/.jcode/cache}"
lock_timeout="${JCODE_FAST_BUILD_LOCK_TIMEOUT:-1800}"
lock_grace="${JCODE_FAST_BUILD_LOCK_GRACE:-2}"
cargo_runner="${JCODE_FAST_CARGO:-$repo_root/scripts/dev_cargo.sh}"
lock_dir='' lock_identity='' lock_token='' lock_owned=0 compat_key='' target_dir='' lane_target=''

log() { printf 'fast_build: %s\n' "$*" >&2; }
die() { log "$*"; exit 2; }
hash_text() { if command -v shasum >/dev/null 2>&1; then shasum -a 256 | awk '{print $1}'; elif command -v sha256sum >/dev/null 2>&1; then sha256sum | awk '{print $1}'; else die 'need shasum or sha256sum'; fi; }
repo_identity() { local v; v=$(git -C "$repo_root" config --get remote.origin.url 2>/dev/null || true); [[ -n "$v" ]] || v="workspace:$(hash_text < "$repo_root/Cargo.toml")"; printf '%s\n' "$v"; }
lock_hash() { hash_text < "$repo_root/Cargo.lock"; }

# dev_cargo prints only a digest after applying its toolchain, linker, wrapper,
# feature, and flag policy. The digest intentionally cannot disclose env values.
runner_fingerprint() {
  local output
  output=$("$cargo_runner" --print-fast-build-fingerprint "$@") || die 'dev_cargo could not determine the effective runner fingerprint'
  [[ "$output" =~ ^fast_build_runner_fingerprint=[a-f0-9]{64}$ ]] || die 'runner did not provide a valid effective fingerprint'
  printf '%s\n' "${output#*=}"
}
compatibility_key() {
  { printf 'repo=%s\nlock=%s\nrunner=%s\ntarget=%s\n' "$(repo_identity)" "$(lock_hash)" "$(runner_fingerprint "$@")" "$lane_target"; } | hash_text | cut -c1-24
}
set_paths() { compat_key=$(compatibility_key "$@"); target_dir="$cache_root/fast-build/targets/$compat_key"; lock_dir="$cache_root/fast-build/locks/$compat_key.lock"; }

# `test -L` is an lstat-style symlink check. Never traverse or remove a link.
path_identity() { [[ ! -L "$1" && -d "$1" ]] || return 1; stat -f '%d:%i' "$1" 2>/dev/null || stat -c '%d:%i' "$1" 2>/dev/null; }
regular_file() { [[ ! -L "$1" && -f "$1" ]]; }
file_identity() { regular_file "$1" || return 1; stat -f '%d:%i' "$1" 2>/dev/null || stat -c '%d:%i' "$1" 2>/dev/null; }
owner_record() { # token pid process-start marker
  local f=$1 token pid start
  regular_file "$f" || return 1
  IFS=' ' read -r token pid start < "$f" || return 1
  [[ "$token" =~ ^[A-Za-z0-9._-]+$ && "$pid" =~ ^[0-9]+$ && "$start" =~ ^[a-f0-9]{64}$ ]] || return 1
  printf '%s %s %s\n' "$token" "$pid" "$start"
}
pid_start() { ps -o lstart= -p "$1" 2>/dev/null | tr -s ' ' | hash_text 2>/dev/null || true; }
owner_is_live() { local token pid start now; read -r token pid start < <(owner_record "$1") || return 1; kill -0 "$pid" 2>/dev/null || return 1; now=$(pid_start "$pid"); [[ -n "$now" && "$now" == "$start" ]]; }
timed_out() { (( SECONDS - lock_started >= lock_timeout )); }

release_lock() {
  local owner="$lock_dir/owner" token_file="$lock_dir/token.$lock_token" current
  [[ "$lock_owned" == 1 && -n "$lock_dir" ]] || return 0
  # Only remove the exact two regular files created by this process, and only
  # while the same directory inode remains. No recursive removal is ever used.
  if [[ "$(path_identity "$lock_dir" 2>/dev/null || true)" == "$lock_identity" ]] && current=$(owner_record "$owner" 2>/dev/null) && [[ "${current%% *}" == "$lock_token" ]] && [[ "$(file_identity "$owner" 2>/dev/null || true)" == "$(file_identity "$token_file" 2>/dev/null || true)" ]]; then
    rm "$owner" 2>/dev/null || true
    rm "$token_file" 2>/dev/null || true
    rmdir "$lock_dir" 2>/dev/null || log "could not remove owned lock $lock_dir"
  fi
  lock_owned=0
}
recover_stale_lock() {
  local first second owner="$lock_dir/owner" record token pid start token_file
  first=$(path_identity "$lock_dir") || die "refusing symlink or non-directory lock component: $lock_dir"
  sleep "$lock_grace"
  timed_out && return 1
  second=$(path_identity "$lock_dir") || die "refusing changed lock component: $lock_dir"
  [[ "$first" == "$second" ]] || return 1
  record=$(owner_record "$owner" 2>/dev/null || true)
  # A winner can be between mkdir and owner publication. It is active for the
  # grace interval. Afterwards recovery requires a stable dead owner record.
  [[ -n "$record" ]] || return 1
  read -r token pid start <<<"$record"
  owner_is_live "$owner" && return 1
  token_file="$lock_dir/token.$token"
  [[ "$(file_identity "$owner" 2>/dev/null || true)" == "$(file_identity "$token_file" 2>/dev/null || true)" ]] || return 1
  [[ "$(path_identity "$lock_dir" 2>/dev/null || true)" == "$second" ]] || return 1
  rm "$owner" 2>/dev/null || return 1
  rm "$token_file" 2>/dev/null || return 1
  rmdir "$lock_dir" 2>/dev/null || return 1
  log "recovered stale build-lane lock $lock_dir"
  return 0
}
acquire_lock() {
  [[ "${JCODE_FAST_BUILD_LOCK_KEY:-}" == "$compat_key" ]] && return 0
  mkdir -p "$(dirname "$lock_dir")"
  lock_started=$SECONDS
  while :; do
    timed_out && die "timed out after ${lock_timeout}s waiting for shared build lane $compat_key. It was not killed."
    if mkdir "$lock_dir" 2>/dev/null; then
      lock_identity=$(path_identity "$lock_dir") || die "new lock is unsafe: $lock_dir"
      # Test-only deterministic race hook. Production callers never set it.
      [[ -z "${JCODE_FAST_TEST_PAUSE_BEFORE_OWNER:-}" ]] || sleep "$JCODE_FAST_TEST_PAUSE_BEFORE_OWNER"
      lock_token="$$.${RANDOM}.${RANDOM}"
      local token_file="$lock_dir/token.$lock_token" start
      start=$(pid_start "$$"); [[ -n "$start" ]] || die 'could not identify lock owner process'
      (umask 077; printf '%s %s %s\n' "$lock_token" "$$" "$start" > "$token_file")
      # Atomic hard-link publication: owner and token are the same regular inode.
      ln "$token_file" "$lock_dir/owner" || { rm "$token_file"; rmdir "$lock_dir"; die 'could not publish lock ownership'; }
      lock_owned=1
      export JCODE_FAST_BUILD_LOCK_KEY="$compat_key" JCODE_DEV_CARGO_GATE_HELD=1
      trap release_lock EXIT
      trap 'release_lock; exit 128' HUP INT TERM
      log "acquired shared build lane $compat_key"
      return 0
    fi
    [[ -L "$lock_dir" || -d "$lock_dir" ]] || die "refusing unsafe lock component: $lock_dir"
    if ! recover_stale_lock; then
      timed_out && die "timed out after ${lock_timeout}s waiting for shared build lane $compat_key. It was not killed."
      sleep 1
    fi
  done
}
run_cargo() { [[ $# -gt 0 ]] || die 'cargo requires Cargo arguments'; mkdir -p "$target_dir"; export CARGO_TARGET_DIR="$target_dir"; [[ -z "${JCODE_FAST_BUILD_TARGET:-}" ]] || export CARGO_BUILD_TARGET="$JCODE_FAST_BUILD_TARGET"; "$cargo_runner" "$@"; }

require_focused_test() {
  [[ "${1:-}" == test ]] || die 'focused syntax: focused <test-filter> [-- cargo test selectors...]'
  local filter=${2:-}; [[ -n "$filter" && "$filter" != -* ]] || die 'focused requires a non-option test filter as its second argument'
  shift 2
  # A selector before the filter is rejected by the fixed grammar, so `focused
  # test --test suite` cannot accidentally treat `suite` as the filter.
}
changed_packages() {
  if [[ -n "${JCODE_FAST_CHANGED_PACKAGES:-}" ]]; then printf '%s\n' "$JCODE_FAST_CHANGED_PACKAGES" | tr ',' '\n'; return; fi
  local base="${JCODE_FAST_BASE_REF:-HEAD~1}" path dir manifest package
  git -C "$repo_root" rev-parse --verify --quiet "$base" >/dev/null || base=HEAD
  { git -C "$repo_root" diff --name-only "$base"...HEAD; git -C "$repo_root" diff --name-only; git -C "$repo_root" ls-files --others --exclude-standard; } | sort -u | while IFS= read -r path; do
    [[ -n "$path" ]] || continue; dir="$repo_root/${path%/*}"; [[ "$path" == */* ]] || dir="$repo_root"
    while [[ "$dir" == "$repo_root"/* || "$dir" == "$repo_root" ]]; do
      manifest="$dir/Cargo.toml"; if [[ -f "$manifest" ]]; then package=$(awk -F'=' '/^name[[:space:]]*=/ {gsub(/[[:space:]\"]/, "", $2); print $2; exit}' "$manifest"); [[ -n "$package" ]] && printf '%s\n' "$package"; break; fi
      [[ "$dir" == "$repo_root" ]] && break; dir=${dir%/*}
    done
  done | sort -u
}
run_candidate_check() { local packages=() p; while IFS= read -r p; do [[ -z "$p" ]] || packages+=("$p"); done < <(changed_packages); ((${#packages[@]})) || die 'candidate found no changed package; set JCODE_FAST_CHANGED_PACKAGES explicitly'; local args=(check); for p in "${packages[@]}"; do args+=(-p "$p"); done; run_cargo "${args[@]}"; }
show_status() { set_paths "$@"; printf 'compatibility_key=%s\ncache_root=%s\ntarget_dir=%s\nlock_dir=%s\n' "$compat_key" "$cache_root" "$target_dir" "$lock_dir"; [[ -d "$target_dir" ]] && du -sh "$target_dir" || printf 'target_status=absent\n'; [[ -L "$lock_dir" ]] && printf 'lock_status=unsafe-symlink\n' || [[ -d "$lock_dir" ]] && printf 'lock_status=held\n' || printf 'lock_status=free\n'; }
usage() { cat <<'EOF'
Usage: scripts/fast_build.sh <cargo|focused|candidate|release|cache-path|status> [args]
  focused test <test-filter> [-- cargo test selectors...]
  candidate test <test-filter> [-- cargo test selectors...]
Candidate checks only explicitly mapped changed packages and fails when none map.
EOF
}
prepare_command_target() { lane_target="${JCODE_FAST_BUILD_TARGET:-${CARGO_BUILD_TARGET:-}}"; local previous='' arg; for arg in "$@"; do [[ "$previous" == --target ]] && { lane_target=$arg; return; }; case "$arg" in --target) previous=--target;; --target=*) lane_target=${arg#--target=}; return;; esac; done; }
command=${1:-}; case "$command" in
 cache-path) shift; prepare_command_target "$@"; set_paths "$@"; printf '%s\n' "$target_dir";;
 status) shift; prepare_command_target "$@"; show_status "$@";;
 cargo|focused|candidate|release) shift; prepare_command_target "$@"; set_paths "$@"; acquire_lock; case "$command" in cargo) run_cargo "$@";; focused) require_focused_test "$@"; run_cargo "$@";; candidate) require_focused_test "$@"; run_candidate_check; run_cargo "$@";; release) [[ $# -gt 0 ]] || die 'release requires Cargo arguments'; case " $* " in *' --release '*|*' -r '*) ;; *) die 'release requires --release or -r';; esac; run_cargo "$@";; esac;;
 -h|--help|help|'') usage;; *) die "unknown command '$command' (use --help)";; esac
