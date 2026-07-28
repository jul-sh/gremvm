use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn packer_template_pins_the_image_and_preserves_recovery() {
    let packer = include_str!("../packer/gremvm.pkr.hcl");
    let auto_login = include_str!("../packer/auto-login.pl");
    let image = packer
        .lines()
        .find(|line| line.trim_start().starts_with("vm_base_name"))
        .and_then(|line| line.split('"').nth(1))
        .unwrap();
    let (repository, digest) = image.rsplit_once("@sha256:").unwrap();
    assert_eq!(repository, "ghcr.io/cirruslabs/macos-tahoe-vanilla");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(packer.contains("recovery_partition = \"relocate\""));
    assert!(packer.contains("headless           = true"));
    assert!(packer.contains("display            = \"1512x982px\""));
    assert!(packer.contains("systemsetup -setremotelogin on"));
    assert!(packer.contains("PasswordAuthentication no"));
    assert!(packer.contains("/usr/bin/perl /tmp/gremvm-auto-login.pl"));
    assert!(packer.contains("autoLoginUser admin"));
    assert!(auto_login.contains("<STDIN>"));
    assert!(packer.contains("socketfilterfw --setglobalstate off"));
}

#[test]
fn auto_login_encoder_reads_the_password_from_stdin() {
    let mut child = Command::new("/usr/bin/perl")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/packer/auto-login.pl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"admin").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        hex::decode("1ced3f4abcbcddeaa3b91f7d").unwrap()
    );
}

#[test]
fn background_and_console_runs_are_suspendable() {
    let driver = include_str!("../src/lib.rs");

    assert_eq!(driver.matches("\"--suspendable\"").count(), 2);
    assert!(driver.contains(".args([\"suspend\", &self.config.vm_name])"));
    assert!(driver.contains("Command::new(\"/usr/bin/caffeinate\")"));
    assert!(driver.contains("starting the background VM before opening the console"));
    assert!(driver.contains("saving VM state; this can take several minutes"));
    assert!(driver.contains("this command stays active until it closes"));
    assert!(driver.contains("console closed; restoring the background VM"));
    assert!(driver.contains("background restart was withheld to avoid a cold boot"));
}

#[test]
fn screen_sharing_uses_the_guest_ip() {
    let driver = include_str!("../src/lib.rs");

    assert!(driver.contains("Command::new(\"/usr/bin/open\").arg(format!(\"vnc://{ip}\"))"));
    assert!(driver.contains("Action::ScreenShare => self.screen_share()"));
}
