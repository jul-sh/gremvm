#!/bin/sh
set -eu
umask 077

program=${0##*/}
repo=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
temporary_home=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/gremvm-wrangler.XXXXXX")

die() {
    echo "$program: $*" >&2
    exit 1
}
command -v wrangler > /dev/null || die "missing dependency 'wrangler'; run through nix develop path:.#cloudflare"

cleanup() {
    HOME=$temporary_home XDG_CONFIG_HOME=$temporary_home wrangler logout > /dev/null 2>&1 || true
    /bin/rm -rf "$temporary_home"
}
trap cleanup EXIT HUP INT TERM

HOME=$temporary_home XDG_CONFIG_HOME=$temporary_home wrangler login
HOME=$temporary_home XDG_CONFIG_HOME=$temporary_home GREMVM_CLOUDFLARE_AUTH=wrangler \
    "$repo/scripts/cloudflare-setup.sh" "$@"
