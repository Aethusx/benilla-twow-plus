#!/usr/bin/env bash
# Sync this fork with upstream benilla: fetch, merge upstream's main into the current branch, then
# build and test the crates on top (nampower, unitxp, superwow, the benilla-mods launcher) and the benilla
# crates their hooks live in.
#
# Merge conflicts can only land in the hook call sites crates/nampower/README.md,
# crates/unitxp/README.md and crates/superwow/README.md list; the script stops at a conflict and names the files.
#
#   scripts/nampower-sync.sh              # remote `upstream`, branch `main`
#   UPSTREAM=origin BRANCH=main scripts/nampower-sync.sh
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

remote="${UPSTREAM:-upstream}"
branch="${BRANCH:-main}"

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "nampower-sync: the tree has uncommitted changes; commit or stash them first" >&2
    exit 1
fi
if ! git remote get-url "$remote" >/dev/null 2>&1; then
    echo "nampower-sync: no remote '$remote' (git remote add $remote <url>)" >&2
    exit 1
fi

git fetch "$remote" "$branch"
if ! git merge --no-edit "$remote/$branch"; then
    echo "nampower-sync: merge stopped on conflicts in:" >&2
    git diff --name-only --diff-filter=U >&2
    echo "keep upstream's side, re-apply the hook lines (crates/*/README.md), then" >&2
    echo "  git add <files> && git commit && scripts/nampower-sync.sh" >&2
    exit 1
fi

cargo build -p benilla-mods
cargo test -p nampower -p unitxp -p superwow
cargo clippy -p nampower -p unitxp -p superwow -p benilla-mods --all-targets -- -D warnings
cargo clippy -p benilla-app -p benilla-world -p benilla-ui -- -D warnings
echo "nampower-sync: merged $remote/$branch; the crates on top build and their tests pass"
