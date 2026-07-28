use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn help_describes_the_public_interface() {
    cargo_bin_cmd!("gremvm")
        .arg("install")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "--vm-name <VM_NAME>          Name of the Tart VM [default: gremvm]",
        ))
        .stdout(predicate::str::contains(
            "--cpu-count <CPU_COUNT>      Number of virtual CPUs [default: 6]",
        ))
        .stdout(predicate::str::contains(
            "--memory-gb <MEMORY_GB>      Guest memory in GiB [default: 24]",
        ))
        .stdout(predicate::str::contains(
            "--disk-gb <DISK_GB>          Virtual disk size in decimal GB [default: 192]",
        ))
        .stdout(predicate::str::contains(
            "--volume-name <VOLUME_NAME>  Encrypted APFS volume containing the VM",
        ))
        .stdout(
            predicate::str::contains("Encrypted APFS volume containing the VM [default:").not(),
        );

    cargo_bin_cmd!("gremvm")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The guest always uses bridged networking on en0.",
        ))
        .stdout(predicate::str::contains("internal-run").not())
        .stdout(predicate::str::contains("internal-keychain").not());
}

#[test]
fn install_rejects_invalid_settings() {
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
fn an_empty_home_reports_not_installed() {
    let home = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout("state: not-installed\n");
}

#[test]
fn management_commands_are_serialized() {
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("Library/Application Support/GremVM/state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join("management.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("install")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "another gremvm management command is running",
        ));
}

#[test]
fn keychain_helper_publishes_its_result() {
    let home = tempfile::tempdir().unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .args(["internal-keychain", "check"])
        .assert()
        .success();

    let result = home
        .path()
        .join("Library/Application Support/GremVM/state/keychain.result");
    assert_eq!(std::fs::read_to_string(result).unwrap(), "locked\n");
}

#[test]
fn keychain_prompt_hides_input_and_restores_the_terminal() {
    let home = tempfile::tempdir().unwrap();
    let password = "not-a-real-password";
    let keychain = home.path().join("Library/Keychains/login.keychain-db");
    std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
    assert!(
        Command::new("/usr/bin/security")
            .args(["create-keychain", "-p", password])
            .arg(&keychain)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("/usr/bin/security")
            .arg("lock-keychain")
            .arg(&keychain)
            .status()
            .unwrap()
            .success()
    );
    let script = format!(
        r#"set timeout 5
spawn -noecho /bin/zsh -f -c {{
    TRAPINT() {{ : }}
    before=$(stty -g)
    "$1" internal-keychain unlock || exit 95
    /usr/bin/security show-keychain-info "$2" || exit 93
    /usr/bin/security lock-keychain "$2" || exit 94
    "$1" internal-keychain unlock || exit 96
    [[ "$before" = "$(stty -g)" ]] || exit 92
}} zsh {{{}}} {{{}}}
expect {{
    "password to unlock" {{}}
    timeout {{ exit 90 }}
    eof {{ exit 91 }}
}}
send -- "{password}\r"
expect {{
    "password to unlock" {{}}
    timeout {{ exit 90 }}
    eof {{ exit 91 }}
}}
send -- "\003"
expect eof
catch wait result
exit [lindex $result 3]
"#,
        env!("CARGO_BIN_EXE_gremvm"),
        keychain.display()
    );
    let output = Command::new("/usr/bin/expect")
        .args(["-c", &script])
        .env("HOME", home.path())
        .output()
        .unwrap();
    let transcript = [output.stdout, output.stderr].concat();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&transcript)
    );
    assert!(
        !String::from_utf8_lossy(&transcript).contains(password),
        "{}",
        String::from_utf8_lossy(&transcript)
    );
    assert_eq!(
        std::fs::read_to_string(
            home.path()
                .join("Library/Application Support/GremVM/state/keychain.result")
        )
        .unwrap(),
        "locked\n"
    );
}

#[test]
fn malformed_configuration_has_a_clear_error() {
    let home = tempfile::tempdir().unwrap();
    let config = home
        .path()
        .join("Library/Application Support/GremVM/config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.json"), "not json\n").unwrap();

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("start")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "persisted configuration is invalid",
        ));

    std::fs::write(
        config.join("config.json"),
        r#"{"vm_name":"gremvm","cpu_count":6,"memory_gb":24,"disk_gb":192}"#,
    )
    .unwrap();
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
fn default_storage_uses_tarts_normal_home() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("Library/Application Support/GremVM");
    let config = root.join("config");
    let tart = root.join("runtime/bin/tart");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(tart.parent().unwrap()).unwrap();
    std::fs::write(
        config.join("config.json"),
        r#"{"vm_name":"gremvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"}}"#,
    )
    .unwrap();
    std::fs::write(&tart, "#!/bin/sh\n[ \"${TART_HOME+x}\" != x ]\n").unwrap();
    std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .env("TART_HOME", "/unexpected")
        .arg("status")
        .assert()
        .success()
        .stdout("state: not-provisioned\n");
}
