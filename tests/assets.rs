#[test]
fn packer_template_pins_the_image_and_preserves_recovery() {
    let packer = include_str!("../packer/gremvm.pkr.hcl");
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
    assert!(packer.contains("systemsetup -setremotelogin on"));
    assert!(packer.contains("PasswordAuthentication no"));
    assert!(packer.contains("socketfilterfw --setglobalstate off"));
}
