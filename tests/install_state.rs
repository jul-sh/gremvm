#[path = "../src/install_state.rs"]
mod install_state;

use install_state::{VmInstallation, classify};

#[test]
fn installation_markers_define_every_vm_state() {
    use VmInstallation::{Absent, Partial, Ready, Unmanaged};

    for (vm, provisioned, installing, expected) in [
        (false, false, false, Absent),
        (false, false, true, Absent),
        (false, true, false, Absent),
        (false, true, true, Absent),
        (true, false, false, Unmanaged),
        (true, false, true, Partial),
        (true, true, false, Ready),
        (true, true, true, Ready),
    ] {
        assert_eq!(classify(vm, provisioned, installing), expected);
    }
}
