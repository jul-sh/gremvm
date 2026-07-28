use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn app_root(home: &Path) -> PathBuf {
    home.join("Library/Application Support/GremVM")
}

fn write_config(home: &Path, json: &str) {
    let config = app_root(home).join("config/config.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(config, json).unwrap();
}

fn write_runtime(home: &Path, script: &str) {
    let lume = app_root(home).join("runtime/bin/lume");
    fs::create_dir_all(lume.parent().unwrap()).unwrap();
    fs::write(&lume, script).unwrap();
    fs::set_permissions(lume, fs::Permissions::from_mode(0o700)).unwrap();
}

fn mark_ready(home: &Path) {
    let marker = app_root(home).join("state/provisioned");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(marker, "").unwrap();

    let vm = home.join(".lume/gremvm");
    fs::create_dir_all(&vm).unwrap();
    for (name, contents) in [("config.json", "{}"), ("disk.img", ""), ("nvram.bin", "")] {
        fs::write(vm.join(name), contents).unwrap();
    }
    fs::write(vm.join(".provisioning"), "stale").unwrap();
}

#[test]
fn help_is_the_complete_public_interface() {
    let output = cargo_bin_cmd!("gremvm")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("persistent Lume macOS VM"))
        .stdout(predicate::str::contains(
            "The guest always uses bridged networking on en0.",
        ))
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(output).unwrap();

    for command in [
        "install",
        "provision",
        "status",
        "start",
        "stop",
        "restart",
        "ssh",
        "screen-share",
        "console",
        "logs",
        "uninstall",
    ] {
        assert!(
            help.contains(command),
            "missing command {command} in:\n{help}"
        );
    }
    assert!(!help.contains("internal-run"));
    assert!(!help.contains("internal-keychain"));

    cargo_bin_cmd!("gremvm")
        .args(["screen-share", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--url"))
        .stdout(predicate::str::contains(
            "Print the connection URL for use on another Mac",
        ));
    cargo_bin_cmd!("gremvm")
        .args(["start", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wait for its desktop"));
}

#[test]
fn install_defaults_are_visible_and_the_volume_is_optional() {
    cargo_bin_cmd!("gremvm")
        .args(["install", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Name of the Lume VM"))
        .stdout(predicate::str::contains("[default: gremvm]"))
        .stdout(predicate::str::contains("[default: 6]"))
        .stdout(predicate::str::contains("[default: 24]"))
        .stdout(predicate::str::contains("[default: 192]"))
        .stdout(predicate::str::contains("--volume-name <VOLUME_NAME>"))
        .stdout(
            predicate::str::contains("Encrypted APFS volume containing the VM [default:").not(),
        );
}

#[test]
fn install_rejects_unsafe_names_and_oversized_disks() {
    cargo_bin_cmd!("gremvm")
        .args(["install", "--vm-name", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("name must be"));
    cargo_bin_cmd!("gremvm")
        .args(["install", "--disk-gb", "351"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("351 is not in 50..=350"));
    cargo_bin_cmd!("gremvm")
        .args(["install", "--volume-name", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("name must be"));
}

#[test]
fn an_empty_home_is_not_installed() {
    let home = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout("state: not-installed\n");
}

#[test]
fn persisted_configuration_is_strict() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "not json\n");
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("start")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "persisted configuration is invalid",
        ));

    write_config(
        home.path(),
        r#"{"vm_name":"gremvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"},"surprise":true}"#,
    );
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("start")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "persisted configuration is invalid",
        ));
}

#[test]
fn status_accepts_complete_vm_and_uses_isolated_lume_configuration() {
    let home = tempfile::tempdir().unwrap();
    write_config(
        home.path(),
        r#"{"vm_name":"gremvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"}}"#,
    );
    mark_ready(home.path());
    write_runtime(
        home.path(),
        r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/lume-args"
printf '%s\n' "$XDG_CONFIG_HOME" > "$HOME/lume-config-home"
test "$LUME_TELEMETRY_ENABLED" = 0 || exit 65
test "$LUME_UPDATE_CHECK" = 0 || exit 66
test "$LUME_LOG_LEVEL" = error || exit 67
case "$1" in
  --version) printf '%s\n' '0.4.0' ;;
  get) printf '%s\n' '[{"name":"gremvm","os":"macOS","cpuCount":6,"memorySize":25769803776,"diskSize":{"allocated":1048576,"total":206158430208},"display":"1512x982","status":"stopped","vncUrl":null,"ipAddress":null,"sshAvailable":false,"locationName":"gremvm","networkMode":"bridged:en0"}]' ;;
  *) exit 64 ;;
esac
"#,
    );

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state: stopped"))
        .stdout(predicate::str::contains("cpu: 6"))
        .stdout(predicate::str::contains("memory-gb: 24"))
        .stdout(predicate::str::contains("disk-gb: 192"))
        .stdout(predicate::str::contains("display: 1512x982"))
        .stdout(predicate::str::contains("network: bridged:en0"))
        .stdout(predicate::str::contains(
            home.path().join(".lume").to_str().unwrap(),
        ));

    let args = fs::read_to_string(home.path().join("lume-args")).unwrap();
    for expected in [
        "get",
        "gremvm",
        "--format",
        "json",
        "--storage",
        home.path().join(".lume").to_str().unwrap(),
    ] {
        assert!(
            args.lines().any(|arg| arg == expected),
            "missing {expected:?} in {args:?}"
        );
    }

    let config_home = fs::read_to_string(home.path().join("lume-config-home")).unwrap();
    assert_eq!(
        config_home.trim(),
        app_root(home.path())
            .join("state/lume-config")
            .to_str()
            .unwrap()
    );
    let lume_config =
        fs::read_to_string(app_root(home.path()).join("state/lume-config/lume/config.yaml"))
            .unwrap();
    assert!(lume_config.contains(home.path().join(".lume").to_str().unwrap()));
}
