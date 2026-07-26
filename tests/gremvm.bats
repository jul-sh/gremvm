#!/usr/bin/env bats

setup() {
    export REPO_ROOT="$BATS_TEST_DIRNAME/.."
    export TEST_HOME="$BATS_TEST_TMPDIR/home"
    mkdir -p "$TEST_HOME"
}

@test "help exposes only the lifecycle surface" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"install"* ]]
    [[ "$output" == *"provision"* ]]
    [[ "$output" == *"start | stop | restart"* ]]
    [[ "$output" == *"logs"* ]]
    [[ "$output" == *"uninstall"* ]]
    [[ "$output" != *"backup"* ]]
    [[ "$output" != *"console"* ]]
    [[ "$output" != *"sip-off"* ]]
    [[ "$output" != *"firewall-check"* ]]
    [[ "$output" != *"runtime-path"* ]]
    [[ "$output" != *"acknowledge-hardening"* ]]
}

@test "status is not-installed in an empty home" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "unknown commands fail closed" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" destroy-everything
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown command"* ]]
}

@test "release pin is exact and contains only fields used by the wrapper" {
    run sh -c '. "$1"; printf "%s %s %s" "$LUME_VERSION" "$LUME_TEAM_ID" "$LUME_ARCHIVE_SHA256"' sh "$REPO_ROOT/versions/lume.env"
    [ "$status" -eq 0 ]
    [ "$output" = "0.4.0 YCK386LBJ7 8b44bbcc5ae9693f4b1343fea58aadddd37053fa990cd234e703c8c9e73b1cba" ]
    run grep -E '^LUME_(RELEASE_COMMIT|ARCHIVE_NIX_SHA256)=' "$REPO_ROOT/versions/lume.env"
    [ "$status" -ne 0 ]
}

@test "no personal signing material remains in the deployment repo" {
    run find "$REPO_ROOT" -path "$REPO_ROOT/.git" -prune -o -type f \( -name '*.p8' -o -name '*.p12' -o -name '*.age.b64' \) -print
    [ "$status" -eq 0 ]
    [ -z "$output" ]
}

@test "installed-style symlink resolves the repository release pin" {
    mkdir -p "$BATS_TEST_TMPDIR/bin"
    ln -s "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR/bin/gremvm"
    run env HOME="$TEST_HOME" "$BATS_TEST_TMPDIR/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "legacy configuration and environment values cannot alter the fixed VM" {
    legacy="$TEST_HOME/Library/Application Support/GremVM/config"
    mkdir -p "$legacy"
    printf 'GREMVM_VM_NAME=other\nGREMVM_VM_STORAGE=/tmp/other\nGREMVM_CPU_COUNT=1\n' > "$legacy/gremvm.env"
    run env HOME="$TEST_HOME" GREMVM_ROOT=/tmp/other GREMVM_VM_NAME=other GREMVM_VM_STORAGE=/tmp/other GREMVM_CPU_COUNT=1 GREMVM_MEMORY=1GB GREMVM_DISK_SIZE=1GB GREMVM_DISPLAY=1x1 GREMVM_IPSW=/tmp/other.ipsw GREMVM_GUEST_ADMIN_USER=other GREMVM_BACKUP_DESTINATION=/tmp/other "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "public commands reject options and removed commands" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" logs --follow
    [ "$status" -ne 0 ]
    [[ "$output" == *"usage: gremvm logs"* ]]

    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" backup
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown command: backup"* ]]
}

@test "uninstall preserves the fixed VM path" {
    root="$TEST_HOME/Library/Application Support/GremVM"
    mkdir -p "$root/vms/work"
    printf 'preserve\n' > "$root/vms/work/sentinel"
    run env HOME="$TEST_HOME" GREMVM_VM_STORAGE=/tmp/other GREMVM_VM_NAME=other "$REPO_ROOT/bin/gremvm" uninstall
    [ "$status" -eq 0 ]
    [ -f "$root/vms/work/sentinel" ]
}

@test "guest monitor requests restart after sustained unavailability" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        monitor_marker=$2/healthy
        monitor_count=$2/count
        printf "0\n" > "$monitor_count"
        vm_field() {
            count=$(/bin/cat "$monitor_count")
            count=$((count + 1))
            printf "%s\n" "$count" > "$monitor_count"
            if [ -e "$monitor_marker" ]; then
                printf "false\n"
            else
                : > "$monitor_marker"
                printf "true\n"
            fi
        }
        monitor_pause() { :; }
        runner_active() { kill -0 "$1" 2>/dev/null; }
        /bin/sleep 60 &
        monitored_pid=$!
        set +e
        monitor_guest "$monitored_pid"
        result=$?
        kill "$monitored_pid" 2>/dev/null || true
        wait "$monitored_pid" 2>/dev/null || true
        exit "$result"
    ' sh "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR"
    [ "$status" -eq 75 ]
    [ "$(/bin/cat "$BATS_TEST_TMPDIR/count")" -eq 46 ]
}

@test "stuck Lume runner is killed after bounded TERM grace" {
    run env HOME="$TEST_HOME" sh -c '
        . "$1" --help >/dev/null
        monitor_pause() { :; }
        runner_active() { kill -0 "$1" 2>/dev/null; }
        runner_marker=$2/runner-ready
        /bin/sh -c '\''trap "" TERM; : > "$1"; while :; do :; done'\'' sh "$runner_marker" &
        RUNNER_PID=$!
        while [ ! -e "$runner_marker" ]; do
            /bin/sleep 0.01
        done
        stop_supervised_runner
        [ -z "$RUNNER_PID" ]
    ' sh "$REPO_ROOT/bin/gremvm" "$BATS_TEST_TMPDIR"
    [ "$status" -eq 0 ]
}
