#!/usr/bin/env bats

setup() {
    export REPO_ROOT="$BATS_TEST_DIRNAME/.."
    export TEST_HOME="$BATS_TEST_TMPDIR/home"
    mkdir -p "$TEST_HOME"
}

@test "help exposes the required lifecycle commands" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"install"* ]]
    [[ "$output" == *"status"* ]]
    [[ "$output" == *"start | stop | restart"* ]]
    [[ "$output" == *"logs"* ]]
    [[ "$output" == *"uninstall"* ]]
}

@test "status is a typed not-installed state in an empty home" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" status
    [ "$status" -eq 0 ]
    [ "$output" = "state: not-installed" ]
}

@test "unknown commands fail closed" {
    run env HOME="$TEST_HOME" "$REPO_ROOT/bin/gremvm" destroy-everything
    [ "$status" -ne 0 ]
    [[ "$output" == *"unknown command"* ]]
}

@test "release pin is exact and complete" {
    run sh -c '. "$1"; printf "%s %s %s" "$LUME_VERSION" "$LUME_TEAM_ID" "$LUME_ARCHIVE_SHA256"' sh "$REPO_ROOT/versions/lume.env"
    [ "$status" -eq 0 ]
    [ "$output" = "0.4.0 YCK386LBJ7 8b44bbcc5ae9693f4b1343fea58aadddd37053fa990cd234e703c8c9e73b1cba" ]
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

@test "firewall policy typo fails closed" {
    run env HOME="$TEST_HOME" GREMVM_REQUIRE_APPLICATION_FIREWALL=treu "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ]
    [[ "$output" == *"must be true or false"* ]]
}

@test "unsafe VM names are structurally rejected" {
    run env HOME="$TEST_HOME" GREMVM_VM_NAME=.. "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ]
    [[ "$output" == *"must begin with an ASCII letter or digit"* ]]

    run env HOME="$TEST_HOME" GREMVM_VM_NAME=-work "$REPO_ROOT/bin/gremvm" status
    [ "$status" -ne 0 ]
    [[ "$output" == *"must begin with an ASCII letter or digit"* ]]
}

@test "uninstall refuses runtime overlap with VM data before deletion" {
    if [ -z "$(/bin/ps -p $$ -o lstart= 2> /dev/null)" ]; then
        skip "Darwin Nix build sandbox hides process start identity"
    fi
    root="$TEST_HOME/Library/Application Support/GremVM"
    mkdir -p "$root/runtime"
    printf 'preserve\n' > "$root/runtime/sentinel"
    run env HOME="$TEST_HOME" GREMVM_VM_STORAGE="$root" GREMVM_VM_NAME=runtime "$REPO_ROOT/bin/gremvm" uninstall
    [ "$status" -ne 0 ]
    if [[ "$output" != *"runtime removal overlaps VM data"* ]]; then
        printf 'unexpected output: %s\n' "$output" >&3
        false
    fi
    [ -f "$root/runtime/sentinel" ]
}
