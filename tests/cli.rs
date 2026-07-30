use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

fn command_alias(directory: &Path, name: &str) -> PathBuf {
    let alias = directory.join(name);
    symlink(env!("CARGO_BIN_EXE_gremvm"), &alias).unwrap();
    alias
}

fn instance_root(home: &Path, name: &str) -> PathBuf {
    home.join("Library/Application Support/GremVM/instances")
        .join(name)
}

#[test]
fn help_describes_the_public_interface() {
    cargo_bin_cmd!("gremvm-install")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: nix run . -- <COMMAND>"))
        .stdout(predicate::str::contains("\n  install"))
        .stdout(predicate::str::contains("\n  start").not())
        .stdout(predicate::str::contains("\n  status").not());

    cargo_bin_cmd!("gremvm-install")
        .arg("install")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "<NAME>  VM and command name to install",
        ))
        .stdout(predicate::str::contains(
            "--cpu-count <CPU_COUNT>    Number of virtual CPUs [default: 6]",
        ))
        .stdout(predicate::str::contains(
            "--memory-gb <MEMORY_GB>    Guest memory in GiB [default: 24]",
        ))
        .stdout(predicate::str::contains(
            "--disk-gb <DISK_GB>        Virtual disk size in decimal GB [default: 192]",
        ))
        .stdout(predicate::str::contains(
            "--guest-user <GUEST_USER>  Guest account short name [default: admin]",
        ))
        .stdout(predicate::str::contains(
            "--ask-password             Prompt for the initial guest password instead of generating one",
        ))
        .stdout(predicate::str::contains(
            "--storage <DIRECTORY>      Existing absolute directory containing the VM",
        ))
        .stdout(predicate::str::contains("--vm-name").not())
        .stdout(predicate::str::contains("--volume-name").not());

    cargo_bin_cmd!("gremvm")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The guest always uses bridged networking on en0.",
        ))
        .stdout(predicate::str::contains(
            "screen-share  Open the guest in macOS Screen Sharing",
        ))
        .stdout(predicate::str::contains(
            "console       Open Tart's local recovery console",
        ))
        .stdout(predicate::str::contains(
            "tailscale     Manage CLI-only Tailscale inside the guest",
        ))
        .stdout(predicate::str::contains("\n  install").not())
        .stdout(predicate::str::contains("\n  provision").not())
        .stdout(predicate::str::contains("\n  gui").not())
        .stdout(predicate::str::contains("internal-run").not())
        .stdout(predicate::str::contains("internal-keychain").not());

    cargo_bin_cmd!("gremvm")
        .arg("install")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'install'",
        ));

    let aliases = tempfile::tempdir().unwrap();
    Command::new(command_alias(aliases.path(), "foovm"))
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: foovm <COMMAND>"))
        .stdout(predicate::str::contains("\n  install").not());

    cargo_bin_cmd!("gremvm")
        .arg("provision")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'provision'",
        ));

    cargo_bin_cmd!("gremvm")
        .args(["tailscale", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "setup   Install, upgrade, and connect Tailscale",
        ))
        .stdout(predicate::str::contains(
            "status  Show the guest's Tailscale connection",
        ))
        .stdout(predicate::str::contains("auth-key").not());
}

#[test]
fn install_rejects_invalid_settings() {
    cargo_bin_cmd!("gremvm-install")
        .arg("install")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<NAME>"));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "../escape"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("name must be"));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--vm-name", "foovm"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--vm-name'"));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--disk-gb", "49"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("49 is not in 50.."));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--guest-user", "Admin"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("guest user must be"));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--guest-user", "root"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reserved account"));
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--storage", "relative/path"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "storage directory must be absolute",
        ));

    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing");
    cargo_bin_cmd!("gremvm-install")
        .args(["install", "gremvm", "--storage"])
        .arg(&missing)
        .assert()
        .failure()
        .stderr(predicate::str::contains("storage directory does not exist"));
}

#[test]
fn install_accepts_large_disks_and_explicit_storage() {
    let home = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let state = instance_root(home.path(), "gremvm").join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join("management.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    cargo_bin_cmd!("gremvm-install")
        .env("HOME", home.path())
        .args([
            "install",
            "gremvm",
            "--disk-gb",
            "5000000000",
            "--guest-user",
            "build_user",
            "--ask-password",
            "--storage",
        ])
        .arg(storage.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "another gremvm management command is running",
        ));
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
    let state = instance_root(home.path(), "gremvm").join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join("management.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    cargo_bin_cmd!("gremvm-install")
        .env("HOME", home.path())
        .args(["install", "gremvm"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "another gremvm management command is running",
        ));
}

#[test]
fn named_install_uses_its_own_management_lock() {
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let foovm = command_alias(aliases.path(), "foovm");
    let state = instance_root(home.path(), "foovm").join("state");
    std::fs::create_dir_all(&state).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(state.join("management.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    cargo_bin_cmd!("gremvm-install")
        .env("HOME", home.path())
        .args(["install", "foovm"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "another foovm management command is running",
        ));

    Command::new(foovm)
        .env("HOME", home.path())
        .arg("install")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'install'",
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

    let result = instance_root(home.path(), "gremvm").join("state/keychain.result");
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
        std::fs::read_to_string(instance_root(home.path(), "gremvm").join("state/keychain.result"))
            .unwrap(),
        "locked\n"
    );
}

#[test]
fn malformed_configuration_has_a_clear_error() {
    let home = tempfile::tempdir().unwrap();
    let config = instance_root(home.path(), "gremvm").join("config");
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
fn volume_configuration_is_strict_but_loads_the_legacy_shape() {
    let home = tempfile::tempdir().unwrap();
    let root = instance_root(home.path(), "gremvm");
    let config = root.join("config/config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::write(root.join("logs/vm.log"), "").unwrap();
    std::fs::write(root.join("logs/vm.error.log"), "").unwrap();
    let base = serde_json::json!({
        "vm_name": "gremvm",
        "cpu_count": 6,
        "memory_gb": 24,
        "disk_gb": 192,
        "storage": {
            "kind": "volume",
            "name": "GremVM",
            "uuid": "9DAE1DD8-1A6C-4D59-BB60-018B31AB722B",
        },
    });
    std::fs::write(&config, base.to_string()).unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("logs")
        .assert()
        .success();

    let mut current = base;
    current["storage"] = serde_json::json!({
        "kind": "plain-volume",
        "path": "/Volumes/My Work/gremvm",
        "mount_point": "/Volumes/My Work",
        "name": "My Work",
        "uuid": "9DAE1DD8-1A6C-4D59-BB60-018B31AB722B",
    });
    std::fs::write(&config, current.to_string()).unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("logs")
        .assert()
        .success();

    current["storage"]
        .as_object_mut()
        .unwrap()
        .remove("mount_point");
    std::fs::write(config, current.to_string()).unwrap();
    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("logs")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "persisted configuration is invalid",
        ));
}

#[test]
fn default_storage_uses_tarts_normal_home() {
    let home = tempfile::tempdir().unwrap();
    let root = instance_root(home.path(), "gremvm");
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
        .stdout("state: incomplete\n");

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("screen-share")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "VM does not exist; rerun 'nix run github:jul-sh/gremvm -- install gremvm'",
        ));
}

#[test]
fn explicit_storage_uses_the_exact_directory() {
    let home = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let storage = storage.path().canonicalize().unwrap();
    let root = instance_root(home.path(), "gremvm");
    let config = root.join("config");
    let tart = root.join("runtime/bin/tart");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(tart.parent().unwrap()).unwrap();
    std::fs::write(
        config.join("config.json"),
        serde_json::json!({
            "vm_name": "gremvm",
            "guest_user": "build_user",
            "cpu_count": 6,
            "memory_gb": 24,
            "disk_gb": 5_000_000_000_u64,
            "storage": {
                "kind": "directory",
                "path": &storage,
            },
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &tart,
        "#!/bin/sh\n[ \"$TART_HOME\" = \"$EXPECTED_TART_HOME\" ] || exit 42\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .env("EXPECTED_TART_HOME", &storage)
        .env("TART_HOME", "/unexpected")
        .arg("status")
        .assert()
        .success()
        .stdout("state: incomplete\n");
}

#[test]
fn command_names_select_isolated_vms() {
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let foovm = command_alias(aliases.path(), "foovm");
    let gremvm_root = instance_root(home.path(), "gremvm");
    let foovm_root = instance_root(home.path(), "foovm");

    for (root, name, cpu, disk, run) in [
        (gremvm_root.clone(), "gremvm", 6, 192, false),
        (foovm_root.clone(), "foovm", 4, 96, true),
    ] {
        let config = root.join("config");
        let state = root.join("state");
        let tart = root.join("runtime/bin/tart");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(tart.parent().unwrap()).unwrap();
        std::fs::write(
            config.join("config.json"),
            serde_json::json!({
                "vm_name": name,
                "cpu_count": cpu,
                "memory_gb": 8,
                "disk_gb": disk,
                "storage": { "kind": "default" },
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(state.join("provisioned"), "").unwrap();
        if run {
            std::fs::write(state.join("run"), "").unwrap();
        }
        std::fs::write(
            &tart,
            format!(
                "#!/bin/sh\ncase \"$1\" in\nlist) echo {name};;\nget) echo '{{\"State\":\"stopped\",\"CPU\":{cpu},\"Memory\":8192,\"Disk\":{disk},\"OS\":\"darwin\"}}';;\n*) exit 1;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state: stopped\n"))
        .stdout(predicate::str::contains("name: gremvm\n"))
        .stdout(predicate::str::contains("cpu: 6\n"))
        .stdout(predicate::str::contains("disk-gb: 192\n"));

    Command::new(&foovm)
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state: stopped\n"))
        .stdout(predicate::str::contains("name: foovm\n"))
        .stdout(predicate::str::contains("cpu: 4\n"))
        .stdout(predicate::str::contains("disk-gb: 96\n"));

    Command::new(&foovm)
        .env("HOME", home.path())
        .arg("screen-share")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "VM is not running; run 'foovm start'",
        ));

    Command::new(&foovm)
        .env("HOME", home.path())
        .arg("internal-keychain")
        .arg("check")
        .assert()
        .success();
    assert!(foovm_root.join("state/keychain.result").is_file());
    assert!(!gremvm_root.join("state/keychain.result").exists());
}

#[test]
fn uninstall_deletes_only_the_selected_vm_data() {
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let bundle = tempfile::tempdir().unwrap();
    let name = format!("testvm{}", std::process::id());
    let command = command_alias(aliases.path(), &name);
    let root = instance_root(home.path(), &name);
    let config = root.join("config");
    let state = root.join("state");
    let logs = root.join("logs");
    let runtime = root.join("runtime");
    let bundle_bin = bundle.path().join("bin");
    let command_link = home.path().join(".local/bin").join(&name);
    let autoload_agent = home
        .path()
        .join("Library/LaunchAgents")
        .join(format!("io.gremvm.tart.{name}.plist"));
    let vm_state = home.path().join("fake-vm-exists");
    let trace = home.path().join("tart.trace");
    let unrelated = home.path().join(".tart/vms/unrelated/data");

    for directory in [
        &config,
        &state,
        &logs,
        &bundle_bin,
        command_link.parent().unwrap(),
        autoload_agent.parent().unwrap(),
        unrelated.parent().unwrap(),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::write(
        config.join("config.json"),
        serde_json::json!({
            "vm_name": name,
            "guest_user": "admin",
            "cpu_count": 6,
            "memory_gb": 24,
            "disk_gb": 192,
            "storage": { "kind": "default" },
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(state.join("provisioned"), "").unwrap();
    std::fs::write(state.join("service.plist"), "service").unwrap();
    std::fs::write(&autoload_agent, "legacy service").unwrap();
    std::fs::write(&vm_state, "exists").unwrap();
    std::fs::write(&unrelated, "keep").unwrap();
    std::fs::write(bundle_bin.join("gremvm"), "runtime").unwrap();
    let tart = bundle_bin.join("tart");
    std::fs::write(
        &tart,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_TART_TRACE"
case "$1" in
  list) [ ! -f "$FAKE_TART_STATE" ] || printf '%s\n' "$FAKE_VM_NAME" ;;
  get) printf '{"State":"stopped","CPU":6,"Memory":24576,"Disk":192,"OS":"darwin"}\n' ;;
  delete) [ "$2" = "$FAKE_VM_NAME" ] || exit 42; rm "$FAKE_TART_STATE" ;;
  *) exit 43 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink(bundle.path(), &runtime).unwrap();
    symlink(runtime.join("bin/gremvm"), &command_link).unwrap();

    Command::new(command)
        .env("HOME", home.path())
        .env("FAKE_VM_NAME", &name)
        .env("FAKE_TART_STATE", &vm_state)
        .env("FAKE_TART_TRACE", &trace)
        .arg("uninstall")
        .assert()
        .success()
        .stdout(format!("uninstalled: {name}\n"));

    assert!(!vm_state.exists());
    assert!(!runtime.exists());
    assert!(!command_link.exists());
    assert!(!state.join("service.plist").exists());
    assert!(!autoload_agent.exists());
    assert!(config.join("config.json").is_file());
    assert_eq!(std::fs::read_to_string(unrelated).unwrap(), "keep");
    assert!(
        std::fs::read_to_string(trace)
            .unwrap()
            .lines()
            .any(|line| line == format!("delete {name}"))
    );
}

#[test]
fn a_command_cannot_select_another_vms_configuration() {
    let home = tempfile::tempdir().unwrap();
    let aliases = tempfile::tempdir().unwrap();
    let foovm = command_alias(aliases.path(), "foovm");
    let config = instance_root(home.path(), "foovm").join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.json"),
        r#"{"vm_name":"barvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"}}"#,
    )
    .unwrap();

    Command::new(foovm)
        .env("HOME", home.path())
        .arg("start")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "configuration belongs to command 'barvm', not 'foovm'",
        ));
}

#[test]
fn the_original_command_preserves_legacy_custom_vm_names() {
    let home = tempfile::tempdir().unwrap();
    let root = instance_root(home.path(), "gremvm");
    let config = root.join("config");
    let state = root.join("state");
    let tart = root.join("runtime/bin/tart");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(tart.parent().unwrap()).unwrap();
    std::fs::write(
        config.join("config.json"),
        r#"{"vm_name":"oldvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"}}"#,
    )
    .unwrap();
    std::fs::write(state.join("provisioned"), "").unwrap();
    std::fs::write(
        &tart,
        "#!/bin/sh\ncase \"$1\" in\nlist) echo oldvm;;\nget) echo '{\"State\":\"stopped\",\"CPU\":6,\"Memory\":24576,\"Disk\":192,\"OS\":\"darwin\"}';;\n*) exit 1;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state: stopped\n"))
        .stdout(predicate::str::contains("name: oldvm\n"));
}

#[test]
fn suspended_vm_status_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let root = instance_root(home.path(), "gremvm");
    let config = root.join("config");
    let state = root.join("state");
    let tart = root.join("runtime/bin/tart");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(tart.parent().unwrap()).unwrap();
    std::fs::write(
        config.join("config.json"),
        r#"{"vm_name":"gremvm","cpu_count":6,"memory_gb":24,"disk_gb":192,"storage":{"kind":"default"}}"#,
    )
    .unwrap();
    std::fs::write(state.join("provisioned"), "").unwrap();
    std::fs::write(
        &tart,
        "#!/bin/sh\ncase \"$1\" in\nlist) echo gremvm;;\nget) echo '{\"State\":\"suspended\",\"CPU\":6,\"Memory\":24576,\"Disk\":192,\"OS\":\"darwin\"}';;\n*) exit 1;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&tart, std::fs::Permissions::from_mode(0o700)).unwrap();

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state: suspended\n"));

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .arg("screen-share")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "VM is not running; run 'gremvm start'",
        ));

    cargo_bin_cmd!("gremvm")
        .env("HOME", home.path())
        .args(["tailscale", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "VM is not running; run 'gremvm start'",
        ));
}
