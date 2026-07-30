use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Identity {
    Gremvm,
    Named(String),
}

impl Identity {
    pub(crate) fn from_name(name: String) -> Self {
        match name.as_str() {
            "gremvm" => Self::Gremvm,
            _ => Self::Named(name),
        }
    }

    pub(crate) fn command_name(&self) -> &str {
        match self {
            Self::Gremvm => "gremvm",
            Self::Named(name) => name,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Instance {
    pub(crate) identity: Identity,
    pub(crate) root: PathBuf,
    pub(crate) service_label: String,
    pub(crate) keychain_helper_label: String,
    pub(crate) password_service: String,
}

pub(crate) fn resolve(home: &Path, name: String) -> Instance {
    let base = home.join("Library/Application Support/GremVM");
    let identity = Identity::from_name(name);
    let (root, service_label, keychain_helper_label) = match &identity {
        Identity::Gremvm => (base, "io.gremvm.tart".into(), "io.gremvm.keychain".into()),
        Identity::Named(name) => (
            base.join("instances").join(name),
            format!("io.gremvm.tart.{name}"),
            format!("io.gremvm.keychain.{name}"),
        ),
    };
    Instance {
        identity,
        root,
        password_service: format!("{service_label}.gui-password"),
        service_label,
        keychain_helper_label,
    }
}
