#!/bin/sh

set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
cd "$repo"

need() {
    command -v "$1" > /dev/null 2>&1 || {
        echo "missing check dependency '$1'; run: nix develop path:. -c ./scripts/check.sh" >&2
        exit 1
    }
}

for dependency in bats nixfmt shellcheck shfmt; do
    need "$dependency"
done

shellcheck bin/gremvm scripts/*.sh tests/*.sh
shfmt -d -i 4 -ci -sr bin/gremvm scripts tests
nixfmt --check flake.nix

bats tests
sh tests/smoke.sh

printf 'all checks passed\n'
