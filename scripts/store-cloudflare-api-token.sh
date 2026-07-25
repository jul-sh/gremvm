#!/bin/sh
set -eu
umask 077

repo=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
output=$repo/secrets/CLOUDFLARE_API_TOKEN.age
tmp=$(/usr/bin/mktemp "$repo/secrets/.cloudflare-api.XXXXXX")
terminal_echo=false

cleanup() {
    if [ "$terminal_echo" = true ]; then
        /bin/stty echo < /dev/tty 2> /dev/null || true
    fi
    /bin/rm -f "$tmp"
}
trap cleanup EXIT HUP INT TERM

command -v keytap > /dev/null || {
    echo 'keytap is required; run this through nix develop' >&2
    exit 1
}
[ -t 0 ] && [ -r /dev/tty ] || {
    echo 'run this script interactively so the Cloudflare token is never passed as an argument or environment variable' >&2
    exit 1
}

printf 'Paste the new GremVM Cloudflare API token (input hidden): ' >&2
/bin/stty -echo < /dev/tty
terminal_echo=true
IFS= read -r token < /dev/tty
/bin/stty echo < /dev/tty
terminal_echo=false
printf '\n' >&2
[ -n "$token" ] || {
    echo 'Cloudflare API token is empty' >&2
    exit 1
}

# No --to or -R: the derived Keytap identity is the only recipient.
printf '%s' "$token" | keytap encrypt keytap > "$tmp"
unset token
[ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$tmp")" -eq 1 ] || {
    echo 'refusing secret with anything other than one age recipient' >&2
    exit 1
}
/bin/chmod 600 "$tmp"
/bin/mv -f "$tmp" "$output"
trap - EXIT HUP INT TERM
echo "Stored $output with the Keytap-only recipient."
