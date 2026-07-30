use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
use serde::{Deserialize, Serialize};
use signal_hook::{
    consts::{SIGHUP, TERM_SIGNALS},
    iterator::Signals,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

mod install_state;
mod instance;

use install_state::{VmInstallation, classify};
use instance::{Identity, resolve};

const VOLUME_PASSWORD_SERVICE: &str = "io.gremvm.volume-password";
const BRIDGE: &str = "en0";

#[derive(Parser)]
#[command(
    bin_name = "nix run . --",
    version,
    about = "Install a persistent Tart macOS VM",
    arg_required_else_help = true
)]
enum InstallerAction {
    /// Install or update GremVM and create the VM if needed.
    Install(InstallOptions),
}

#[derive(Parser)]
#[command(
    version,
    about = "Manage a persistent Tart macOS VM",
    arg_required_else_help = true,
    after_help = "The guest always uses bridged networking on en0."
)]
enum Action {
    /// Show VM state.
    Status,
    /// Start the VM and wait for SSH.
    Start,
    /// Stop the VM and disable automatic restart.
    Stop,
    /// Stop and start the VM.
    Restart,
    /// Connect as the configured guest user.
    Ssh {
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Open the guest in macOS Screen Sharing.
    ScreenShare,
    /// Open Tart's local recovery console.
    Console,
    /// Manage CLI-only Tailscale inside the guest.
    Tailscale {
        #[command(subcommand)]
        action: TailscaleAction,
    },
    /// Show VM logs.
    Logs {
        #[arg(long)]
        follow: bool,
    },
    /// Delete the VM and remove its service and runtime.
    Uninstall,
    #[command(name = "internal-run", hide = true)]
    InternalRun,
    #[command(name = "internal-keychain", hide = true)]
    InternalKeychain {
        #[arg(value_enum)]
        mode: KeychainHelperMode,
    },
}

#[derive(Subcommand)]
enum TailscaleAction {
    /// Install, upgrade, and connect Tailscale.
    Setup,
    /// Show the guest's Tailscale connection.
    Status,
}

#[derive(Args)]
struct InstallOptions {
    /// VM and command name to install.
    #[arg(value_name = "NAME", value_parser = valid_name)]
    name: String,
    /// Number of virtual CPUs.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..=64))]
    cpu_count: u32,
    /// Guest memory in GiB.
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u32).range(4..=256))]
    memory_gb: u32,
    /// Virtual disk size in decimal GB.
    #[arg(long, default_value_t = 192, value_parser = clap::value_parser!(u64).range(50..))]
    disk_gb: u64,
    /// Guest account short name.
    #[arg(long, default_value = "admin", value_parser = valid_user)]
    guest_user: String,
    /// Prompt for the initial guest password instead of generating one.
    #[arg(long)]
    ask_password: bool,
    /// Existing absolute directory containing the VM.
    #[arg(long, value_name = "DIRECTORY", value_parser = existing_directory)]
    storage: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
struct Config {
    vm_name: String,
    guest_user: String,
    cpu_count: u32,
    memory_gb: u32,
    disk_gb: u64,
    storage: Storage,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Storage {
    Default,
    Directory {
        path: PathBuf,
    },
    PlainVolume(VolumeStorage),
    #[serde(rename = "encrypted-volume")]
    EncryptedVolume(VolumeStorage),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeStorage {
    path: PathBuf,
    mount_point: PathBuf,
    name: String,
    uuid: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfig {
    vm_name: String,
    #[serde(default = "default_guest_user")]
    guest_user: String,
    cpu_count: u32,
    memory_gb: u32,
    disk_gb: u64,
    storage: StoredStorage,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum StoredStorage {
    Default,
    Directory {
        path: PathBuf,
    },
    PlainVolume(VolumeStorage),
    EncryptedVolume(VolumeStorage),
    #[serde(rename = "volume")]
    LegacyVolume {
        name: String,
        uuid: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VolumeKind {
    Plain,
    Encrypted,
}

#[derive(Clone, Copy)]
enum PasswordChoice {
    Generate,
    Prompt,
}

#[derive(Clone, Copy)]
enum InstallPlan {
    Create,
    Update,
}

impl InstallOptions {
    fn resolve(self, paths: &Paths) -> Result<(Config, PasswordChoice)> {
        let storage = match self.storage {
            None => Storage::Default,
            Some(path) => {
                let volume = volume_for_path(&path)?;
                if volume.volume_uuid == volume_for_path(&paths.home)?.volume_uuid {
                    Storage::Directory { path }
                } else {
                    let encrypted = volume.file_vault;
                    if encrypted {
                        ensure!(
                            volume.filesystem_type == "apfs",
                            "encrypted VM storage volume must be APFS"
                        );
                    }
                    let mount_point = volume
                        .mount_point
                        .context("storage volume is not mounted")?;
                    let name = volume.volume_name;
                    let uuid = volume.volume_uuid;
                    validate_volume_reference(&path, &mount_point, &name, &uuid)?;
                    let volume = VolumeStorage {
                        path,
                        mount_point,
                        name,
                        uuid,
                    };
                    if encrypted {
                        Storage::EncryptedVolume(volume)
                    } else {
                        Storage::PlainVolume(volume)
                    }
                }
            }
        };
        let vm_name = match &paths.identity {
            Identity::Gremvm if paths.config_file.exists() => {
                Config::load(paths)
                    .context("persisted configuration is invalid")?
                    .vm_name
            }
            Identity::Gremvm => "gremvm".into(),
            Identity::Named(name) => name.clone(),
        };
        Ok((
            Config {
                vm_name,
                guest_user: self.guest_user,
                cpu_count: self.cpu_count,
                memory_gb: self.memory_gb,
                disk_gb: self.disk_gb,
                storage,
            },
            if self.ask_password {
                PasswordChoice::Prompt
            } else {
                PasswordChoice::Generate
            },
        ))
    }
}

impl Config {
    fn load(paths: &Paths) -> Result<Self> {
        let stored: StoredConfig = serde_json::from_reader(fs::File::open(&paths.config_file)?)?;
        let storage = match stored.storage {
            StoredStorage::Default => Storage::Default,
            StoredStorage::Directory { path } => Storage::Directory { path },
            StoredStorage::PlainVolume(volume) => Storage::PlainVolume(volume),
            StoredStorage::EncryptedVolume(volume) => Storage::EncryptedVolume(volume),
            StoredStorage::LegacyVolume { name, uuid } => {
                let path = Path::new("/Volumes").join(&name);
                Storage::EncryptedVolume(VolumeStorage {
                    mount_point: path.clone(),
                    path,
                    name,
                    uuid,
                })
            }
        };
        let config = Self {
            vm_name: stored.vm_name,
            guest_user: stored.guest_user,
            cpu_count: stored.cpu_count,
            memory_gb: stored.memory_gb,
            disk_gb: stored.disk_gb,
            storage,
        };
        valid_name(&config.vm_name).map_err(anyhow::Error::msg)?;
        match &paths.identity {
            Identity::Gremvm => {}
            Identity::Named(name) => ensure!(
                config.vm_name == *name,
                "configuration belongs to command '{}', not '{name}'",
                config.vm_name
            ),
        }
        valid_user(&config.guest_user).map_err(anyhow::Error::msg)?;
        match &config.storage {
            Storage::Default => {}
            Storage::Directory { path } => {
                ensure!(path.is_absolute(), "storage directory must be absolute")
            }
            Storage::PlainVolume(volume) | Storage::EncryptedVolume(volume) => {
                validate_volume_reference(
                    &volume.path,
                    &volume.mount_point,
                    &volume.name,
                    &volume.uuid,
                )?;
            }
        }
        ensure!((1..=64).contains(&config.cpu_count), "invalid CPU count");
        ensure!((4..=256).contains(&config.memory_gb), "invalid memory size");
        ensure!(config.disk_gb >= 50, "invalid disk size");
        Ok(config)
    }

    fn check_existing(&self, paths: &Paths) -> Result<()> {
        if paths.config_file.exists() {
            ensure!(
                Self::load(paths)? == *self,
                "install settings differ from the saved configuration at {}",
                paths.config_file.display()
            );
        }
        Ok(())
    }

    fn persist(&self, paths: &Paths) -> Result<()> {
        private_dir(&paths.config_dir)?;
        let mut file = NamedTempFile::new_in(&paths.config_dir)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        writeln!(file)?;
        file.as_file().sync_all()?;
        file.persist(&paths.config_file)?;
        Ok(())
    }
}

struct Paths {
    identity: Identity,
    home: PathBuf,
    root: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
    run_marker: PathBuf,
    installing: PathBuf,
    provisioned: PathBuf,
    service_plist: PathBuf,
    command_link: PathBuf,
    ssh_key: PathBuf,
    guest_tailscale: PathBuf,
    service_label: String,
    keychain_helper_label: String,
    password_service: String,
}

impl Paths {
    fn discover(name: String) -> Result<Self> {
        valid_name(&name).map_err(anyhow::Error::msg)?;
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let instance = resolve(&home, name);
        let root = instance.root;
        let config_dir = root.join("config");
        let state = root.join("state");
        Ok(Self {
            command_link: home
                .join(".local/bin")
                .join(instance.identity.command_name()),
            service_plist: state.join("service.plist"),
            runtime: root.join("runtime"),
            run_marker: state.join("run"),
            installing: state.join("installing"),
            provisioned: state.join("provisioned"),
            ssh_key: config_dir.join("id_ed25519"),
            guest_tailscale: root.join("runtime/libexec/gremvm/tailscaled"),
            logs: root.join("logs"),
            home,
            root,
            config_file: config_dir.join("config.json"),
            config_dir,
            state,
            identity: instance.identity,
            password_service: instance.password_service,
            service_label: instance.service_label,
            keychain_helper_label: instance.keychain_helper_label,
        })
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.runtime.join("bin").join(name)
    }

    fn command_name(&self) -> &str {
        self.identity.command_name()
    }

    fn service_target(&self) -> String {
        format!("{}/{}", user_domain(), self.service_label)
    }

    fn keychain_helper_target(&self) -> String {
        format!("{}/{}", user_domain(), self.keychain_helper_label)
    }

    fn install_hint(&self) -> String {
        format!(
            "nix run github:jul-sh/gremvm -- install {}",
            self.command_name()
        )
    }

    fn autoload_agent(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", self.service_label))
    }
}

#[derive(Deserialize)]
struct VmInfo {
    #[serde(rename = "State")]
    state: VmState,
    #[serde(rename = "CPU")]
    cpu: u32,
    #[serde(rename = "Memory")]
    memory: u64,
    #[serde(rename = "Disk")]
    disk: u64,
    #[serde(rename = "OS")]
    os: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum VmState {
    Running,
    Suspended,
    Stopped,
}

enum TailscaleState {
    NotInstalled,
    Disconnected,
    Connected { ip: String },
}

enum ConsoleSource {
    Snapshot,
    ColdBoot,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VolumeInfo {
    #[serde(default)]
    file_vault: bool,
    filesystem_type: String,
    #[serde(default)]
    locked: bool,
    mount_point: Option<PathBuf>,
    volume_name: String,
    #[serde(rename = "VolumeUUID")]
    volume_uuid: String,
    #[serde(default)]
    writable_volume: bool,
}

#[derive(Deserialize)]
struct ConsoleInfo {
    #[serde(rename = "IOConsoleLocked", default)]
    locked: bool,
    #[serde(rename = "IOConsoleUsers")]
    users: Vec<ConsoleSession>,
}

#[derive(Deserialize)]
struct ConsoleSession {
    #[serde(rename = "kCGSSessionUserIDKey")]
    user_id: u32,
    #[serde(rename = "kCGSSessionOnConsoleKey", default)]
    on_console: bool,
    #[serde(rename = "kCGSessionLoginDoneKey", default)]
    login_done: bool,
    #[serde(rename = "CGSSessionScreenIsLocked", default)]
    screen_locked: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AgentPlist {
    label: String,
    program_arguments: Vec<String>,
    environment_variables: BTreeMap<&'static str, String>,
    keep_alive: KeepAlive,
    limit_load_to_session_type: &'static str,
    process_type: &'static str,
    throttle_interval: u32,
    standard_out_path: String,
    standard_error_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct KeepAlive {
    path_state: BTreeMap<String, bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct KeychainHelperPlist {
    label: String,
    program_arguments: Vec<String>,
    environment_variables: BTreeMap<&'static str, String>,
    limit_load_to_session_type: &'static str,
    process_type: &'static str,
    run_at_load: bool,
    standard_in_path: String,
    standard_out_path: String,
    standard_error_path: String,
}

#[derive(Clone, Copy)]
enum CredentialPolicy {
    Existing,
    CreateIfMissing,
}

#[derive(Clone, Copy)]
enum KeychainMode {
    Current,
    BackgroundInteractive,
    Background,
}

#[derive(Clone, Copy)]
enum StorageAccess {
    Interactive,
    Background,
}

enum VolumeUnlock {
    Unnecessary,
    Password(Vec<u8>),
}

#[derive(Clone, Copy, ValueEnum)]
enum KeychainHelperMode {
    Check,
    Unlock,
}

enum KeychainRequest {
    Check,
    Unlock { terminal: String },
}

struct HiddenInput {
    terminal: fs::File,
    original: Termios,
}

impl HiddenInput {
    fn new(path: &str) -> Result<Self> {
        let terminal = OpenOptions::new().read(true).write(true).open(path)?;
        let original = tcgetattr(&terminal)?;
        let input = Self { terminal, original };
        let mut hidden = input.original.clone();
        hidden
            .local_flags
            .remove(LocalFlags::ECHO | LocalFlags::ECHONL);
        tcsetattr(&input.terminal, SetArg::TCSANOW, &hidden)?;
        Ok(input)
    }
}

impl Drop for HiddenInput {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.terminal, SetArg::TCSANOW, &self.original);
    }
}

struct App {
    paths: Paths,
    config: Config,
}

pub fn command_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|path| {
            PathBuf::from(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "gremvm".into())
}

pub fn run() -> Result<()> {
    let command = Action::parse();
    let name = command_name();
    valid_name(&name).map_err(anyhow::Error::msg)?;
    let paths = Paths::discover(name)?;
    let _lock = match &command {
        Action::Start
        | Action::Stop
        | Action::Restart
        | Action::Console
        | Action::Tailscale {
            action: TailscaleAction::Setup,
        }
        | Action::Uninstall => Some(management_lock(&paths)?),
        _ => None,
    };
    match command {
        Action::InternalKeychain { mode } => internal_keychain(&paths, mode),
        Action::Status if !is_executable(&paths.bin("tart")) => {
            println!("state: not-installed");
            Ok(())
        }
        command => {
            ensure!(
                paths.config_file.is_file(),
                "configuration is missing; run '{}'",
                paths.install_hint()
            );
            App {
                config: Config::load(&paths).context("persisted configuration is invalid")?,
                paths,
            }
            .dispatch(command)
        }
    }
}

pub fn run_installer() -> Result<()> {
    let InstallerAction::Install(options) = InstallerAction::parse();
    let paths = Paths::discover(options.name.clone())?;
    let _lock = management_lock(&paths)?;
    let (config, password) = options.resolve(&paths)?;
    App { paths, config }.install(password)
}

impl App {
    fn dispatch(&self, command: Action) -> Result<()> {
        match command {
            Action::Status => self.status(),
            Action::Start => self.start(),
            Action::Stop => self.stop(),
            Action::Restart => self.restart(),
            Action::Ssh { command } => self.ssh(&command),
            Action::ScreenShare => self.screen_share(),
            Action::Console => self.console(),
            Action::Tailscale { action } => self.tailscale(action),
            Action::Logs { follow } => self.logs(follow),
            Action::Uninstall => self.uninstall(),
            Action::InternalRun => self.internal_run(),
            Action::InternalKeychain { .. } => unreachable!(),
        }
    }

    fn install(&self, password: PasswordChoice) -> Result<()> {
        self.clear_keychain_helper()?;
        self.config.check_existing(&self.paths)?;
        self.ensure_keychain(KeychainMode::Current)?;
        self.ensure_storage(StorageAccess::Interactive)?;
        self.validate_host()?;
        self.install_runtime()?;
        self.install_command()?;
        let plan = self.install_plan()?;
        let policy = match plan {
            InstallPlan::Create => CredentialPolicy::CreateIfMissing,
            InstallPlan::Update => CredentialPolicy::Existing,
        };
        self.install_ssh_key(policy)?;
        self.prepare_guest_password(policy, password)?;
        self.config.persist(&self.paths)?;
        remove_if_present(&self.paths.run_marker)?;
        self.install_service()?;
        if let InstallPlan::Create = plan {
            remove_if_present(&self.paths.provisioned)?;
            touch(&self.paths.installing)?;
            self.build_vm()?;
            ensure!(
                self.vm_exists()?,
                "Packer completed without creating the VM"
            );
            self.verify_config()?;
            touch(&self.paths.provisioned)?;
            remove_if_present(&self.paths.installing)?;
        }
        self.stop_tart()?;
        println!("installed: {}", self.paths.command_link.display());
        println!("VM: {} (stopped)", self.config.vm_name);
        println!("next: {} start", self.paths.command_name());
        Ok(())
    }

    fn install_plan(&self) -> Result<InstallPlan> {
        match classify(
            self.vm_exists()?,
            self.paths.provisioned.exists(),
            self.paths.installing.exists(),
        ) {
            VmInstallation::Absent => Ok(InstallPlan::Create),
            VmInstallation::Partial => {
                println!("removing incomplete VM before retrying...");
                self.delete_partial_vm()?;
                Ok(InstallPlan::Create)
            }
            VmInstallation::Ready => {
                self.verify_config()?;
                remove_if_present(&self.paths.installing)?;
                Ok(InstallPlan::Update)
            }
            VmInstallation::Unmanaged => bail!(
                "a VM named '{}' exists but was not created by GremVM; rename or remove it before rerunning '{}'",
                self.config.vm_name,
                self.paths.install_hint()
            ),
        }
    }

    fn validate_host(&self) -> Result<()> {
        ensure!(
            std::env::consts::ARCH == "aarch64",
            "Tart requires Apple silicon"
        );
        ensure!(
            u64::from(self.config.cpu_count) <= sysctl("hw.logicalcpu")?,
            "requested CPU count exceeds the host"
        );
        ensure!(
            u64::from(self.config.memory_gb) * 1024 * 1024 * 1024 < sysctl("hw.memsize")?,
            "requested memory exceeds the host"
        );
        validate_bridge()
    }

    fn install_runtime(&self) -> Result<()> {
        private_dir(&self.paths.root)?;
        ensure!(
            !fs::symlink_metadata(&self.paths.runtime).is_ok_and(|m| m.is_dir()),
            "refusing to replace runtime directory: {}",
            self.paths.runtime.display()
        );
        let executable = std::env::current_exe()?.canonicalize()?;
        let bundle = executable
            .parent()
            .and_then(Path::parent)
            .context("cannot locate the packaged GremVM bundle")?;
        for file in [
            "bin/gremvm",
            "bin/tart",
            "bin/packer",
            "share/gremvm/gremvm.pkr.hcl",
            "share/gremvm/configure-guest.sh",
            "share/gremvm/password.expect",
            "libexec/gremvm/tailscaled",
        ] {
            ensure!(
                bundle.join(file).is_file(),
                "packaged file is missing: {file}"
            );
        }
        checked(
            Command::new("nix-store")
                .arg("--realise")
                .arg(bundle)
                .arg("--add-root")
                .arg(&self.paths.runtime)
                .arg("--indirect"),
            "register the Nix runtime GC root",
        )?;
        ensure!(
            self.paths.runtime.canonicalize()? == bundle,
            "Nix registered an unexpected runtime"
        );
        Ok(())
    }

    fn install_command(&self) -> Result<()> {
        let directory = self
            .paths
            .command_link
            .parent()
            .context("invalid command link path")?;
        ensure!(
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|entry| entry == directory)
            }),
            "{} must be in PATH",
            directory.display()
        );
        private_dir(directory)?;
        let target = self.paths.bin("gremvm");
        match fs::symlink_metadata(&self.paths.command_link) {
            Ok(metadata) if metadata.file_type().is_symlink() => ensure!(
                fs::read_link(&self.paths.command_link)? == target,
                "refusing to replace command link: {}",
                self.paths.command_link.display()
            ),
            Ok(_) => bail!(
                "refusing to replace command path: {}",
                self.paths.command_link.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                symlink(target, &self.paths.command_link)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn install_ssh_key(&self, policy: CredentialPolicy) -> Result<()> {
        private_dir(&self.paths.config_dir)?;
        let public = self.paths.ssh_key.with_extension("pub");
        if !self.paths.ssh_key.exists() && !public.exists() {
            if let CredentialPolicy::Existing = policy {
                bail!("the SSH key is missing and cannot be regenerated for an existing VM");
            }
            success(
                Command::new("/usr/bin/ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C"])
                    .arg(format!("gremvm@{}", self.config.vm_name))
                    .arg("-f")
                    .arg(&self.paths.ssh_key),
                "generate the SSH key",
            )?;
        }
        ensure!(
            self.paths.ssh_key.is_file() && public.is_file(),
            "SSH key is incomplete"
        );
        fs::set_permissions(&self.paths.ssh_key, fs::Permissions::from_mode(0o600))?;
        fs::set_permissions(public, fs::Permissions::from_mode(0o644))?;
        Ok(())
    }

    fn prepare_guest_password(
        &self,
        policy: CredentialPolicy,
        choice: PasswordChoice,
    ) -> Result<()> {
        self.ensure_keychain(KeychainMode::Current)?;
        if self
            .keychain_password(&self.config.guest_user, &self.paths.password_service)?
            .is_some()
        {
            self.guest_password()?;
            return Ok(());
        }
        if let CredentialPolicy::Existing = policy {
            bail!(
                "the guest password is missing from Keychain and cannot be regenerated for an existing VM"
            );
        }
        if let PasswordChoice::Prompt = choice {
            let password = prompt_guest_password()?;
            return self.store_keychain_password(
                &self.config.guest_user,
                &self.paths.password_service,
                &password,
            );
        }
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random)?;
        self.store_keychain_password(
            &self.config.guest_user,
            &self.paths.password_service,
            &hex::encode(random),
        )
    }

    fn guest_password(&self) -> Result<String> {
        self.ensure_keychain(KeychainMode::Current)?;
        let bytes = self
            .keychain_password(&self.config.guest_user, &self.paths.password_service)?
            .context("the guest password is missing from Keychain")?;
        let password =
            String::from_utf8(bytes).context("the stored guest password is not UTF-8")?;
        validate_guest_password(&password).map_err(anyhow::Error::msg)?;
        Ok(password)
    }

    fn keychain_password(&self, account: &str, service: &str) -> Result<Option<Vec<u8>>> {
        let output = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-a", account, "-s", service, "-w"])
            .arg(login_keychain(&self.paths))
            .output()
            .context("cannot read a password from Keychain")?;
        if output.status.success() {
            let mut password = output.stdout;
            if password.ends_with(b"\n") {
                password.pop();
                if password.ends_with(b"\r") {
                    password.pop();
                }
            }
            ensure!(!password.is_empty(), "the stored password is empty");
            return Ok(Some(password));
        }
        ensure!(
            output.status.code() == Some(44),
            "cannot read a password from Keychain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(None)
    }

    fn store_keychain_password(&self, account: &str, service: &str, password: &str) -> Result<()> {
        let mut security = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot store a password in Keychain")?;
        let keychain = login_keychain(&self.paths)
            .to_str()
            .context("login Keychain path is not UTF-8")?
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        writeln!(
            security
                .stdin
                .take()
                .context("security stdin is unavailable")?,
            "add-generic-password -a {account} -s {service} -U -X {} \"{keychain}\"",
            hex::encode(password.as_bytes()),
        )?;
        check_output(
            &security.wait_with_output()?,
            "store a password in Keychain",
        )
    }

    fn install_service(&self) -> Result<()> {
        let parent = self
            .paths
            .service_plist
            .parent()
            .context("invalid LaunchAgent path")?;
        for directory in [parent, &self.paths.logs, &self.paths.state] {
            private_dir(directory)?;
        }
        let string = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .with_context(|| format!("path is not UTF-8: {}", path.display()))
        };
        let runtime = string(&self.paths.runtime)?;
        let plist = AgentPlist {
            label: self.paths.service_label.clone(),
            program_arguments: vec![string(&self.paths.command_link)?, "internal-run".into()],
            environment_variables: BTreeMap::from([
                ("HOME", string(&self.paths.home)?),
                (
                    "PATH",
                    format!("{runtime}/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
                ),
            ]),
            keep_alive: KeepAlive {
                path_state: BTreeMap::from([(string(&self.paths.run_marker)?, true)]),
            },
            limit_load_to_session_type: "Background",
            process_type: "Background",
            throttle_interval: 10,
            standard_out_path: string(&self.paths.logs.join("vm.log"))?,
            standard_error_path: string(&self.paths.logs.join("vm.error.log"))?,
        };
        let mut temporary = NamedTempFile::new_in(parent)?;
        plist::to_writer_xml(temporary.as_file_mut(), &plist)?;
        temporary.flush()?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        temporary.as_file().sync_all()?;
        let target = self.paths.service_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the service")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.service_loaded(&target)? {
                ensure!(Instant::now() < deadline, "service did not unload");
                thread::sleep(Duration::from_millis(50));
            }
        }
        remove_if_present(&self.paths.autoload_agent())?;
        temporary.persist(&self.paths.service_plist)?;
        Ok(())
    }

    fn service_loaded(&self, target: &str) -> Result<bool> {
        let output = Command::new("/bin/launchctl")
            .args(["print", target])
            .output()
            .context("cannot inspect the launchd service")?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(113) => Ok(false),
            _ => bail!(
                "cannot inspect launchd service {target}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
    }

    fn launchctl(&self, arguments: &[&str], description: &str) -> Result<()> {
        success(Command::new("/bin/launchctl").args(arguments), description)
    }

    fn bootstrap(&self) -> Result<()> {
        if !self.service_loaded(&self.paths.service_target())? {
            self.launchctl(
                &[
                    "bootstrap",
                    &user_domain(),
                    self.paths
                        .service_plist
                        .to_str()
                        .context("LaunchAgent path is not UTF-8")?,
                ],
                "load the service",
            )?;
        }
        Ok(())
    }

    fn start_service(&self) -> Result<()> {
        self.bootstrap()?;
        self.launchctl(
            &["kickstart", &self.paths.service_target()],
            "start the service",
        )
    }

    fn build_vm(&self) -> Result<()> {
        let public_key = fs::read_to_string(self.paths.ssh_key.with_extension("pub"))?;
        let password = self.guest_password()?;
        println!(
            "creating {} from the pinned macOS image...",
            self.config.vm_name
        );
        self.packer(public_key.trim(), &password)
    }

    fn delete_partial_vm(&self) -> Result<()> {
        match self.vm_info()?.state {
            VmState::Running | VmState::Suspended => success(
                self.tart()?
                    .args(["stop", &self.config.vm_name, "--timeout", "30"]),
                "stop the incomplete VM",
            )?,
            VmState::Stopped => {}
        }
        self.delete_vm()
    }

    fn delete_vm(&self) -> Result<()> {
        if !self.vm_exists()? {
            return Ok(());
        }
        success(
            self.tart()?.args(["delete", &self.config.vm_name]),
            "delete the VM",
        )?;
        ensure!(!self.vm_exists()?, "Tart did not delete the VM");
        Ok(())
    }

    fn packer(&self, key: &str, password: &str) -> Result<()> {
        let work = self.paths.state.join("packer");
        private_dir(&work)?;
        let plugin = self.paths.runtime.join("libexec/packer/plugins");
        ensure!(
            plugin.join("github.com/cirruslabs/tart").is_dir(),
            "Packer plugin is missing"
        );
        let mut command = Command::new(self.paths.bin("packer"));
        self.configure_tart_storage(&mut command)?;
        let path = std::env::join_paths([
            self.paths.runtime.join("bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
            PathBuf::from("/usr/sbin"),
            PathBuf::from("/sbin"),
        ])?;
        command
            .current_dir(work)
            .env("PATH", path)
            .env("PACKER_PLUGIN_PATH", plugin)
            .env("PACKER_NO_COLOR", "1")
            .env("CHECKPOINT_DISABLE", "1")
            .env("PKR_VAR_vm_name", &self.config.vm_name)
            .env("PKR_VAR_guest_user", &self.config.guest_user)
            .env("PKR_VAR_cpu_count", self.config.cpu_count.to_string())
            .env("PKR_VAR_memory_gb", self.config.memory_gb.to_string())
            .env("PKR_VAR_disk_size_gb", self.config.disk_gb.to_string())
            .env("PKR_VAR_ssh_public_key", key)
            .env("PKR_VAR_guest_password", password);
        let template = self.paths.runtime.join("share/gremvm/gremvm.pkr.hcl");
        command.args(["build", "-color=false"]).arg(template);
        ensure!(
            command.status().context("cannot run Packer")?.success(),
            "Packer failed"
        );
        Ok(())
    }

    fn ensure_keychain(&self, mode: KeychainMode) -> Result<()> {
        match mode {
            KeychainMode::Current => self.ensure_current_keychain()?,
            KeychainMode::BackgroundInteractive => self.ensure_background_keychain()?,
            KeychainMode::Background => ensure!(
                keychain_unlocked(&self.paths)?,
                "the host login Keychain is locked; run '{} start' from an interactive terminal",
                self.paths.command_name()
            ),
        }
        Ok(())
    }

    fn ensure_current_keychain(&self) -> Result<()> {
        if !keychain_unlocked(&self.paths)? {
            let terminal = interactive_terminal()?;
            ensure!(
                unlock_keychain(&self.paths, &terminal)?,
                "failed to unlock the host login Keychain"
            );
        }
        Ok(())
    }

    fn ensure_background_keychain(&self) -> Result<()> {
        if !self.run_keychain_helper(KeychainRequest::Check)? {
            ensure!(
                self.run_keychain_helper(KeychainRequest::Unlock {
                    terminal: interactive_terminal()?,
                })?,
                "failed to unlock the login Keychain for the Background service"
            );
        }
        Ok(())
    }

    fn run_keychain_helper(&self, request: KeychainRequest) -> Result<bool> {
        let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
        match request {
            KeychainRequest::Check => {
                self.run_keychain_helper_on("check", "/dev/null", 5, &mut signals)
            }
            KeychainRequest::Unlock { terminal } => {
                ensure!(terminal.starts_with("/dev/tty"), "invalid terminal path");
                let _input = HiddenInput::new(&terminal)?;
                self.run_keychain_helper_on("unlock", &terminal, 300, &mut signals)
            }
        }
    }

    fn run_keychain_helper_on(
        &self,
        mode: &str,
        terminal: &str,
        timeout: u64,
        signals: &mut Signals,
    ) -> Result<bool> {
        private_dir(&self.paths.state)?;
        let plist_path = self.paths.state.join("keychain.plist");
        let result_path = self.paths.state.join("keychain.result");
        let domain = user_domain();
        self.cleanup_keychain_helper()?;

        let string = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .with_context(|| format!("path is not UTF-8: {}", path.display()))
        };
        let plist = KeychainHelperPlist {
            label: self.paths.keychain_helper_label.clone(),
            program_arguments: vec![
                string(&self.paths.command_link)?,
                "internal-keychain".into(),
                mode.into(),
            ],
            environment_variables: BTreeMap::from([("HOME", string(&self.paths.home)?)]),
            limit_load_to_session_type: "Background",
            process_type: "Background",
            run_at_load: true,
            standard_in_path: terminal.to_owned(),
            standard_out_path: terminal.to_owned(),
            standard_error_path: terminal.to_owned(),
        };
        let mut temporary = NamedTempFile::new_in(&self.paths.state)?;
        plist::to_writer_xml(temporary.as_file_mut(), &plist)?;
        temporary.flush()?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.persist(&plist_path)?;
        let plist_path = string(&plist_path)?;

        let outcome = (|| {
            self.launchctl(
                &["bootstrap", &domain, &plist_path],
                "load the Keychain helper",
            )?;
            let deadline = Instant::now() + Duration::from_secs(timeout);
            loop {
                match fs::read_to_string(&result_path) {
                    Ok(result) => return Ok(result.trim() == "unlocked"),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                ensure!(
                    signals.pending().next().is_none(),
                    "Keychain unlock was interrupted"
                );
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for the Keychain helper"
                );
                thread::sleep(Duration::from_millis(50));
            }
        })();
        let cleanup = self.cleanup_keychain_helper();
        match (outcome, cleanup) {
            (Ok(unlocked), Ok(())) => Ok(unlocked),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("Keychain helper cleanup also failed: {cleanup:#}")))
            }
        }
    }

    fn clear_keychain_helper(&self) -> Result<()> {
        private_dir(&self.paths.state)?;
        self.cleanup_keychain_helper()
    }

    fn cleanup_keychain_helper(&self) -> Result<()> {
        let target = self.paths.keychain_helper_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the Keychain helper")?;
        }
        remove_if_present(&self.paths.state.join("keychain.plist"))?;
        remove_if_present(&self.paths.state.join("keychain.result"))
    }

    fn ensure_directory(&self, path: &Path) -> Result<()> {
        ensure!(
            canonical_directory(path)? == path,
            "VM storage directory changed identity: {}",
            path.display()
        );
        Ok(())
    }

    fn ensure_volume(
        &self,
        storage: &VolumeStorage,
        kind: VolumeKind,
        access: StorageAccess,
    ) -> Result<()> {
        let VolumeStorage {
            path,
            mount_point,
            name,
            uuid,
        } = storage;
        validate_volume_reference(path, mount_point, name, uuid)?;
        let mut volume = volume_info(uuid)?;
        validate_volume(&volume, name, uuid, kind)?;
        let unlock = match (kind, access, volume.locked) {
            (VolumeKind::Plain, _, _)
            | (VolumeKind::Encrypted, StorageAccess::Background, false) => {
                VolumeUnlock::Unnecessary
            }
            (VolumeKind::Encrypted, StorageAccess::Interactive, locked) => {
                self.ensure_keychain(KeychainMode::Current)?;
                let stored = self.keychain_password(uuid, VOLUME_PASSWORD_SERVICE)?;
                let password = match stored {
                    Some(password) if self.volume_password_valid(uuid, &password)? => password,
                    _ => {
                        let password =
                            prompt_password(&format!("password for encrypted volume {name}: "))?;
                        ensure!(
                            self.volume_password_valid(uuid, password.as_bytes())?,
                            "incorrect password for encrypted volume {name}"
                        );
                        self.store_keychain_password(uuid, VOLUME_PASSWORD_SERVICE, &password)?;
                        password.into_bytes()
                    }
                };
                if locked {
                    VolumeUnlock::Password(password)
                } else {
                    VolumeUnlock::Unnecessary
                }
            }
            (VolumeKind::Encrypted, StorageAccess::Background, true) => {
                self.ensure_keychain(KeychainMode::Background)?;
                VolumeUnlock::Password(
                    self.keychain_password(uuid, VOLUME_PASSWORD_SERVICE)?
                        .context(format!(
                            "encrypted volume credential is missing; run '{} start' interactively",
                            self.paths.command_name()
                        ))?,
                )
            }
        };
        match unlock {
            VolumeUnlock::Unnecessary => {}
            VolumeUnlock::Password(password) => {
                self.run_volume_password(
                    uuid,
                    &password,
                    &["apfs", "unlockVolume", uuid, "-stdinpassphrase", "-nomount"],
                    "unlock the VM storage volume",
                )?;
                volume = volume_info(uuid)?;
                validate_volume(&volume, name, uuid, kind)?;
            }
        }
        if volume
            .mount_point
            .as_ref()
            .is_none_or(|path| path.as_os_str().is_empty())
        {
            let standard_mount = Path::new("/Volumes").join(name);
            let mut mount = Command::new("/usr/sbin/diskutil");
            mount.arg("mount");
            if mount_point.as_path() == standard_mount {
                ensure!(
                    !mount_point.exists(),
                    "VM storage mount path already exists: {}",
                    mount_point.display()
                );
            } else {
                ensure!(
                    mount_point.is_dir(),
                    "custom VM storage mount point is missing: {}",
                    mount_point.display()
                );
                ensure!(
                    fs::read_dir(mount_point)?.next().is_none(),
                    "custom VM storage mount point is not empty: {}",
                    mount_point.display()
                );
                mount.arg("-mountPoint").arg(mount_point);
            }
            success(
                mount.arg(uuid).stdin(Stdio::null()),
                "mount the VM storage volume",
            )?;
            volume = volume_info(uuid)?;
            validate_volume(&volume, name, uuid, kind)?;
        }
        ensure!(
            volume.volume_name == name.as_str()
                && volume.volume_uuid == uuid.as_str()
                && volume.mount_point.as_deref() == Some(mount_point),
            "VM storage volume {} is not mounted at {}",
            name,
            mount_point.display()
        );
        ensure!(volume.writable_volume, "VM storage volume is read-only");
        self.ensure_directory(path)
            .context("VM storage directory is unavailable after mounting its volume")?;
        Ok(())
    }

    fn volume_password_valid(&self, uuid: &str, password: &[u8]) -> Result<bool> {
        let output = self.volume_password_command(
            password,
            &["apfs", "unlockVolume", uuid, "-stdinpassphrase", "-verify"],
        )?;
        Ok(output.status.success())
    }

    fn run_volume_password(
        &self,
        uuid: &str,
        password: &[u8],
        arguments: &[&str],
        description: &str,
    ) -> Result<()> {
        check_output(
            &self.volume_password_command(password, arguments)?,
            description,
        )
        .with_context(|| format!("encrypted volume {uuid}"))
    }

    fn volume_password_command(&self, password: &[u8], arguments: &[&str]) -> Result<Output> {
        let mut command = Command::new("/usr/sbin/diskutil")
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run diskutil")?;
        command
            .stdin
            .take()
            .context("diskutil stdin is unavailable")?
            .write_all(password)?;
        command
            .wait_with_output()
            .context("cannot wait for diskutil")
    }

    fn volume_mounted(&self, storage: &VolumeStorage, kind: VolumeKind) -> Result<bool> {
        let VolumeStorage {
            mount_point,
            name,
            uuid,
            ..
        } = storage;
        let volume = volume_info(uuid)?;
        Ok(volume.volume_name == name.as_str()
            && volume.volume_uuid == uuid.as_str()
            && match kind {
                VolumeKind::Plain => !volume.file_vault,
                VolumeKind::Encrypted => volume.filesystem_type == "apfs" && volume.file_vault,
            }
            && !volume.locked
            && volume.mount_point.as_deref() == Some(mount_point))
    }

    fn ensure_storage(&self, access: StorageAccess) -> Result<()> {
        match &self.config.storage {
            Storage::Default => Ok(()),
            Storage::Directory { path } => self.ensure_directory(path),
            Storage::PlainVolume(volume) => self.ensure_volume(volume, VolumeKind::Plain, access),
            Storage::EncryptedVolume(volume) => {
                self.ensure_volume(volume, VolumeKind::Encrypted, access)
            }
        }
    }

    fn configure_tart_storage(&self, command: &mut Command) -> Result<()> {
        match &self.config.storage {
            Storage::Default => {
                command.env_remove("TART_HOME");
                Ok(())
            }
            Storage::Directory { path } => {
                self.ensure_directory(path)?;
                command.env("TART_HOME", path);
                Ok(())
            }
            Storage::PlainVolume(volume) => {
                self.ensure_volume(volume, VolumeKind::Plain, StorageAccess::Background)?;
                command.env("TART_HOME", &volume.path);
                Ok(())
            }
            Storage::EncryptedVolume(volume) => {
                self.ensure_volume(volume, VolumeKind::Encrypted, StorageAccess::Background)?;
                command.env("TART_HOME", &volume.path);
                Ok(())
            }
        }
    }

    fn tart(&self) -> Result<Command> {
        ensure!(
            is_executable(&self.paths.bin("tart")),
            "Tart is not installed"
        );
        let mut command = Command::new(self.paths.bin("tart"));
        self.configure_tart_storage(&mut command)?;
        Ok(command)
    }

    fn tart_text(&self, arguments: &[&str]) -> Result<String> {
        checked(self.tart()?.args(arguments), "run Tart")
    }

    fn vm_exists(&self) -> Result<bool> {
        Ok(self
            .tart_text(&["list", "--source", "local", "--quiet"])?
            .lines()
            .any(|line| line == self.config.vm_name))
    }

    fn vm_info(&self) -> Result<VmInfo> {
        serde_json::from_str(&self.tart_text(&["get", &self.config.vm_name, "--format", "json"])?)
            .context("Tart returned malformed VM information")
    }

    fn vm_ip(&self, wait: u32) -> Result<String> {
        self.tart_text(&[
            "ip",
            &self.config.vm_name,
            "--wait",
            &wait.to_string(),
            "--resolver",
            "arp",
        ])
    }

    fn verify_config(&self) -> Result<()> {
        let vm = self.vm_info()?;
        ensure!(
            vm.cpu == self.config.cpu_count,
            "VM CPU count differs from configuration"
        );
        ensure!(
            vm.memory == u64::from(self.config.memory_gb) * 1024,
            "VM memory differs from configuration"
        );
        ensure!(
            vm.disk == self.config.disk_gb,
            "VM disk differs from configuration"
        );
        ensure!(vm.os == "darwin", "VM operating system is not macOS");
        Ok(())
    }

    fn preflight_start(&self) -> Result<()> {
        ensure!(
            self.paths.service_plist.is_file(),
            "run '{}' first",
            self.paths.install_hint()
        );
        self.ensure_keychain(KeychainMode::Current)?;
        self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        self.ensure_storage(StorageAccess::Interactive)?;
        ensure!(
            self.vm_exists()?,
            "VM does not exist; rerun '{}'",
            self.paths.install_hint()
        );
        ensure!(
            self.paths.provisioned.exists(),
            "VM installation is incomplete; rerun '{}'",
            self.paths.install_hint()
        );
        self.verify_config()?;
        validate_bridge()?;
        Ok(())
    }

    fn start(&self) -> Result<()> {
        self.preflight_start()?;
        self.start_ready()
    }

    fn start_ready(&self) -> Result<()> {
        touch(&self.paths.run_marker)?;
        self.start_service()?;
        println!(
            "running: {}@{}",
            self.config.guest_user,
            self.wait_for_ssh(300)?
        );
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        self.preflight_start()?;
        self.stop()?;
        self.start_ready()
    }

    fn stop(&self) -> Result<()> {
        self.stop_vm()?;
        println!("stopped");
        Ok(())
    }

    fn stop_vm(&self) -> Result<()> {
        self.clear_keychain_helper()?;
        remove_if_present(&self.paths.run_marker)?;
        self.stop_tart()
    }

    fn stop_tart(&self) -> Result<()> {
        let storage_mounted = match &self.config.storage {
            Storage::Default => true,
            Storage::Directory { path } => path.is_dir(),
            Storage::PlainVolume(volume) => {
                volume.path.is_dir() && self.volume_mounted(volume, VolumeKind::Plain)?
            }
            Storage::EncryptedVolume(volume) => {
                volume.path.is_dir() && self.volume_mounted(volume, VolumeKind::Encrypted)?
            }
        };
        let manageable =
            is_executable(&self.paths.bin("tart")) && storage_mounted && self.vm_exists()?;
        let stop_error = if manageable {
            match self.vm_info()?.state {
                VmState::Running | VmState::Suspended => success(
                    self.tart()?
                        .args(["stop", &self.config.vm_name, "--timeout", "30"]),
                    "stop the VM",
                )
                .err(),
                VmState::Stopped => None,
            }
        } else {
            None
        };
        let target = self.paths.service_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the service")?;
        }
        if manageable {
            match self.vm_info()?.state {
                VmState::Stopped => {}
                VmState::Running | VmState::Suspended => {
                    if let Some(error) = stop_error {
                        return Err(error);
                    }
                    bail!("the VM did not stop");
                }
            }
        }
        Ok(())
    }

    fn prepare_console(&self, resume_background: bool) -> Result<()> {
        remove_if_present(&self.paths.run_marker)?;
        let source = match self.vm_info()?.state {
            VmState::Running => {
                println!("saving VM state; this can take several minutes...");
                success(
                    self.tart()?.args(["suspend", &self.config.vm_name]),
                    "suspend the VM",
                )?;
                let deadline = Instant::now() + Duration::from_secs(300);
                loop {
                    match self.vm_info()?.state {
                        VmState::Suspended => break,
                        VmState::Stopped => bail!("the VM stopped before Tart could suspend it"),
                        VmState::Running => ensure!(
                            Instant::now() < deadline,
                            "timed out waiting for the VM to suspend"
                        ),
                    }
                    thread::sleep(Duration::from_millis(500));
                }
                ConsoleSource::Snapshot
            }
            VmState::Suspended => ConsoleSource::Snapshot,
            VmState::Stopped if resume_background => {
                bail!("the background VM stopped before it could be suspended")
            }
            VmState::Stopped => ConsoleSource::ColdBoot,
        };
        let target = self.paths.service_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the service")?;
        }
        match (source, self.vm_info()?.state) {
            (ConsoleSource::Snapshot, VmState::Suspended)
            | (ConsoleSource::ColdBoot, VmState::Stopped) => Ok(()),
            (ConsoleSource::Snapshot, VmState::Running | VmState::Stopped) => {
                bail!("the VM snapshot was lost during the background handoff")
            }
            (ConsoleSource::ColdBoot, VmState::Running | VmState::Suspended) => {
                bail!("the VM changed state during the console handoff")
            }
        }
    }

    fn suspend_console_process(
        &self,
        child: &mut std::process::Child,
    ) -> Result<std::process::ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            let sent = match self.tart() {
                Ok(mut suspend) => suspend
                    .process_group(0)
                    .args(["suspend", &self.config.vm_name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success()),
                Err(_) => false,
            };
            if sent {
                break;
            }
            if Instant::now() >= deadline {
                bail!("timed out asking Tart to suspend the console VM; Tart was left running");
            }
            thread::sleep(Duration::from_millis(250));
        }
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for the console VM to suspend; Tart was left running");
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    fn wait_for_ssh(&self, ip_wait: u32) -> Result<String> {
        let ip = self.vm_ip(ip_wait)?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let mut probe = self.ssh_command(&ip);
            probe
                .args(["-o", "ConnectTimeout=5", "-o", "ConnectionAttempts=1"])
                .arg("/usr/bin/true")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if probe.status()?.success() {
                return Ok(ip);
            }
            ensure!(
                Instant::now() < deadline,
                "SSH did not become ready at {ip}"
            );
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn ssh_command(&self, ip: &str) -> Command {
        let mut command = Command::new("/usr/bin/ssh");
        command
            .arg("-i")
            .arg(&self.paths.ssh_key)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
            ])
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile={}",
                self.paths.config_dir.join("known_hosts").display()
            ))
            .arg(format!("{}@{ip}", self.config.guest_user));
        command
    }

    fn ssh(&self, arguments: &[OsString]) -> Result<()> {
        let ip = self.wait_for_ssh(120)?;
        let mut command = self.ssh_command(&ip);
        command.args(arguments);
        Err(anyhow!("cannot execute ssh: {}", command.exec()))
    }

    fn tailscale(&self, action: TailscaleAction) -> Result<()> {
        self.ensure_storage(StorageAccess::Background)?;
        ensure!(
            self.vm_exists()?,
            "VM does not exist; rerun '{}'",
            self.paths.install_hint()
        );
        ensure!(
            self.paths.provisioned.exists(),
            "VM installation is incomplete; rerun '{}'",
            self.paths.install_hint()
        );
        match self.vm_info()?.state {
            VmState::Running => {}
            VmState::Suspended | VmState::Stopped => {
                bail!(
                    "VM is not running; run '{} start'",
                    self.paths.command_name()
                )
            }
        }
        let ip = self.wait_for_ssh(120)?;
        match action {
            TailscaleAction::Setup => self.setup_tailscale(&ip)?,
            TailscaleAction::Status => {}
        }
        self.print_tailscale_status(&ip)
    }

    fn setup_tailscale(&self, ip: &str) -> Result<()> {
        ensure!(
            self.paths.guest_tailscale.is_file(),
            "the packaged Tailscale binary is missing"
        );
        let upload = format!("/Users/{}/.gremvm-tailscaled", self.config.guest_user);
        let output = self
            .ssh_command(ip)
            .arg(format!(
                "umask 077; /bin/cat > {upload} && /bin/chmod 0700 {upload}"
            ))
            .stdin(Stdio::from(fs::File::open(&self.paths.guest_tailscale)?))
            .output()
            .context("cannot upload Tailscale to the guest")?;
        check_output(&output, "upload Tailscale to the guest")?;

        let checksum = checked(
            Command::new("/usr/bin/shasum")
                .args(["-a", "256"])
                .arg(&self.paths.guest_tailscale),
            "hash the packaged Tailscale binary",
        )?
        .split_whitespace()
        .next()
        .context("shasum returned no digest")?
        .to_owned();
        ensure!(
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "shasum returned an invalid digest"
        );
        let password = self.guest_password()?;
        let mut install = self
            .ssh_command(ip)
            .arg(format!(
                concat!(
                    "/usr/bin/sudo -S -k -p '' /bin/sh -c '",
                    "stage=$(/usr/bin/mktemp /private/var/tmp/gremvm-tailscaled.XXXXXX) ",
                    "|| exit 1; ",
                    "/bin/cat {} > \"$stage\" && ",
                    "/bin/chmod 0700 \"$stage\" && ",
                    "test \"$(/usr/bin/shasum -a 256 \"$stage\" | ",
                    "/usr/bin/cut -d \" \" -f 1)\" = {} && ",
                    "\"$stage\" install-system-daemon && ",
                    "/bin/ln -sfn tailscaled /usr/local/bin/tailscale && ",
                    "/bin/launchctl unload ",
                    "/Library/LaunchDaemons/com.tailscale.tailscaled.plist && ",
                    "/usr/bin/plutil -insert KeepAlive -bool true ",
                    "/Library/LaunchDaemons/com.tailscale.tailscaled.plist && ",
                    "/bin/launchctl load ",
                    "/Library/LaunchDaemons/com.tailscale.tailscaled.plist; ",
                    "result=$?; /bin/rm -f \"$stage\"; exit $result",
                    "'; result=$?; /bin/rm -f {}; exit $result"
                ),
                upload, checksum, upload
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot install Tailscale in the guest")?;
        writeln!(
            install
                .stdin
                .take()
                .context("guest sudo stdin is unavailable")?,
            "{password}"
        )?;
        check_output(
            &install.wait_with_output()?,
            "install Tailscale in the guest",
        )?;

        success(
            self.ssh_command(ip).arg(concat!(
                "attempt=0; while [ \"$attempt\" -lt 10 ]; do ",
                "/usr/local/bin/tailscale status --json --peers=false ",
                ">/dev/null 2>&1 && exit 0; ",
                "attempt=$((attempt + 1)); /bin/sleep 1; done; exit 1"
            )),
            "wait for Tailscale in the guest",
        )?;
        println!("authenticate using the URL below if Tailscale asks...");
        let status = self
            .ssh_command(ip)
            .arg(format!(
                "/usr/local/bin/tailscale up --accept-dns=false --hostname={} --operator={}",
                self.config.vm_name, self.config.guest_user
            ))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("cannot connect Tailscale in the guest")?;
        ensure!(status.success(), "failed to connect Tailscale in the guest");
        Ok(())
    }

    fn print_tailscale_status(&self, ip: &str) -> Result<()> {
        match self.tailscale_state(ip)? {
            TailscaleState::NotInstalled => {
                println!("tailscale: not-installed");
                println!("next: {} tailscale setup", self.paths.command_name());
            }
            TailscaleState::Disconnected => {
                println!("tailscale: disconnected");
                println!("next: {} tailscale setup", self.paths.command_name());
            }
            TailscaleState::Connected { ip } => {
                println!("tailscale: connected");
                println!("tailscale-ip: {ip}");
                println!("ssh: ssh {}@{ip}", self.config.guest_user);
                println!("screen-sharing: vnc://{ip}");
            }
        }
        Ok(())
    }

    fn tailscale_state(&self, ip: &str) -> Result<TailscaleState> {
        let state = checked(
            self.ssh_command(ip).arg(concat!(
                "if [ ! -x /usr/local/bin/tailscale ]; then ",
                "printf 'not-installed\\n'; ",
                "elif /usr/local/bin/tailscale wait --timeout=1s >/dev/null 2>&1 && ",
                "address=$(/usr/local/bin/tailscale ip -4 2>/dev/null); then ",
                "printf 'connected\\n%s\\n' \"$address\"; ",
                "else printf 'disconnected\\n'; fi"
            )),
            "inspect Tailscale in the guest",
        )?;
        let mut lines = state.lines();
        let parsed = match lines.next() {
            Some("not-installed") => TailscaleState::NotInstalled,
            Some("disconnected") => TailscaleState::Disconnected,
            Some("connected") => {
                let ip = lines
                    .next()
                    .context("Tailscale did not return an IPv4 address")?;
                ip.parse::<std::net::Ipv4Addr>()
                    .context("Tailscale returned an invalid IPv4 address")?;
                TailscaleState::Connected { ip: ip.to_owned() }
            }
            _ => bail!("Tailscale returned an unknown state"),
        };
        ensure!(lines.next().is_none(), "Tailscale returned malformed state");
        Ok(parsed)
    }

    fn screen_share(&self) -> Result<()> {
        self.ensure_storage(StorageAccess::Background)?;
        ensure!(
            self.vm_exists()?,
            "VM does not exist; rerun '{}'",
            self.paths.install_hint()
        );
        ensure!(
            self.paths.provisioned.exists(),
            "VM installation is incomplete; rerun '{}'",
            self.paths.install_hint()
        );
        match self.vm_info()?.state {
            VmState::Running => {}
            VmState::Suspended | VmState::Stopped => {
                bail!(
                    "VM is not running; run '{} start'",
                    self.paths.command_name()
                )
            }
        }
        let ip = self.vm_ip(120)?;
        ensure_graphical_session(self.paths.command_name(), "screen-share")?;
        success(
            Command::new("/usr/bin/open").arg(format!("vnc://{ip}")),
            "open guest Screen Sharing",
        )?;
        println!("screen sharing: {}@{ip}", self.config.guest_user);
        Ok(())
    }

    fn console(&self) -> Result<()> {
        ensure_console_session(self.paths.command_name())?;
        self.clear_keychain_helper()?;
        self.ensure_keychain(KeychainMode::Current)?;
        let resume_background =
            self.paths.run_marker.exists() && self.service_loaded(&self.paths.service_target())?;
        if resume_background {
            self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        }
        self.ensure_storage(StorageAccess::Interactive)?;
        ensure!(self.vm_exists()?, "VM does not exist");
        validate_bridge()?;
        if resume_background {
            match self.vm_info()?.state {
                VmState::Running => {}
                VmState::Suspended | VmState::Stopped => {
                    println!("starting the background VM before opening the console...");
                    self.start_service()?;
                    self.wait_for_ssh(300)?;
                }
            }
        }
        let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
        let console = (|| {
            ensure_console_session(self.paths.command_name())?;
            self.prepare_console(resume_background)?;
            ensure_console_session(self.paths.command_name())?;
            ensure!(
                signals.pending().next().is_none(),
                "console launch was interrupted"
            );
            println!("opening Tart recovery console; this command stays active until it closes...");
            let mut run = Command::new("/usr/bin/caffeinate");
            self.configure_tart_storage(&mut run)?;
            run.process_group(0).args([
                "-disu",
                self.paths
                    .bin("tart")
                    .to_str()
                    .context("Tart path is not UTF-8")?,
                "run",
                "--suspendable",
                &format!("--net-bridged={BRIDGE}"),
                &self.config.vm_name,
            ]);
            let mut child = run.spawn()?;
            thread::sleep(Duration::from_millis(250));
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if signals.pending().next().is_some() {
                    ensure_console_session(self.paths.command_name())?;
                    break self.suspend_console_process(&mut child)?;
                }
                thread::sleep(Duration::from_millis(100));
            };
            ensure!(status.success(), "Tart exited with {status}");
            ensure!(
                matches!(self.vm_info()?.state, VmState::Suspended),
                "the console VM exited without a saved state"
            );
            Ok(())
        })();
        drop(signals);
        if let Err(error) = console {
            return if resume_background {
                Err(error.context(
                    format!(
                        "background restart was withheld to avoid a cold boot; inspect the VM, then run '{} start' when ready",
                        self.paths.command_name()
                    ),
                ))
            } else {
                Err(error)
            };
        }
        if resume_background {
            println!("console closed; restoring the background VM...");
            touch(&self.paths.run_marker)?;
            self.start_service()?;
            println!(
                "running: {}@{}",
                self.config.guest_user,
                self.wait_for_ssh(300)?
            );
        }
        Ok(())
    }

    fn status(&self) -> Result<()> {
        if !self.vm_exists()? {
            println!("state: incomplete");
            return Ok(());
        }
        let vm = self.vm_info()?;
        let ip = match vm.state {
            VmState::Running => self.vm_ip(0).ok(),
            VmState::Suspended | VmState::Stopped => None,
        };
        let supervised =
            self.paths.run_marker.exists() && self.service_loaded(&self.paths.service_target())?;
        let state = match (vm.state, ip.as_deref(), supervised) {
            (_, _, _) if !self.paths.provisioned.exists() => "incomplete",
            (VmState::Running, Some(_), _) => "running",
            (VmState::Running, None, _) => "running-address-unknown",
            (VmState::Suspended, _, true) => "starting",
            (VmState::Suspended, _, false) => "suspended",
            (VmState::Stopped, _, true) => "starting",
            (VmState::Stopped, _, false) => "stopped",
        };
        println!("state: {state}");
        if let Some(ip) = ip {
            println!("ip: {ip}");
        }
        println!("name: {}", self.config.vm_name);
        println!("cpu: {}", vm.cpu);
        println!("memory-gb: {}", vm.memory / 1024);
        println!("disk-gb: {}", vm.disk);
        println!("network: bridged:{BRIDGE}");
        Ok(())
    }

    fn logs(&self, follow: bool) -> Result<()> {
        let mut command = Command::new("/usr/bin/tail");
        command.args(["-n", "200"]);
        if follow {
            command.arg("-F");
        }
        command
            .arg(self.paths.logs.join("vm.log"))
            .arg(self.paths.logs.join("vm.error.log"));
        Err(anyhow!("cannot execute tail: {}", command.exec()))
    }

    fn uninstall(&self) -> Result<()> {
        self.ensure_storage(StorageAccess::Interactive)?;
        self.stop_vm()?;
        self.delete_vm()?;
        remove_if_present(&self.paths.service_plist)?;
        remove_if_present(&self.paths.autoload_agent())?;
        if fs::symlink_metadata(&self.paths.command_link)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
            && fs::read_link(&self.paths.command_link)? == self.paths.bin("gremvm")
        {
            remove_if_present(&self.paths.command_link)?;
        }
        remove_if_present(&self.paths.runtime)?;
        println!("uninstalled: {}", self.config.vm_name);
        Ok(())
    }

    fn internal_run(&self) -> Result<()> {
        if !self.paths.run_marker.exists() || !self.paths.provisioned.exists() {
            return Ok(());
        }
        self.ensure_keychain(KeychainMode::Background)?;
        self.ensure_storage(StorageAccess::Background)?;
        if !self.vm_exists()? {
            return Ok(());
        }
        validate_bridge()?;
        self.verify_config()?;
        let mut command = self.tart()?;
        command.args([
            "run",
            "--no-graphics",
            "--suspendable",
            &format!("--net-bridged={BRIDGE}"),
            &self.config.vm_name,
        ]);
        Err(anyhow!("cannot execute Tart: {}", command.exec()))
    }
}

fn valid_name(name: &str) -> std::result::Result<String, String> {
    let valid = name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    valid
        .then(|| name.to_owned())
        .ok_or_else(|| "name must be 1-64 letters, numbers, dots, underscores, or hyphens and start with a letter or number".into())
}

fn default_guest_user() -> String {
    "admin".into()
}

fn valid_user(name: &str) -> std::result::Result<String, String> {
    let reserved = ["daemon", "guest", "nobody", "root"];
    let valid = name.len() <= 32
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
        })
        && !reserved.contains(&name);
    valid
        .then(|| name.to_owned())
        .ok_or_else(|| {
            "guest user must be 1-32 lowercase letters, numbers, underscores, or hyphens, start with a letter, and not be a reserved account".into()
        })
}

fn existing_directory(value: &str) -> std::result::Result<PathBuf, String> {
    canonical_directory(Path::new(value)).map_err(|error| error.to_string())
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "storage directory must be absolute");
    ensure!(
        path.is_dir(),
        "storage directory does not exist: {}",
        path.display()
    );
    let canonical = path.canonicalize()?;
    NamedTempFile::new_in(&canonical)
        .and_then(NamedTempFile::close)
        .with_context(|| format!("storage directory is not writable: {}", canonical.display()))?;
    Ok(canonical)
}

fn prompt_password(prompt: &str) -> Result<String> {
    let password = rpassword::prompt_password(prompt).context("cannot read hidden password")?;
    ensure!(!password.is_empty(), "password must not be empty");
    ensure!(
        !password.chars().any(char::is_control),
        "password must not contain control characters"
    );
    Ok(password)
}

fn prompt_guest_password() -> Result<String> {
    let password = prompt_password("guest password: ")?;
    validate_guest_password(&password).map_err(anyhow::Error::msg)?;
    ensure!(
        password == prompt_password("confirm guest password: ")?,
        "guest passwords do not match"
    );
    Ok(password)
}

fn validate_guest_password(password: &str) -> std::result::Result<(), String> {
    (8..=128)
        .contains(&password.chars().count())
        .then_some(())
        .ok_or_else(|| "guest password must be 8-128 characters".into())
}

fn private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(path.is_dir(), "not a directory: {}", path.display());
        return Ok(());
    }
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    Ok(())
}

fn management_lock(paths: &Paths) -> Result<fs::File> {
    private_dir(&paths.state)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.state.join("management.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another {} management command is running",
        paths.command_name()
    );
    Ok(lock)
}

fn touch(path: &Path) -> Result<()> {
    private_dir(path.parent().context("marker has no parent directory")?)?;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    Ok(())
}

fn checked(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("cannot {description}"))?;
    check_output(&output, description)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn login_keychain(paths: &Paths) -> PathBuf {
    paths.home.join("Library/Keychains/login.keychain-db")
}

fn interactive_terminal() -> Result<String> {
    ensure!(
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        "the host login Keychain is locked; rerun from an interactive terminal"
    );
    checked(
        Command::new("/usr/bin/tty").stdin(Stdio::inherit()),
        "locate the interactive terminal",
    )
}

fn keychain_unlocked(paths: &Paths) -> Result<bool> {
    Ok(Command::new("/usr/bin/security")
        .arg("show-keychain-info")
        .arg(login_keychain(paths))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("cannot inspect the host login Keychain")?
        .success())
}

fn unlock_keychain(paths: &Paths, terminal: &str) -> Result<bool> {
    let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
    let _input = HiddenInput::new(terminal)?;
    let mut security = Command::new("/usr/bin/security")
        .arg("unlock-keychain")
        .arg(login_keychain(paths))
        .spawn()
        .context("cannot unlock the host login Keychain")?;
    loop {
        if signals.pending().next().is_some() {
            let _ = security.kill();
            let _ = security.wait();
            return Ok(false);
        }
        if let Some(status) = security.try_wait()? {
            return Ok(status.success() && keychain_unlocked(paths)?);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn internal_keychain(paths: &Paths, mode: KeychainHelperMode) -> Result<()> {
    let unlocked = match mode {
        KeychainHelperMode::Check => keychain_unlocked(paths)?,
        KeychainHelperMode::Unlock => unlock_keychain(paths, &interactive_terminal()?)?,
    };
    private_dir(&paths.state)?;
    let mut result = NamedTempFile::new_in(&paths.state)?;
    result.write_all(if unlocked { b"unlocked\n" } else { b"locked\n" })?;
    result.flush()?;
    result.persist(paths.state.join("keychain.result"))?;
    Ok(())
}

fn volume_info(selector: &str) -> Result<VolumeInfo> {
    let output = Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", selector])
        .output()
        .context("cannot inspect the VM storage volume")?;
    check_output(&output, "inspect the VM storage volume")?;
    plist::from_bytes(&output.stdout).context("diskutil returned malformed volume information")
}

fn volume_for_path(path: &Path) -> Result<VolumeInfo> {
    let output = checked(
        Command::new("/bin/df").arg("-P").arg(path),
        "inspect the storage directory filesystem",
    )?;
    let device = output
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().next())
        .context("df returned malformed filesystem information")?;
    volume_info(device)
}

fn validate_volume_reference(
    path: &Path,
    mount_point: &Path,
    name: &str,
    uuid: &str,
) -> Result<()> {
    ensure!(path.is_absolute(), "storage directory must be absolute");
    ensure!(
        mount_point.is_absolute() && mount_point != Path::new("/"),
        "invalid VM storage mount point"
    );
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains('/')
            && !name.chars().any(char::is_control),
        "invalid VM storage volume name"
    );
    ensure!(
        uuid.len() == 36
            && uuid.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            }),
        "invalid VM storage volume UUID"
    );
    ensure!(
        path == mount_point || path.starts_with(mount_point),
        "VM storage directory is outside its recorded volume: {}",
        path.display()
    );
    Ok(())
}

fn validate_volume(volume: &VolumeInfo, name: &str, uuid: &str, kind: VolumeKind) -> Result<()> {
    ensure!(
        volume.volume_name == name && volume.volume_uuid == uuid,
        "diskutil resolved the wrong VM storage volume"
    );
    match kind {
        VolumeKind::Plain => ensure!(
            !volume.file_vault,
            "VM storage volume unexpectedly became encrypted"
        ),
        VolumeKind::Encrypted => ensure!(
            volume.filesystem_type == "apfs" && volume.file_vault,
            "VM storage volume must remain encrypted APFS"
        ),
    }
    Ok(())
}

fn success(command: &mut Command, description: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot {description}"))?;
    check_output(&output, description)
}

fn check_output(output: &Output, description: &str) -> Result<()> {
    ensure!(
        output.status.success(),
        "failed to {description}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn sysctl(name: &str) -> Result<u64> {
    checked(
        Command::new("/usr/sbin/sysctl").args(["-n", name]),
        "run sysctl",
    )?
    .parse()
    .context("sysctl returned a non-number")
}

fn validate_bridge() -> Result<()> {
    ensure!(
        Command::new("/sbin/ifconfig")
            .arg(BRIDGE)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success(),
        "required bridged interface does not exist: {BRIDGE}"
    );
    Ok(())
}

fn user_domain() -> String {
    format!("user/{}", unsafe { libc::getuid() })
}

fn launchd_session() -> Result<String> {
    checked(
        Command::new("/bin/launchctl").arg("managername"),
        "inspect the launchd session",
    )
}

fn ensure_graphical_session(name: &str, command: &str) -> Result<()> {
    ensure!(
        launchd_session()? == "Aqua",
        "{name} {command} requires an active graphical host session"
    );
    Ok(())
}

fn ensure_console_session(name: &str) -> Result<()> {
    ensure_graphical_session(name, "console")?;
    let output = Command::new("/usr/sbin/ioreg")
        .args(["-n", "Root", "-d1", "-a"])
        .output()
        .context("cannot inspect the graphical host session")?;
    check_output(&output, "inspect the graphical host session")?;
    let console: ConsoleInfo = plist::from_bytes(&output.stdout)
        .context("ioreg returned malformed graphical session information")?;
    let uid = unsafe { libc::getuid() };
    let session = console
        .users
        .into_iter()
        .find(|session| session.user_id == uid && session.on_console)
        .context("the current user has no on-console graphical host session")?;
    ensure!(
        !console.locked && session.login_done && !session.screen_locked,
        "{name} console requires this user's on-console graphical session to be unlocked so macOS can encrypt the VM snapshot"
    );
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
