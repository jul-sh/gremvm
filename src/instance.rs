use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Identity {
    Gremvm,
    Named(String),
}

impl Identity {
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
    let (identity, root, service_label, keychain_helper_label) = match name.as_str() {
        "gremvm" => (
            Identity::Gremvm,
            base,
            "io.gremvm.tart".into(),
            "io.gremvm.keychain".into(),
        ),
        _ => (
            Identity::Named(name.clone()),
            base.join("instances").join(&name),
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
