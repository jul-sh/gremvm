#!/bin/sh

set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$repo"

shellcheck bin/gremvm scripts/*.sh tests/*.sh
shfmt -d -i 4 -ci -sr bin/gremvm scripts tests
nixfmt --check flake.nix
bats tests

printf 'all checks passed\n'
