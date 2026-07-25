#!/bin/sh
set -eu
umask 077

repo=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
source_repo=${KEYTAP_SOURCE_REPO:-$HOME/git/keytap}
reader=$source_repo/distribution/read-secret.sh
output=$repo/secrets/CLOUDFLARE_API_TOKEN.age

[ -x "$reader" ] || {
    echo "missing Keytap source-secret reader: $reader" >&2
    exit 1
}
command -v keytap > /dev/null || {
    echo 'keytap is required; run this through nix develop' >&2
    exit 1
}

tmp=$(/usr/bin/mktemp "$repo/secrets/.cloudflare-api.XXXXXX")
cleanup() { /bin/rm -f "$tmp"; }
trap cleanup EXIT HUP INT TERM

# No --to or -R: the derived Keytap identity is the only recipient.
token=$("$reader" CLOUDFLARE_API_TOKEN)
[ -n "$token" ] || {
    echo 'source Cloudflare token is empty' >&2
    exit 1
}
printf '%s' "$token" | keytap encrypt keytap > "$tmp"
unset token
[ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$tmp")" -eq 1 ] || {
    echo 'refusing secret with anything other than one age recipient' >&2
    exit 1
}
/bin/chmod 600 "$tmp"
/bin/mv -f "$tmp" "$output"
trap - EXIT HUP INT TERM
echo "Imported $output with the Keytap-only recipient."
