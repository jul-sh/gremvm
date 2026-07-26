#!/bin/sh

set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)

output=$(HOME=$(mktemp -d /tmp/gremvm-test-home.XXXXXX) "$repo/bin/gremvm" --help)
printf '%s\n' "$output" | grep 'provision' > /dev/null
printf '%s\n' "$output" | grep 'restart' > /dev/null
if printf '%s\n' "$output" | grep -Eq 'backup|console|--destination'; then
    exit 1
fi
