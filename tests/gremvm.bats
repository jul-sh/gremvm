#!/usr/bin/env bats

setup() {
    export REPO_ROOT="$BATS_TEST_DIRNAME/.."
    export TEST_HOME="$BATS_TEST_TMPDIR/home"
    export TEST_ROOT="$TEST_HOME/Library/Application Support/GremVM"
    export TEST_BIN="$BATS_TEST_TMPDIR/bin"
    export TEST_SECRET="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    export TEST_CREDENTIALS="{\"AccountTag\":\"0123456789abcdef0123456789abcdef\",\"TunnelID\":\"12345678-1234-1234-1234-123456789abc\",\"TunnelName\":\"gremvm\",\"TunnelSecret\":\"$TEST_SECRET\"}"
    mkdir -p "$TEST_HOME" "$TEST_BIN"
    {
        printf '%s\n' '#!/bin/sh' 'set -eu'
        # These expansions belong to the generated fake.
        # shellcheck disable=SC2016
        printf '%s\n' '[ "$#" -eq 2 ]' '[ "$1" = decrypt ]' '[ "$2" = keytap ]' 'cat >/dev/null' 'printf "%s\n" "${FAKE_KEYTAP_VALUE:?}"'
    } > "$TEST_BIN/keytap"
    chmod 755 "$TEST_BIN/keytap"
    LOCK_HOLDER=
}

teardown() {
    if [ -n "$LOCK_HOLDER" ]; then
        kill "$LOCK_HOLDER" 2> /dev/null || true
        wait "$LOCK_HOLDER" 2> /dev/null || true
    fi
}

@test "help exposes the required lifecycle commands" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" --help
    [ "$status" -eq 0 ] || return 1
    for command in install provision status start stop restart console logs backup runtime-path uninstall; do
        [[ "$output" == *"$command"* ]] || return 1
    done
}

@test "status is a typed not-installed state in an empty home" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ] || return 1
    [ "$output" = "state: not-installed" ]
}

@test "unknown commands fail closed" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" destroy-everything
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"unknown command"* ]]
}

@test "Tart release pin is exact and complete" {
    run sh -c '. "$1"; printf "%s|%s|%s|%s|%s|%s|%s" "$TART_VERSION" "$TART_RELEASE_COMMIT" "$TART_ARCHIVE_URL" "$TART_ARCHIVE_SHA256" "$TART_ARCHIVE_NIX_SHA256" "$TART_BUNDLE_ID" "$TART_TEAM_ID"' sh "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ] || return 1
    [ "$output" = "2.34.0|cbc160a592cb849eac01fa040ea18d2bfa9039a4|https://github.com/openai/tart/releases/download/2.34.0/tart.tar.gz|c9f1609f4945258ef0ee277dd44dc9700d6f069789a11e43be7f35b0a64b3135|sha256-yfFgn0lFJY7w7id91E3JcA1vBpeJoR5Dvn81sKZLMTU=|com.github.cirruslabs.tart|9M2P8L4D89" ]
}

@test "cloudflared is pinned through Nix" {
    run sh -c '. "$1"; printf "%s" "$CLOUDFLARED_VERSION"' sh "$REPO_ROOT/versions/cloudflared.env"
    [ "$status" -eq 0 ] || return 1
    [ "$output" = "2026.5.2" ] || return 1
    run grep -F 'GREMVM_BUNDLED_CLOUDFLARED=${pkgs.cloudflared}/bin/cloudflared' "$REPO_ROOT/flake.nix"
    [ "$status" -eq 0 ]
}

@test "no personal signing material remains in the active tree" {
    run find "$REPO_ROOT" -path "$REPO_ROOT/.git" -prune -o -type f \( \
        -name '*.age.b64' -o -name '*.cer' -o -name '*.key' -o \
        -name '*.mobileprovision' -o -name '*.p8' -o -name '*.p12' -o \
        -name '*.pem' -o -name '*.pfx' -o -name '*.provisionprofile' \
        \) -print
    [ "$status" -eq 0 ] || return 1
    [ -z "$output" ]
}

@test "installed-style symlink resolves repository pins" {
    mkdir -p "$BATS_TEST_TMPDIR/linked-bin"
    ln -s "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR/linked-bin/gremvm"
    run env HOME="$TEST_HOME" "$BATS_TEST_TMPDIR/linked-bin/gremvm" status
    [ "$status" -eq 0 ] || return 1
    [ "$output" = "state: not-installed" ]
}

@test "unsafe VM names are structurally rejected" {
    run env HOME="$TEST_HOME" GREMVM_VM_NAME=.. "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"invalid VM name"* ]] || return 1
    run env HOME="$TEST_HOME" GREMVM_VM_NAME=-work "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"invalid VM name"* ]]
}

@test "uninstall preserves VM data and Cloudflare credentials" {
    if [ ! -x /usr/bin/lockf ]; then
        skip "Darwin Nix build sandbox hides lockf"
    fi
    mkdir -p "$TEST_ROOT/tart/vms/work" "$TEST_ROOT/cloudflare" "$TEST_ROOT/runtime" "$TEST_ROOT/bin" "$TEST_ROOT/versions"
    printf 'preserve\n' > "$TEST_ROOT/tart/vms/work/sentinel"
    printf 'preserve\n' > "$TEST_ROOT/cloudflare/credentials.json"
    printf 'remove\n' > "$TEST_ROOT/runtime/sentinel"
    printf 'remove\n' > "$TEST_ROOT/bin/sentinel"
    printf 'remove\n' > "$TEST_ROOT/versions/sentinel"

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" uninstall
    [ "$status" -eq 0 ] || return 1
    [ -f "$TEST_ROOT/tart/vms/work/sentinel" ] || return 1
    [ -f "$TEST_ROOT/cloudflare/credentials.json" ] || return 1
    [ ! -e "$TEST_ROOT/runtime" ] || return 1
    [ ! -e "$TEST_ROOT/bin" ] || return 1
    [ ! -e "$TEST_ROOT/versions" ]
}

@test "deployment root cannot be redirected" {
    run env HOME="$TEST_HOME" GREMVM_ROOT="$TEST_HOME/Documents" "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"GREMVM_ROOT is fixed"* ]]
}

@test "runtime path uses the signed Tart app executable" {
    run grep -F 'runtime/current/tart.app/Contents/MacOS/tart' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "Cloudflare setup owns one tunnel DNS record and Access policy" {
    run grep -F '/cfd_tunnel' "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 0 ] || return 1
    run grep -F '/dns_records' "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 0 ] || return 1
    run grep -F '/access/apps' "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 0 ] || return 1
    run grep -F 'gremvm.eviljuliette.com' "$REPO_ROOT/scripts/cloudflare-setup.sh" "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    run grep -F 'GREMVM_CLOUDFLARE_ACCESS_EMAIL' "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 0 ]
}

@test "supervisor routes only SSH to the private Tart guest and stops tunnel first" {
    run grep -F 'find_ssh 30' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    run grep -F '"service":"ssh://%s"' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    run grep -F '"service":"http_status:404"' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    run grep -F '"$CLOUDFLARED" tunnel' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    tunnel_kill=$(grep -nF '/bin/kill "$tunnel_child"' "$REPO_ROOT/bin/gremvm" | head -1 | cut -d: -f1)
    clean_stop=$(grep -nF 'clean_stop || force_stop' "$REPO_ROOT/bin/gremvm" | head -1 | cut -d: -f1)
    [ -n "$tunnel_kill" ] && [ -n "$clean_stop" ] && [ "$tunnel_kill" -lt "$clean_stop" ]
}

@test "guest bootstrap enables Remote Login without broadening the shutdown key" {
    run grep -F '/usr/sbin/systemsetup -setremotelogin on' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ] || return 1
    run grep -F 'restrict,command=\"sudo -n /usr/local/libexec/gremvm-shutdown\"' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "WebRTC browser and TURN implementation is absent" {
    [ ! -e "$REPO_ROOT/remote" ] || [ -z "$(find "$REPO_ROOT/remote" -type f -print)" ] || return 1
    [ ! -e "$REPO_ROOT/versions/remote.env" ] || return 1
    [ ! -e "$REPO_ROOT/scripts/remote-url.sh" ] || return 1
    run grep -E -i 'webrtc|turn_keys|novnc|pion|durable object|wrangler|tailscale' \
        "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/flake.nix" "$REPO_ROOT"/scripts/*.sh
    [ "$status" -eq 1 ] || return 1
    [ -z "$output" ] || return 1
    run grep -E 'gremvm-webrtc|CLOUDFLARE_TURN_KEY|remote/(agent|worker)' \
        "$REPO_ROOT/README.md" "$REPO_ROOT/docs/DECISION.md" "$REPO_ROOT/docs/REMOTE_ACCESS.md" "$REPO_ROOT/secrets/README.md"
    [ "$status" -eq 1 ]
}

@test "Keytap is the sole configured age recipient" {
    found=false
    for secret in "$REPO_ROOT"/secrets/*.age; do
        [ -f "$secret" ] || continue
        found=true
        run grep -a -c '^-> ' "$secret"
        [ "$status" -eq 0 ] || return 1
        [ "$output" -eq 1 ] || return 1
    done
    [ "$found" = true ] || return 1

    run grep -F 'keytap encrypt keytap >' "$REPO_ROOT/scripts/import-cloudflare-api-token.sh" "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 0 ] || return 1
    run grep -E 'keytap[[:space:]]+encrypt.*(--to|-R)' "$REPO_ROOT/scripts/import-cloudflare-api-token.sh" "$REPO_ROOT/scripts/cloudflare-setup.sh"
    [ "$status" -eq 1 ]
}

@test "tunnel credential install is private idempotent and canonical" {
    fake_repo="$BATS_TEST_TMPDIR/fake-repo"
    mkdir -p "$fake_repo/scripts" "$fake_repo/secrets"
    cp "$REPO_ROOT/scripts/cloudflare-install-host.sh" "$fake_repo/scripts/"
    printf '%s\n' 'age-encryption.org/v1' '-> X25519 fake' > "$fake_repo/secrets/CLOUDFLARE_TUNNEL_CREDENTIALS.age"

    run env HOME="$TEST_HOME" PATH="$TEST_BIN:$PATH" FAKE_KEYTAP_VALUE="$TEST_CREDENTIALS" \
        "$fake_repo/scripts/cloudflare-install-host.sh"
    [ "$status" -eq 0 ] || return 1
    [[ "$output" != *"$TEST_SECRET"* ]] || return 1
    destination="$TEST_ROOT/cloudflare/credentials.json"
    [ -f "$destination" ] || return 1
    [ "$(jq -r '.TunnelID' "$destination")" = "12345678-1234-1234-1234-123456789abc" ] || return 1
    if stat -f %Lp "$destination" > /dev/null 2>&1; then
        file_mode=$(stat -f %Lp "$destination")
    else
        file_mode=$(stat -c %a "$destination")
    fi
    [ "$file_mode" = 600 ] || return 1

    before=$(shasum -a 256 "$destination" | awk '{print $1}')
    run env HOME="$TEST_HOME" PATH="$TEST_BIN:$PATH" FAKE_KEYTAP_VALUE="$TEST_CREDENTIALS" \
        "$fake_repo/scripts/cloudflare-install-host.sh"
    [ "$status" -eq 0 ] || return 1
    after=$(shasum -a 256 "$destination" | awk '{print $1}')
    [ "$before" = "$after" ]
}

@test "tunnel credential install rejects malformed material" {
    fake_repo="$BATS_TEST_TMPDIR/fake-repo"
    mkdir -p "$fake_repo/scripts" "$fake_repo/secrets"
    cp "$REPO_ROOT/scripts/cloudflare-install-host.sh" "$fake_repo/scripts/"
    printf '%s\n' 'age-encryption.org/v1' '-> X25519 fake' > "$fake_repo/secrets/CLOUDFLARE_TUNNEL_CREDENTIALS.age"
    malformed='{"AccountTag":"bad","TunnelID":"not-a-uuid","TunnelName":"gremvm","TunnelSecret":"bad"}'
    run env HOME="$TEST_HOME" PATH="$TEST_BIN:$PATH" FAKE_KEYTAP_VALUE="$malformed" \
        "$fake_repo/scripts/cloudflare-install-host.sh"
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"invalid tunnel credentials"* ]]
}

@test "kernel lifecycle lock rejects a concurrent mutation" {
    if [ ! -x /usr/bin/lockf ]; then
        skip "Darwin Nix build sandbox hides lockf"
    fi
    mkdir -p "$TEST_ROOT/state"
    (
        exec 9>> "$TEST_ROOT/state/maintenance.lock"
        /usr/bin/lockf -s -t 0 9 || exit 1
        touch "$TEST_ROOT/state/test-lock-ready"
        sleep 10
    ) > /dev/null 2>&1 &
    LOCK_HOLDER=$!
    attempts=0
    while [ ! -e "$TEST_ROOT/state/test-lock-ready" ] && [ "$attempts" -lt 50 ]; do
        sleep 0.05
        attempts=$((attempts + 1))
    done
    [ -e "$TEST_ROOT/state/test-lock-ready" ] || return 1

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" uninstall
    kill "$LOCK_HOLDER" 2> /dev/null || true
    wait "$LOCK_HOLDER" 2> /dev/null || true
    LOCK_HOLDER=
    [ "$status" -ne 0 ] || return 1
    [[ "$output" == *"another lifecycle or backup operation is active"* ]]
}
