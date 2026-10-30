#!/usr/bin/env bash
# A job's cargo target/ and crate downloads, kept on the cluster's cache server between runs
# (what GitHub's rust-cache did with GitHub's storage). See action.yml.
#
#   snapshot.sh restore <job> <workspace dir>   before the build; sets RUST_SNAPSHOT_* for save
#   snapshot.sh save                            after the job's checks passed
#
# Never fails the job: a snapshot that can't be fetched or stored only makes the job slower.
set -uo pipefail

server=${SCCACHE_WEBDAV_ENDPOINT%/}/target

# The workspace's own crates leave target/ before a snapshot is stored and again after one is
# restored. That is what makes sharing target/ between runs safe: cargo would take a workspace
# crate's artifact as current when it is newer than the crate's sources, and a snapshot stored by
# another branch's job during this run would be. Without them, each run compiles its own crates
# from its own checkout, and target/ holds only the dependencies (from crates.io, which cargo
# identifies by version, never by file times).
prune() {
  local ws=$1 debug=$1/target/debug meta packages targets
  [ -d "$debug" ] || return 0
  meta=$(cd "$ws" && cargo metadata --no-deps --offline --format-version 1) || return 1
  packages=$(jq -r '[.packages[].name] | join("|")' <<<"$meta")
  targets=$(jq -r '[.packages[].targets[].name | gsub("-"; "_")] | unique | join("|")' <<<"$meta")
  # The top level holds only copies of the workspace's own binaries and libraries.
  find "$debug" -maxdepth 1 -type f -delete
  rm -rf "$debug/incremental"
  find "$debug/.fingerprint" "$debug/build" -maxdepth 1 -regextype posix-extended \
    -regex ".*/($packages)-[0-9a-f]{16}" -exec rm -rf {} + 2>/dev/null
  find "$debug/deps" -maxdepth 1 -regextype posix-extended \
    -regex ".*/(lib)?($targets)-[0-9a-f]{16}(\..*)?" -delete 2>/dev/null
  return 0
}

# Only what this run's build used goes into a snapshot. Without this, a snapshot restored from
# another key (main's, after Cargo.lock or a build setting changed) keeps its old artifacts beside
# the new ones, and every change adds more. cargo reports every unit of a build, fresh ones too,
# with the files it produced; anything else in deps/ and build/ is left over. Deleting a file
# cargo still wanted only costs a rebuild of that unit on the next run, never a wrong result.
sweep() {
  local ws=$1 debug=$1/target/debug
  [ -d "$debug/deps" ] || return 0
  local -A used=()
  local h
  while read -r h; do used[$h]=1; done < <(cd "$ws" && {
    cargo test --no-run --message-format=json 2>/dev/null
    cargo clippy --all-targets --message-format=json 2>/dev/null
  } | jq -r '(select(.reason == "compiler-artifact") | .filenames[]),
             (select(.reason == "build-script-executed") | .out_dir)' \
    | grep -oE -- '-[0-9a-f]{16}')
  [ ${#used[@]} -gt 0 ] || return 0
  local f
  for f in "$debug"/deps/* "$debug"/build/*; do
    h=$(grep -oE -- '-[0-9a-f]{16}' <<<"${f##*/}" | tail -1)
    [ -n "$h" ] && [ -z "${used[$h]:-}" ] && rm -rf "$f"
  done
  return 0
}

restore() {
  local job=$1 ws
  ws=$(cd "$2" && pwd)
  [ -n "${SCCACHE_WEBDAV_ENDPOINT:-}" ] || { echo "No cache server for this run."; return 0; }
  # A snapshot fits only the same rustc, dependencies, build settings and paths (CARGO_HOME and the
  # checkout are the same on every runner).
  # format: bump to retire every stored snapshot, as when what goes into one changes (sweep).
  local format=2 rustc deps key
  rustc=$(rustc -vV | sha256sum | cut -c1-12)
  deps=$(
    { echo "format $format"; cat "$ws/Cargo.lock" "$ws"/rust-toolchain* "$CARGO_HOME/config.toml" 2>/dev/null
      env | grep -E '^(CARGO_HOME|CARGO_INCREMENTAL|CARGO_PROFILE_[A-Z_]+|RUSTC_WRAPPER|RUSTFLAGS|RUSTDOCFLAGS)=' | sort
    } | sha256sum | cut -c1-12
  )
  key=$job-$rustc-$deps
  {
    echo "RUST_SNAPSHOT_DIR=$ws"
    echo "RUST_SNAPSHOT_KEY=$key"
    echo "RUST_SNAPSHOT_MAIN=$job-$rustc-main"
  } >> "$GITHUB_ENV"

  # Main's snapshot for this rustc is what a branch with new dependencies starts from; only main
  # stores it.
  if [ "$GITHUB_REF" = refs/heads/main ] && ! exists "$job-$rustc-main"; then
    echo "RUST_SNAPSHOT_SAVE_MAIN=true" >> "$GITHUB_ENV"
  fi
  local name
  for name in "$key" "$job-$rustc-main"; do
    if exists "$name" && fetch "$name" "$ws"; then
      echo "Restored $name."
      prune "$ws"
      [ "$name" = "$key" ] || echo "RUST_SNAPSHOT_SAVE=true" >> "$GITHUB_ENV"
      return 0
    fi
  done
  echo "No snapshot for $key, nor main's."
  echo "RUST_SNAPSHOT_SAVE=true" >> "$GITHUB_ENV"
}

exists() {
  curl -sSf --connect-timeout 5 -o /dev/null -I "$server/$1.tar.zst" 2>/dev/null
}

# Into the paths it was taken from. A broken transfer leaves nothing half-restored.
fetch() {
  local name=$1 ws=$2
  local start=$SECONDS size
  size=$(curl -sSfI --connect-timeout 5 "$server/$name.tar.zst" | tr -d '\r' | awk 'tolower($1) == "content-length:" { print $2 }')
  # tar --totals: the bytes written to disk, which is what a slow disk pays for.
  if curl -sSf --connect-timeout 5 "$server/$name.tar.zst" | zstd -dq | tar -xf - -C / --totals; then
    echo "$name: $(( ${size:-0} / 1048576 )) MiB in $(( SECONDS - start ))s"
    return 0
  fi
  echo "::warning::Snapshot $name couldn't be restored; building without it."
  rm -rf "$ws/target" "$CARGO_HOME/registry" "$CARGO_HOME/git"
  return 1
}

save() {
  local names=()
  [ "${RUST_SNAPSHOT_SAVE:-}" = true ] && names+=("$RUST_SNAPSHOT_KEY")
  if [ "$GITHUB_REF" = refs/heads/main ] && { [ "${RUST_SNAPSHOT_SAVE:-}" = true ] || [ "${RUST_SNAPSHOT_SAVE_MAIN:-}" = true ]; }; then
    names+=("$RUST_SNAPSHOT_MAIN")
  fi
  [ ${#names[@]} -gt 0 ] || { echo "Snapshot ${RUST_SNAPSHOT_KEY:-} is current."; return 0; }
  local ws=$RUST_SNAPSHOT_DIR file=$RUNNER_TEMP/snapshot.tar.zst paths=()
  sweep "$ws"
  prune "$ws" || return 0
  local p
  local cargo_home
  cargo_home=$(realpath -m "$CARGO_HOME")
  for p in "$ws/target" "$cargo_home/registry/index" "$cargo_home/registry/cache" "$cargo_home/git/db"; do
    [ -e "$p" ] && paths+=("${p#/}")
  done
  # Whole into a file first: a PUT that stopped half-way through a stream would store a
  # truncated snapshot. The server writes each upload to a temporary file and renames it into
  # place (homelab repo, ci-runners/manifests/sccache.yaml), so a reader never sees half of one.
  tar -cf - -C / --totals "${paths[@]}" | zstd -q -f -T4 -3 -o "$file" || { echo "::warning::Snapshot not stored."; return 0; }
  echo "Snapshot: $(( $(stat -c %s "$file") / 1048576 )) MiB"
  local name
  for name in "${names[@]}"; do
    curl -sSf --connect-timeout 5 -T "$file" "$server/$name.tar.zst" \
      && echo "Stored $name." || echo "::warning::Snapshot $name not stored."
  done
  rm -f "$file"
}

case ${1:-} in
  restore) restore "$2" "$3" ;;
  save) save ;;
  *) echo "usage: $0 restore <job> <workspace> | save" >&2; exit 2 ;;
esac
exit 0
