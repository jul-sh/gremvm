#!/bin/sh
set -eu
umask 077

program=${0##*/}
repo=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
secret=$repo/secrets/CLOUDFLARE_TUNNEL_CREDENTIALS.age
root=$HOME/Library/Application\ Support/GremVM
destination=$root/cloudflare/credentials.json

die() {
    echo "$program: $*" >&2
    exit 1
}
for dependency in jq keytap; do command -v "$dependency" > /dev/null || die "missing dependency '$dependency'; run through nix develop"; done

[ -r "$secret" ] || die "missing encrypted tunnel credentials: $secret; run cloudflare-setup.sh apply first"
[ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$secret")" -eq 1 ] || die "tunnel credentials must have exactly one Keytap recipient"
[ ! -L "$root" ] || die "deployment root may not be a symlink"
[ ! -e "$destination" ] || { [ -f "$destination" ] && [ ! -L "$destination" ]; } || die "unsafe existing credentials path"

credentials=$(keytap decrypt keytap < "$secret")
canonical=$(printf '%s' "$credentials" | jq -ce '
    select(type == "object") |
    select(.AccountTag | type == "string" and test("^[0-9a-f]{32}$")) |
    select(.TunnelID | type == "string" and test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")) |
    select(.TunnelName == "gremvm") |
    select(.TunnelSecret | type == "string" and test("^[A-Za-z0-9+/]{43}=$")) |
    {AccountTag, TunnelID, TunnelName, TunnelSecret}
') || die "invalid tunnel credentials"
unset credentials

/bin/mkdir -p "$root/cloudflare"
/bin/chmod 700 "$root" "$root/cloudflare"
tmp=$(/usr/bin/mktemp "$root/cloudflare/.credentials.XXXXXX")
cleanup() { /bin/rm -f "$tmp"; }
trap cleanup EXIT HUP INT TERM
printf '%s\n' "$canonical" > "$tmp"
unset canonical
/bin/chmod 600 "$tmp"
/bin/mv -f "$tmp" "$destination"
trap - EXIT HUP INT TERM

echo "Installed the tunnel-specific credential at $destination (mode 0600)."
echo "Run gremvm restart after the VM has been provisioned."
