#[test]
fn packer_template_pins_the_image_and_preserves_recovery() {
    let packer = include_str!("../packer/gremvm.pkr.hcl");
    let configure_guest = include_str!("../packer/configure-guest.sh");
    let password_helper = include_str!("../packer/password.expect");
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
    assert!(packer.contains("ssh_username       = \"admin\""));
    assert!(packer.contains("ssh_password       = \"admin\""));
    assert!(packer.contains("\"GREMVM_GUEST_USER=${var.guest_user}\""));
    assert!(packer.contains("\"GREMVM_GUEST_PASSWORD=${var.guest_password}\""));
    assert!(packer.contains("use_env_var_file = true"));
    assert!(configure_guest.contains("systemsetup -setremotelogin on"));
    assert!(configure_guest.contains("PasswordAuthentication no"));
    assert!(configure_guest.contains("home=/Users/$user"));
    assert!(configure_guest.contains("$home/.skipbuddy"));
    assert!(configure_guest.contains("autologin \"$user\""));
    assert!(configure_guest.contains("[[ \"$user\" != admin ]]"));
    assert!(configure_guest.contains("unset GREMVM_GUEST_PASSWORD"));
    assert!(password_helper.contains("log_user 0"));
    assert!(password_helper.contains("set password [read -nonewline stdin]"));
    assert!(password_helper.contains("-autologin set -userName $user -password -"));
    assert!(!password_helper.contains("-password $password"));
    assert!(configure_guest.contains("socketfilterfw --setglobalstate off"));
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

#[test]
fn tailscale_upload_is_verified_before_root_executes_it() {
    let driver = include_str!("../src/lib.rs");

    assert!(driver.contains("umask 077; "));
    assert!(driver.contains(
        "let upload = format!(\"/Users/{}/.gremvm-tailscaled\", self.config.guest_user);"
    ));
    assert!(driver.contains("/private/var/tmp/gremvm-tailscaled.XXXXXX"));
    assert!(driver.contains("/usr/bin/shasum -a 256 \\\"$stage\\\""));
    assert!(!driver.contains(".gremvm-tailscaled install-system-daemon"));
}
