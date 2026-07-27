#!/bin/sh

set -eu

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
test_home=$(/usr/bin/mktemp -d /tmp/gremvm-test-home.XXXXXX)

cleanup() {
    /bin/rm -R "$test_home"
}
trap cleanup EXIT HUP INT TERM

output=$(HOME="$test_home" "$repo/bin/gremvm" --help)
for required in install provision status start stop restart address ssh screen bridge logs uninstall; do
    printf '%s\n' "$output" | /usr/bin/grep "$required" > /dev/null
done

[ "$(HOME="$test_home" "$repo/bin/gremvm" status)" = 'state: not-installed' ]
[ -f "$repo/versions/tart.env" ]
