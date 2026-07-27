#!/usr/bin/env bats

setup() {
    export REPO_ROOT="$BATS_TEST_DIRNAME/.."
    export TEST_HOME="$BATS_TEST_TMPDIR/home"
    export TEST_ROOT="$TEST_HOME/Library/Application Support/GremVM"
    mkdir -p "$TEST_HOME"
}

@test "help exposes only the Tart lifecycle and LAN access surface" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" --help
    [ "$status" -eq 0 ]
    for command in install provision status start stop restart address ssh screen bridge logs uninstall; do
        [[ "$output" == *"$command"* ]] || return 1
    done
    [[ "$output" != *"backup"* ]]
    [[ "$output" != *"sip"* ]]
    [[ "$output" != *"firewall"* ]]
    [[ "$output" != *"console"* ]]
}

@test "status is not-installed in an empty home" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "unknown commands and public options fail closed" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" destroy-everything
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown command"* ]]

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" logs --follow
    [ "$status" -ne 0 ]
    [[ "$output" == *"usage: gremvm logs"* ]]

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" backup
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown command: backup"* ]]
}

@test "Tart release and guest image are exact content pins" {
    run sh -c '. "$1"; printf "%s|%s|%s|%s|%s|%s" "$TART_VERSION" "$TART_ARCHIVE_URL" "$TART_ARCHIVE_NIX_SHA256" "$TART_BUNDLE_ID" "$TART_TEAM_ID" "$TART_VM_IMAGE"' sh "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
    [ "$output" = "2.34.0|https://github.com/openai/tart/releases/download/2.34.0/tart.tar.gz|sha256-yfFgn0lFJY7w7id91E3JcA1vBpeJoR5Dvn81sKZLMTU=|com.github.cirruslabs.tart|9M2P8L4D89|ghcr.io/cirruslabs/macos-tahoe-vanilla@sha256:e12d678b248f3122e276fa64632970a8e1c6dc60ff6738d21fe9bfa5ea58f426" ]
    [[ "$output" != *":latest"* ]]
}

@test "the implementation uses the pinned Tart backend" {
    run grep -F 'TART_HOME=$ROOT/tart' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
    run grep -F 'VERSIONS_FILE=$REPO_ROOT/versions/tart.env' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
    run grep -F 'TART_VM_IMAGE=' "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
}

@test "Nix supplies the complete pinned Tart app to install and development" {
    run grep -F 'GREMVM_BUNDLED_TART_APP=${tart}/Applications/tart.app' "$REPO_ROOT/flake.nix"
    [ "$status" -eq 0 ]
    run grep -F 'GREMVM_BUNDLED_TART_LICENSE=${tart}/share/tart/LICENSE' "$REPO_ROOT/flake.nix"
    [ "$status" -eq 0 ]
    run grep -F 'cp -R tart.app "$out/Applications/tart.app"' "$REPO_ROOT/flake.nix"
    [ "$status" -eq 0 ]
    run grep -F 'makeWrapper "$out/Applications/tart.app/Contents/MacOS/tart"' "$REPO_ROOT/flake.nix"
    [ "$status" -eq 1 ]
}

@test "installed-style symlink resolves the repository Tart pin" {
    mkdir -p "$BATS_TEST_TMPDIR/bin"
    ln -s "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR/bin/gremvm"
    run env HOME="$TEST_HOME" "$BATS_TEST_TMPDIR/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "legacy configuration cannot redirect the fixed Tart VM" {
    run env HOME="$TEST_HOME" GREMVM_ROOT=/tmp/other GREMVM_VM_NAME=other GREMVM_VM_STORAGE=/tmp/other GREMVM_IPSW=/tmp/other.ipsw "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]

    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        [ "$VM_NAME" = work ]
        [ "$TART_HOME" = "$HOME/Library/Application Support/GremVM/tart" ]
        [ "$VM_DIR" = "$TART_HOME/vms/work" ]
    ' sh "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "Tart lifecycle is an exactly-one token directory" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        mkdir -p "$STATE_DIR"
        initialize_provision_state cloning
        [ "$(read_provision_variant)" = cloning ]
        [ ! -s "$LIFECYCLE_DIR/cloning" ]
        set -- "$LIFECYCLE_DIR"/*
        [ "$#" -eq 1 ]

        transition_provision_state cloning randomizing-mac
        [ "$(read_provision_variant)" = randomizing-mac ]
        [ ! -e "$LIFECYCLE_DIR/cloning" ]
        [ ! -s "$LIFECYCLE_DIR/randomizing-mac" ]

        if (transition_provision_state cloning ready) >/dev/null 2>&1; then
            exit 1
        fi
        [ "$(read_provision_variant)" = randomizing-mac ]

        : > "$LIFECYCLE_DIR/ready"
        if (read_provision_variant) >/dev/null 2>&1; then
            exit 1
        fi
    ' sh "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "ambiguous generic state never adopts an existing Tart VM" {
    run env HOME="$TEST_HOME" sh -c '
        release_file=$2
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        mkdir -p "$STATE_DIR"
        printf "ready\n" > "$STATE_DIR/provision-state"
        mkdir -p "$(dirname "$TART_BIN")"
        : > "$TART_BIN"
        chmod 755 "$TART_BIN"
        vm_exists() { return 0; }
        vm_path_exists() { return 0; }

        [ "$(provision_state)" = foreign-vm ]
        if (provision_command) >"$STATE_DIR/adoption.out" 2>"$STATE_DIR/adoption.err"; then
            exit 1
        fi
        grep -F "refusing automatic adoption" "$STATE_DIR/adoption.err" >/dev/null
        [ ! -e "$LIFECYCLE_DIR" ]
        [ "$(cat "$STATE_DIR/provision-state")" = ready ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
}

@test "provision clones, randomizes, hardens on private NAT, confirms, then becomes ready once" {
    run env HOME="$TEST_HOME" sh -c '
        release_file=$2
        call_log=$3
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        load_release
        mkdir -p "$(dirname "$TART_BIN")" "$STATE_DIR"
        : > "$TART_BIN"
        chmod 755 "$TART_BIN"
        fake_vm=$STATE_DIR/fake-vm
        bridge_interface() { printf "en7\n"; }
        hardening_terminal_available() { return 0; }
        confirm_guest_hardened() { printf "confirm\n" >> "$call_log"; }
        start_command() { printf "start\n" >> "$call_log"; }
        tart() {
            case $1 in
                get)
                    [ -e "$fake_vm" ] || return 1
                    printf "{\"State\":\"stopped\"}\n"
                    ;;
                clone)
                    printf "tart:%s\n" "$*" >> "$call_log"
                    mkdir -p "$VM_DIR"
                    : > "$fake_vm"
                    ;;
                set | run) printf "tart:%s\n" "$*" >> "$call_log" ;;
                *) return 64 ;;
            esac
        }
        run_tart_vm() { tart run "$@"; }
        provision_command >/dev/null
        provision_command >/dev/null
        [ "$(grep -Fxc "tart:clone $TART_VM_IMAGE work" "$call_log")" -eq 1 ]
        [ "$(grep -Fxc "tart:set work --random-mac" "$call_log")" -eq 1 ]
        [ "$(grep -Fxc "tart:run --no-audio work" "$call_log")" -eq 1 ]
        [ "$(grep -Fxc confirm "$call_log")" -eq 1 ]
        [ "$(grep -Fxc start "$call_log")" -eq 2 ]
        ! grep -F -- "--net-bridged" "$call_log" >/dev/null
        [ "$(read_provision_variant)" = ready ]
        set -- "$LIFECYCLE_DIR"/*
        [ "$#" -eq 1 ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env" "$BATS_TEST_TMPDIR/provision-calls"
    [ "$status" -eq 0 ]
}

@test "bridge accepts only an available physical enN and failed updates preserve the prior value" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        mkdir -p "$STATE_DIR"
        bridge_interface_available() { [ "$1" = en7 ]; }
        write_bridge_interface en7
        [ "$(bridge_interface)" = en7 ]

        if (write_bridge_interface utun0) >/dev/null 2>&1; then
            exit 1
        fi
        [ "$(cat "$BRIDGE_FILE")" = en7 ]

        if (write_bridge_interface en8) >/dev/null 2>&1; then
            exit 1
        fi
        [ "$(cat "$BRIDGE_FILE")" = en7 ]
        [ "$(bridge_interface)" = en7 ]
    ' sh "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "bridge rolls back when launchd accepts a restart but Tart never reaches running" {
    run env HOME="$TEST_HOME" sh -c '
        release_file=$2
        rollback_log=$3
        unloaded=$4
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        mkdir -p "$(dirname "$TART_BIN")" "$STATE_DIR"
        : > "$TART_BIN"
        chmod 755 "$TART_BIN"
        bridge_interface_available() { return 0; }
        persist_bridge_interface en7
        service_loaded() { [ ! -e "$unloaded" ]; }
        restart_command() { printf "restart\n" >> "$rollback_log"; }
        wait_for_vm_running() { return 1; }
        bootout_current_service() {
            printf "bootout\n" >> "$rollback_log"
            : > "$unloaded"
        }
        start_command() { printf "restore-start\n" >> "$rollback_log"; }

        if (bridge_command en8) >"$STATE_DIR/bridge.out" 2>"$STATE_DIR/bridge.err"; then
            exit 1
        fi
        [ "$(configured_bridge_value)" = en7 ]
        grep -F "restored the previous bridge configuration" "$STATE_DIR/bridge.err" >/dev/null
        [ "$(grep -Fxc restart "$rollback_log")" -eq 1 ]
        [ "$(grep -Fxc bootout "$rollback_log")" -eq 1 ]
        [ "$(grep -Fxc restore-start "$rollback_log")" -eq 1 ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env" "$BATS_TEST_TMPDIR/rollback-log" "$BATS_TEST_TMPDIR/unloaded"
    [ "$status" -eq 0 ]
}

@test "login agent is Aqua-scoped and watches only the ready lifecycle token" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        prepare_runtime_paths
        write_launch_agent
    ' sh "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]

    plist="$TEST_HOME/Library/LaunchAgents/io.gremvm.tart.plist"
    [ "$(/usr/bin/plutil -extract Label raw -o - "$plist")" = io.gremvm.tart ]
    [ "$(/usr/bin/plutil -extract LimitLoadToSessionType raw -o - "$plist")" = Aqua ]
    [ "$(/usr/bin/plutil -extract ProgramArguments.1 raw -o - "$plist")" = internal-supervise ]
    [ "$(/usr/bin/plutil -extract WorkingDirectory raw -o - "$plist")" = "$TEST_ROOT/tart" ]
    run grep -F "$TEST_ROOT/state/tart-lifecycle/ready" "$plist"
    [ "$status" -eq 0 ]
    run grep -F '<key>KeepAlive</key>' "$plist"
    [ "$status" -eq 0 ]
    run grep -F '<key>ProcessType</key>' "$plist"
    [ "$status" -eq 1 ]
    run grep -F '<string>Background</string>' "$plist"
    [ "$status" -eq 1 ]
}

@test "a restored managed VM gets a new MAC and private hardening before ready" {
    run env HOME="$TEST_HOME" sh -c '
        release_file=$2
        call_log=$3
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        mkdir -p "$(dirname "$TART_BIN")" "$LIFECYCLE_DIR"
        : > "$TART_BIN"
        chmod 755 "$TART_BIN"
        : > "$LIFECYCLE_DIR/managed-vm-missing"
        tart() {
            case $1 in
                get) printf "{\"State\":\"stopped\"}\n" ;;
                set) printf "tart:%s\n" "$*" >> "$call_log" ;;
                *) return 64 ;;
            esac
        }
        open_guest_hardening_console() { printf "private-hardening\n" >> "$call_log"; }
        confirm_guest_hardened() { printf "confirm\n" >> "$call_log"; }
        bridge_interface() { printf "en7\n"; }
        start_command() { printf "start\n" >> "$call_log"; }
        provision_command >/dev/null
        [ "$(grep -Fxc "tart:set work --random-mac" "$call_log")" -eq 1 ]
        [ "$(grep -Fxc private-hardening "$call_log")" -eq 1 ]
        [ "$(grep -Fxc confirm "$call_log")" -eq 1 ]
        [ "$(grep -Fxc start "$call_log")" -eq 1 ]
        [ "$(read_provision_variant)" = ready ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env" "$BATS_TEST_TMPDIR/restored-calls"
    [ "$status" -eq 0 ]
}

@test "supervisor runs the fixed VM headlessly on the selected bridge" {
    fake_tart="$TEST_ROOT/runtime/current/tart.app/Contents/MacOS/tart"
    fake_state="$BATS_TEST_TMPDIR/supervisor-tart-state"
    control_socket="$TEST_ROOT/tart/vms/work/control.sock"
    mkdir -p "$(dirname "$fake_tart")" "$TEST_ROOT/state" "$TEST_ROOT/tart/vms/work"
    printf 'stopped\n' > "$fake_state"
    (CDPATH='' cd -- "$TEST_ROOT/tart/vms/work" && /usr/bin/ruby -rsocket -e 'UNIXServer.new(ARGV.fetch(0)).close' control.sock)
    [ -S "$control_socket" ]
    cat > "$fake_tart" << 'EOF'
#!/bin/sh
if [ "$1" = get ]; then
    printf '{"State":"%s"}\n' "$(/bin/cat "$FAKE_TART_STATE")"
    exit 0
fi
printf 'running\n' > "$FAKE_TART_STATE"
printf '%s|%s|%s\n' "$TART_HOME" "$(/bin/pwd -P)" "$*" > "$FAKE_TART_LOG"
exit 0
EOF
    chmod 755 "$fake_tart"
    mkdir -p "$TEST_ROOT/state/tart-lifecycle"
    : > "$TEST_ROOT/state/tart-lifecycle/ready"

    run env HOME="$TEST_HOME" FAKE_TART_LOG="$BATS_TEST_TMPDIR/tart-args" FAKE_TART_STATE="$fake_state" sh -c '
        release_file=$2
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        bridge_interface() { printf "en7\n"; }
        internal_supervise
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
    expected_vm_dir=$(CDPATH='' cd -- "$TEST_ROOT/tart/vms/work" && /bin/pwd -P)
    [ "$(cat "$BATS_TEST_TMPDIR/tart-args")" = "$TEST_ROOT/tart|$expected_vm_dir|run --no-graphics --no-audio --net-bridged=en7 work" ]
    [ ! -e "$control_socket" ]
}

@test "an early Tart start failure removes ready from launchd retry eligibility" {
    fake_tart="$TEST_ROOT/runtime/current/tart.app/Contents/MacOS/tart"
    mkdir -p "$(dirname "$fake_tart")" "$TEST_ROOT/state/tart-lifecycle" "$TEST_ROOT/tart/vms/work"
    : > "$TEST_ROOT/state/tart-lifecycle/ready"
    cat > "$fake_tart" << 'EOF'
#!/bin/sh
case $1 in
    get) printf '{"State":"stopped"}\n' ;;
    run) exit 42 ;;
    *) exit 64 ;;
esac
EOF
    chmod 755 "$fake_tart"

    run env HOME="$TEST_HOME" sh -c '
        release_file=$2
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        bridge_interface() { printf "en7\n"; }
        if (internal_supervise) >/dev/null 2>&1; then
            exit 1
        fi
        [ "$(read_provision_variant)" = start-failed ]
        [ ! -e "$READY_STATE" ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
}

@test "TERM asks Tart to stop the supervised VM before the runner exits" {
    fake_tart="$TEST_ROOT/runtime/current/tart.app/Contents/MacOS/tart"
    fake_state="$BATS_TEST_TMPDIR/tart-state"
    fake_runner_pid="$BATS_TEST_TMPDIR/tart-runner-pid"
    fake_calls="$BATS_TEST_TMPDIR/tart-signal-calls"
    mkdir -p "$(dirname "$fake_tart")" "$TEST_ROOT/state/tart-lifecycle" "$TEST_ROOT/tart/vms/work"
    printf 'stopped\n' > "$fake_state"
    : > "$TEST_ROOT/state/tart-lifecycle/ready"
    cat > "$fake_tart" << 'EOF'
#!/bin/sh
set -eu

case $1 in
    get)
        printf '{"State":"%s"}\n' "$(/bin/cat "$FAKE_TART_STATE")"
        ;;
    run)
        printf 'run|%s\n' "$*" >> "$FAKE_TART_CALLS"
        printf '%s\n' "$$" > "$FAKE_TART_RUNNER_PID"
        printf 'running\n' > "$FAKE_TART_STATE"
        trap 'printf "stopped\n" > "$FAKE_TART_STATE"; exit 0' HUP INT TERM
        while :; do
            /bin/sleep 1
        done
        ;;
    stop)
        printf 'stop|%s\n' "$*" >> "$FAKE_TART_CALLS"
        /bin/kill -TERM "$(/bin/cat "$FAKE_TART_RUNNER_PID")"
        ;;
    *) exit 64 ;;
esac
EOF
    chmod 755 "$fake_tart"

    run env HOME="$TEST_HOME" FAKE_TART_STATE="$fake_state" FAKE_TART_RUNNER_PID="$fake_runner_pid" FAKE_TART_CALLS="$fake_calls" sh -c '
        release_file=$2
        . "$1" --help >/dev/null
        load_release() { . "$release_file"; }
        bridge_interface() { printf "en7\n"; }

        internal_supervise &
        supervisor_pid=$!
        attempts=0
        while [ ! -s "$FAKE_TART_RUNNER_PID" ]; do
            attempts=$((attempts + 1))
            if [ "$attempts" -ge 200 ]; then
                /bin/kill -KILL "$supervisor_pid" 2>/dev/null || true
                exit 1
            fi
            /bin/sleep 0.01
        done

        (
            /bin/sleep 5
            /bin/kill -KILL "$supervisor_pid" 2>/dev/null || true
        ) &
        watchdog_pid=$!
        /bin/kill -TERM "$supervisor_pid"
        supervisor_status=0
        wait "$supervisor_pid" || supervisor_status=$?
        /bin/kill -TERM "$watchdog_pid" 2>/dev/null || true
        wait "$watchdog_pid" 2>/dev/null || true
        if [ -s "$FAKE_TART_RUNNER_PID" ]; then
            /bin/kill -TERM "$(/bin/cat "$FAKE_TART_RUNNER_PID")" 2>/dev/null || true
        fi
        [ "$supervisor_status" -eq 0 ]
    ' sh "$REPO_ROOT/bin/gremvm" "$REPO_ROOT/versions/tart.env"
    [ "$status" -eq 0 ]
    run grep -F 'stop|stop work --timeout 30' "$fake_calls"
    [ "$status" -eq 0 ]
    [ "$(cat "$fake_state")" = stopped ]
}

@test "a missing ready VM is quarantined into its own lifecycle variant" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        mkdir -p "$LIFECYCLE_DIR"
        : > "$READY_STATE"
        vm_exists() { return 1; }
        [ "$(provision_state)" = managed-vm-missing ]
        quarantine_missing_ready_state
        [ "$(read_provision_variant)" = managed-vm-missing ]
        [ ! -e "$READY_STATE" ]
        set -- "$LIFECYCLE_DIR"/*
        [ "$#" -eq 1 ]
    ' sh "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "bridged address discovery uses Tart's ARP resolver" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        capture=$2
        tart() {
            printf "%s\n" "$*" > "$capture"
            printf "192.0.2.10\n"
        }
        [ "$(resolve_ip 60)" = 192.0.2.10 ]
        [ "$(cat "$capture")" = "ip work --resolver=arp --wait=60" ]
    ' sh "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR/ip-args"
    [ "$status" -eq 0 ]
}

@test "LAN access checks both SSH and guest Screen Sharing" {
    run grep -F '/usr/bin/nc -z -G 5 "$guest_ip" 22' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
    run grep -F '/usr/bin/nc -z -G 5 "$guest_ip" 5900' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
    run grep -F 'screen-sharing: vnc://$GUEST_USER@$guest_ip' "$REPO_ROOT/bin/gremvm"
    [ "$status" -eq 0 ]
}

@test "uninstall preserves Tart VM data, state, and logs" {
    mkdir -p "$TEST_ROOT/tart/vms/work" "$TEST_ROOT/state" "$TEST_ROOT/logs" "$TEST_ROOT/runtime" "$TEST_ROOT/bin" "$TEST_ROOT/versions"
    printf 'preserve\n' > "$TEST_ROOT/tart/vms/work/sentinel"
    printf 'preserve\n' > "$TEST_ROOT/state/sentinel"
    printf 'preserve\n' > "$TEST_ROOT/logs/sentinel"
    printf 'remove\n' > "$TEST_ROOT/runtime/sentinel"

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" uninstall
    [ "$status" -eq 0 ]
    [ -f "$TEST_ROOT/tart/vms/work/sentinel" ]
    [ -f "$TEST_ROOT/state/sentinel" ]
    [ -f "$TEST_ROOT/logs/sentinel" ]
    [ ! -e "$TEST_ROOT/runtime" ]
    [ ! -e "$TEST_ROOT/bin" ]
    [ ! -e "$TEST_ROOT/versions" ]
}

@test "no personal signing material is tracked" {
    run find "$REPO_ROOT" -path "$REPO_ROOT/.git" -prune -o -type f \( -name '*.p8' -o -name '*.p12' -o -name '*.age.b64' -o -name '*.mobileprovision' \) -print
    [ "$status" -eq 0 ]
    [ -z "$output" ]
}
