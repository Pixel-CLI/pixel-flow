#!/bin/sh
# SPDX-FileCopyrightText: The Pixel contributors
# SPDX-License-Identifier: MIT
#
# The Pixel core commit is pinned in ONE place, [workspace.dependencies] of the
# root Cargo.toml. Every git entry there must name the same full commit, and
# nothing outside it may name a git source.
set -eu

cd "$(dirname "$0")/.."
revs=$(grep -E '^[A-Za-z0-9_-]+ = \{.*git = "https://github.com/Pixel-CLI/pixel"' Cargo.toml \
  | sed -E 's/.*rev = "([0-9a-f]{40})".*/\1/' | sort -u)
count=$(printf '%s\n' "$revs" | grep -c . || true)
if [ "$count" != 1 ]; then
  echo "expected exactly one pinned Pixel commit in Cargo.toml, found $count:" >&2
  printf '%s\n' "$revs" >&2
  exit 1
fi
if printf '%s' "$revs" | grep -qvE '^[0-9a-f]{40}$'; then
  echo "the pin must be a full 40-hex commit id" >&2
  exit 1
fi
stray=$(grep -rln 'git = "' crates --include=Cargo.toml || true)
if [ -n "$stray" ]; then
  echo "git dependencies belong in the root Cargo.toml only:" >&2
  echo "$stray" >&2
  exit 1
fi
echo "pinned Pixel commit: $revs"
