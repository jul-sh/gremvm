#!/bin/sh

# Opt-in acceptance check for a real provisioned host. This is intentionally
# not part of the hermetic test suite because it needs the running VM and LAN.
set -eu

GREMVM=${GREMVM:-$HOME/Library/Application\ Support/GremVM/bin/gremvm}
[ -x "$GREMVM" ] || {
    printf 'set GREMVM to the installed wrapper\n' >&2
    exit 1
}

access=$("$GREMVM" address)
guest_ip=$(printf '%s\n' "$access" | /usr/bin/awk '$1 == "ip:" { print $2 }')
case $guest_ip in
    '' | *[!0-9.]*)
        printf 'invalid guest address: %s\n' "$guest_ip" >&2
        exit 1
        ;;
esac

/usr/bin/nc -z -G 5 "$guest_ip" 22
/usr/bin/nc -z -G 5 "$guest_ip" 5900
banner=$(
    /usr/bin/ruby -rsocket -rtimeout -e '
        Timeout.timeout(5) do
          socket = TCPSocket.new(ARGV.fetch(0), 5900)
          STDOUT.write(socket.read(4))
        end
    ' "$guest_ip"
)
[ "$banner" = 'RFB ' ] || {
    printf 'port 5900 did not return an RFB banner\n' >&2
    exit 1
}

printf 'LAN SSH and Screen Sharing are reachable at %s.\n' "$guest_ip"
printf 'Complete the control check from a second Mac with: open vnc://admin@%s\n' "$guest_ip"
