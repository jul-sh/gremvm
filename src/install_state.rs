#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VmInstallation {
    Absent,
    Partial,
    Ready,
    Unmanaged,
}

pub(crate) fn classify(vm_exists: bool, provisioned: bool, installing: bool) -> VmInstallation {
    match (vm_exists, provisioned, installing) {
        (true, true, _) => VmInstallation::Ready,
        (true, false, true) => VmInstallation::Partial,
        (true, false, false) => VmInstallation::Unmanaged,
        (false, _, _) => VmInstallation::Absent,
    }
}
