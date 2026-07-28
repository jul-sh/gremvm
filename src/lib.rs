use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use clap::{Args, Parser, ValueEnum};
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
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

const LABEL: &str = "io.gremvm";
const KEYCHAIN_HELPER_LABEL: &str = "io.gremvm.keychain";
const PASSWORD_SERVICE: &str = "io.gremvm.guest-password";
const VOLUME_PASSWORD_SERVICE: &str = "io.gremvm.volume-password";
const BRIDGE: &str = "en0";
const DISPLAY: &str = "1512x982";
const IPSW_NAME: &str = "UniversalMac_26.6_25G72_Restore.ipsw";
const IPSW_URL: &str = "https://updates.cdn-apple.com/2026SummerFCS/fullrestores/140-65618/10445B26-DE2C-43EC-9149-0A831602E74B/UniversalMac_26.6_25G72_Restore.ipsw";
const IPSW_SHA256: &str = "a8d59bdec11a16f704c1a41edc86461c77e71c790ac834a05266b9670287142c";
const IPSW_SIZE: u64 = 19_772_077_142;

#[derive(Parser)]
#[command(
    name = "gremvm",
    version,
    about = "Manage one persistent Lume macOS VM",
    arg_required_else_help = true,
    after_help = "The guest always uses bridged networking on en0."
)]
enum Action {
    /// Install the pinned runtime and background service.
    Install(InstallOptions),
    /// Create and start the VM.
    Provision,
    /// Show VM state.
    Status,
    /// Start the VM and wait for its desktop.
    Start,
    /// Stop the VM and disable automatic restart.
    Stop,
    /// Stop and start the VM.
    Restart,
    /// Connect as the admin guest user.
    Ssh {
        #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
    /// Open the guest in macOS Screen Sharing.
    ScreenShare {
        /// Print the connection URL for use on another Mac.
        #[arg(long)]
        url: bool,
    },
    /// Open Lume's local recovery console.
    Console,
    /// Show VM logs.
    Logs {
        #[arg(long)]
        follow: bool,
    },
    /// Remove host integration while preserving VM data.
    Uninstall,
    #[command(name = "internal-run", hide = true)]
    InternalRun,
    #[command(name = "internal-keychain", hide = true)]
    InternalKeychain {
        #[arg(value_enum)]
        mode: KeychainHelperMode,
    },
}

#[derive(Args)]
struct InstallOptions {
    /// Name of the Lume VM.
    #[arg(long, default_value = "gremvm", value_parser = valid_name)]
    vm_name: String,
    /// Number of virtual CPUs.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..=64))]
    cpu_count: u32,
    /// Guest memory in GiB.
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u32).range(4..=256))]
    memory_gb: u32,
    /// Maximum virtual disk size in GiB.
    #[arg(long, default_value_t = 192, value_parser = clap::value_parser!(u32).range(50..=350))]
    disk_gb: u32,
    /// Encrypted APFS volume containing the VM.
    #[arg(long, value_parser = valid_name)]
    volume_name: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    vm_name: String,
    cpu_count: u32,
    memory_gb: u32,
    disk_gb: u32,
    storage: Storage,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Storage {
    Default,
    Volume { name: String, uuid: String },
}

impl InstallOptions {
    fn resolve(self) -> Result<Config> {
        let storage = match self.volume_name {
            None => Storage::Default,
            Some(name) => {
                let volume = volume_info(&name)?;
                ensure!(
                    volume.volume_name == name,
                    "diskutil resolved the wrong VM storage volume"
                );
                ensure!(
                    volume.filesystem_type == "apfs" && volume.file_vault,
                    "VM storage volume must be encrypted APFS"
                );
                Storage::Volume {
                    name,
                    uuid: volume.volume_uuid,
                }
            }
        };
        Ok(Config {
            vm_name: self.vm_name,
            cpu_count: self.cpu_count,
            memory_gb: self.memory_gb,
            disk_gb: self.disk_gb,
            storage,
        })
    }
}

impl Config {
    fn load(paths: &Paths) -> Result<Self> {
        let config: Self = serde_json::from_reader(fs::File::open(&paths.config_file)?)?;
        valid_name(&config.vm_name).map_err(anyhow::Error::msg)?;
        if let Storage::Volume { name, uuid } = &config.storage {
            valid_name(name).map_err(anyhow::Error::msg)?;
            valid_name(uuid).map_err(anyhow::Error::msg)?;
        }
        ensure!((1..=64).contains(&config.cpu_count), "invalid CPU count");
        ensure!((4..=256).contains(&config.memory_gb), "invalid memory size");
        ensure!((50..=350).contains(&config.disk_gb), "invalid disk size");
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
        write_json(&paths.config_file, self)
    }
}

struct Paths {
    home: PathBuf,
    root: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
    run_marker: PathBuf,
    provisioned: PathBuf,
    desktop: PathBuf,
    launch_agent: PathBuf,
    command_link: PathBuf,
    ssh_key: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let root = home.join("Library/Application Support/GremVM");
        let config_dir = root.join("config");
        let state = root.join("state");
        Ok(Self {
            command_link: home.join(".local/bin/gremvm"),
            launch_agent: home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            runtime: root.join("runtime"),
            run_marker: state.join("run"),
            provisioned: state.join("provisioned"),
            desktop: state.join("desktop.json"),
            ssh_key: config_dir.join("id_ed25519"),
            logs: root.join("logs"),
            home,
            root,
            config_file: config_dir.join("config.json"),
            config_dir,
            state,
        })
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.runtime.join("bin").join(name)
    }

    fn guest_setup(&self) -> PathBuf {
        self.runtime.join("share/gremvm/guest-setup.sh")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LumeInfoDto {
    name: String,
    os: String,
    cpu_count: u32,
    memory_size: u64,
    disk_size: DiskSizeDto,
    display: String,
    status: String,
    provisioning_operation: Option<String>,
    vnc_url: Option<String>,
    ip_address: Option<String>,
    network_mode: Option<String>,
}

#[derive(Deserialize)]
struct DiskSizeDto {
    allocated: u64,
    total: u64,
}

struct VmInfo {
    name: String,
    os: String,
    cpu_count: u32,
    memory_size: u64,
    disk_allocated: u64,
    disk_total: u64,
    display: String,
    network_mode: Option<String>,
    state: VmState,
}

enum VmState {
    Running {
        ip: Option<String>,
        vnc_url: Option<String>,
    },
    Stopped,
    Provisioning {
        operation: Option<String>,
    },
}

impl TryFrom<LumeInfoDto> for VmInfo {
    type Error = anyhow::Error;

    fn try_from(value: LumeInfoDto) -> Result<Self> {
        let state = match value.status.as_str() {
            "running" => VmState::Running {
                ip: value.ip_address,
                vnc_url: value.vnc_url,
            },
            "stopped" => VmState::Stopped,
            status if status.starts_with("provisioning") => VmState::Provisioning {
                operation: value.provisioning_operation,
            },
            status => bail!("Lume returned an unknown VM state: {status}"),
        };
        Ok(Self {
            name: value.name,
            os: value.os,
            cpu_count: value.cpu_count,
            memory_size: value.memory_size,
            disk_allocated: value.disk_size.allocated,
            disk_total: value.disk_size.total,
            display: value.display,
            network_mode: value.network_mode,
            state,
        })
    }
}

#[derive(Clone, Copy)]
enum VmInstallation {
    Absent,
    Incomplete,
    Provisioning,
    Unconfigured,
    Ready,
}

#[derive(Clone, Copy)]
enum KeyPolicy {
    Create,
    Existing,
}

#[derive(Clone, Copy)]
enum StorageAccess {
    Interactive,
    Background,
}

#[derive(Clone, Copy)]
enum KeychainAccess {
    Current,
    BackgroundInteractive,
    Background,
}

enum KeychainRequest {
    Check,
    Unlock { terminal: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum KeychainHelperMode {
    Check,
    Unlock,
}

enum RuntimeOwner {
    Supervisor(Child),
    External,
}

enum VolumeState {
    Unavailable,
    Unmounted,
    Mounted,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Desktop {
    ip: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VolumeInfo {
    file_vault: bool,
    filesystem_type: String,
    mount_point: Option<PathBuf>,
    volume_name: String,
    #[serde(rename = "VolumeUUID")]
    volume_uuid: String,
    writable_volume: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct AgentPlist {
    label: &'static str,
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
    label: &'static str,
    program_arguments: Vec<String>,
    environment_variables: BTreeMap<&'static str, String>,
    limit_load_to_session_type: &'static str,
    process_type: &'static str,
    run_at_load: bool,
    standard_in_path: String,
    standard_out_path: String,
    standard_error_path: String,
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

pub fn run() -> Result<()> {
    let command = Action::parse();
    let paths = Paths::discover()?;
    let _lock = match &command {
        Action::Install(_)
        | Action::Provision
        | Action::Start
        | Action::Stop
        | Action::Restart
        | Action::Uninstall => Some(management_lock(&paths)?),
        _ => None,
    };
    match command {
        Action::Install(options) => App {
            config: options.resolve()?,
            paths,
        }
        .install(),
        Action::InternalKeychain { mode } => internal_keychain(&paths, mode),
        Action::Status if !is_executable(&paths.bin("lume")) => {
            println!("state: not-installed");
            Ok(())
        }
        command => {
            ensure!(
                paths.config_file.is_file(),
                "configuration is missing; run 'gremvm install'"
            );
            App {
                config: Config::load(&paths).context("persisted configuration is invalid")?,
                paths,
            }
            .dispatch(command)
        }
    }
}

impl App {
    fn dispatch(&self, command: Action) -> Result<()> {
        match command {
            Action::Install(_) => unreachable!(),
            Action::Provision => self.provision(),
            Action::Status => self.status(),
            Action::Start => self.start(),
            Action::Stop => self.stop(),
            Action::Restart => self.restart(),
            Action::Ssh { command } => self.ssh(&command),
            Action::ScreenShare { url } => self.screen_share(url),
            Action::Console => self.console(),
            Action::Logs { follow } => self.logs(follow),
            Action::Uninstall => self.uninstall(),
            Action::InternalRun => self.internal_run(),
            Action::InternalKeychain { .. } => unreachable!(),
        }
    }
    fn install(&self) -> Result<()> {
        self.cleanup_keychain_helper()?;
        self.config.check_existing(&self.paths)?;
        self.ensure_keychain(KeychainAccess::Current)?;
        self.ensure_storage(StorageAccess::Interactive)?;
        self.validate_host()?;
        self.install_runtime()?;
        self.install_command()?;

        let installation = self.vm_installation();
        let restart = match installation {
            VmInstallation::Absent => {
                remove_if_present(&self.paths.run_marker)?;
                remove_if_present(&self.paths.provisioned)?;
                remove_if_present(&self.paths.config_dir.join("known_hosts"))?;
                self.install_ssh_key(KeyPolicy::Create)?;
                self.guest_password(KeyPolicy::Create)?;
                false
            }
            VmInstallation::Ready => {
                self.install_ssh_key(KeyPolicy::Existing)?;
                self.guest_password(KeyPolicy::Existing)?;
                self.verify_config()?;
                self.paths.run_marker.exists()
            }
            VmInstallation::Unconfigured => {
                self.install_ssh_key(KeyPolicy::Existing)?;
                self.guest_password(KeyPolicy::Existing)?;
                self.verify_config()?;
                remove_if_present(&self.paths.run_marker)?;
                false
            }
            VmInstallation::Provisioning | VmInstallation::Incomplete => {
                remove_if_present(&self.paths.run_marker)?;
                bail!(
                    "VM provisioning is incomplete at {}; GremVM will not delete it",
                    self.vm_dir().display()
                )
            }
        };

        if restart {
            self.ensure_keychain(KeychainAccess::BackgroundInteractive)?;
        }
        self.config.persist(&self.paths)?;
        self.install_service()?;
        if restart {
            self.start_service()?;
        }

        println!("installed: {}", self.paths.command_link.display());
        match installation {
            VmInstallation::Ready => println!("VM: {}", self.config.vm_name),
            VmInstallation::Absent | VmInstallation::Unconfigured => {
                println!("next: gremvm provision")
            }
            VmInstallation::Provisioning | VmInstallation::Incomplete => unreachable!(),
        }
        Ok(())
    }

    fn validate_host(&self) -> Result<()> {
        ensure!(
            std::env::consts::ARCH == "aarch64",
            "Lume requires Apple silicon"
        );
        let version = checked(
            Command::new("/usr/bin/sw_vers").arg("-productVersion"),
            "inspect the macOS version",
        )?;
        let major: u32 = version
            .split('.')
            .next()
            .context("macOS returned an empty version")?
            .parse()
            .context("macOS returned an invalid version")?;
        ensure!(major >= 14, "Lume requires macOS 14 or newer");
        ensure!(
            u64::from(self.config.cpu_count) <= sysctl("hw.logicalcpu")?,
            "requested CPU count exceeds the host"
        );
        ensure!(
            u64::from(self.config.memory_gb) * gib() < sysctl("hw.memsize")?,
            "requested memory exceeds the host"
        );
        validate_bridge()
    }

    fn install_runtime(&self) -> Result<()> {
        private_dir(&self.paths.root)?;
        if let Ok(metadata) = fs::symlink_metadata(&self.paths.runtime) {
            ensure!(
                metadata.file_type().is_symlink(),
                "refusing to replace runtime path: {}",
                self.paths.runtime.display()
            );
        }
        let executable = std::env::current_exe()?.canonicalize()?;
        let bundle = executable
            .parent()
            .and_then(Path::parent)
            .context("cannot locate the packaged GremVM bundle")?;
        for file in ["bin/gremvm", "bin/lume", "share/gremvm/guest-setup.sh"] {
            ensure!(
                bundle.join(file).is_file(),
                "packaged file is missing: {file}"
            );
        }
        ensure!(
            bundle.join("Applications/lume.app").is_dir(),
            "packaged Lume app is missing"
        );
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

    fn install_ssh_key(&self, policy: KeyPolicy) -> Result<()> {
        private_dir(&self.paths.config_dir)?;
        let public = self.paths.ssh_key.with_extension("pub");
        match (self.paths.ssh_key.exists(), public.exists()) {
            (false, false) => match policy {
                KeyPolicy::Create => success(
                    Command::new("/usr/bin/ssh-keygen")
                        .args(["-q", "-t", "ed25519", "-N", "", "-C"])
                        .arg(format!("gremvm@{}", self.config.vm_name))
                        .arg("-f")
                        .arg(&self.paths.ssh_key),
                    "generate the SSH key",
                )?,
                KeyPolicy::Existing => {
                    bail!("the SSH key is missing and cannot be regenerated for an existing VM")
                }
            },
            (true, true) => {}
            _ => bail!("SSH key is incomplete"),
        }
        fs::set_permissions(&self.paths.ssh_key, fs::Permissions::from_mode(0o600))?;
        fs::set_permissions(public, fs::Permissions::from_mode(0o644))?;
        Ok(())
    }

    fn install_service(&self) -> Result<()> {
        let parent = self
            .paths
            .launch_agent
            .parent()
            .context("invalid LaunchAgent path")?;
        for directory in [parent, &self.paths.logs, &self.paths.state] {
            private_dir(directory)?;
        }
        for log in ["vm.log", "vm.error.log"] {
            touch(&self.paths.logs.join(log))?;
        }

        let runtime = utf8(&self.paths.runtime)?;
        let plist = AgentPlist {
            label: LABEL,
            program_arguments: vec![utf8(&self.paths.bin("gremvm"))?, "internal-run".into()],
            environment_variables: BTreeMap::from([
                ("HOME", utf8(&self.paths.home)?),
                (
                    "PATH",
                    format!("{runtime}/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
                ),
            ]),
            keep_alive: KeepAlive {
                path_state: BTreeMap::from([(utf8(&self.paths.run_marker)?, true)]),
            },
            limit_load_to_session_type: "Background",
            process_type: "Background",
            throttle_interval: 10,
            standard_out_path: utf8(&self.paths.logs.join("vm.log"))?,
            standard_error_path: utf8(&self.paths.logs.join("vm.error.log"))?,
        };
        let mut temporary = NamedTempFile::new_in(parent)?;
        plist::to_writer_xml(temporary.as_file_mut(), &plist)?;
        temporary.flush()?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        temporary.as_file().sync_all()?;

        let target = service_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the service")?;
            let deadline = Instant::now() + Duration::from_secs(30);
            while self.service_loaded(&target)? {
                ensure!(Instant::now() < deadline, "service did not unload");
                thread::sleep(Duration::from_millis(100));
            }
        }
        temporary.persist(&self.paths.launch_agent)?;
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
        if !self.service_loaded(&service_target())? {
            self.launchctl(
                &[
                    "bootstrap",
                    &user_domain(),
                    self.paths
                        .launch_agent
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
        self.launchctl(&["kickstart", &service_target()], "start the service")
    }

    fn storage_root(&self) -> PathBuf {
        match &self.config.storage {
            Storage::Default => self.paths.home.join(".lume"),
            Storage::Volume { name, .. } => Path::new("/Volumes").join(name).join("GremVM"),
        }
    }

    fn vm_dir(&self) -> PathBuf {
        self.storage_root().join(&self.config.vm_name)
    }

    fn vm_installation(&self) -> VmInstallation {
        let directory = self.vm_dir();
        if !directory.exists() {
            return VmInstallation::Absent;
        }
        if ["config.json", "disk.img", "nvram.bin"]
            .iter()
            .all(|file| directory.join(file).is_file())
        {
            if self.paths.provisioned.exists() {
                VmInstallation::Ready
            } else {
                VmInstallation::Unconfigured
            }
        } else if directory.join(".provisioning").exists() {
            VmInstallation::Provisioning
        } else {
            VmInstallation::Incomplete
        }
    }

    fn volume_state(&self, name: &str, uuid: &str) -> Result<VolumeState> {
        let Some(volume) = volume_info_optional(uuid)? else {
            return Ok(VolumeState::Unavailable);
        };
        ensure!(
            volume.volume_name == name && volume.volume_uuid == uuid,
            "diskutil resolved the wrong VM storage volume"
        );
        ensure!(
            volume.filesystem_type == "apfs" && volume.file_vault,
            "VM storage volume must be encrypted APFS"
        );
        match volume.mount_point {
            None => Ok(VolumeState::Unmounted),
            Some(path) if path == Path::new("/Volumes").join(name) => Ok(VolumeState::Mounted),
            Some(path) => bail!(
                "VM storage volume is mounted at {}, not /Volumes/{name}",
                path.display()
            ),
        }
    }

    fn ensure_storage(&self, access: StorageAccess) -> Result<()> {
        let Storage::Volume { name, uuid } = &self.config.storage else {
            return private_storage_dir(&self.storage_root());
        };
        let _lock = exclusive_lock(&self.paths.state.join("storage.lock"), false)?;
        let state = self.volume_state(name, uuid)?;
        if let VolumeState::Unavailable = state {
            bail!("VM storage volume is not attached: {name} ({uuid})");
        }
        let password = self.volume_password(name, uuid, access)?;
        match state {
            VolumeState::Unavailable => {
                unreachable!()
            }
            VolumeState::Unmounted => {
                let mount_point = Path::new("/Volumes").join(name);
                ensure!(
                    !mount_point.exists(),
                    "VM storage mount path already exists: {}",
                    mount_point.display()
                );
                let output = output_with_input(
                    Command::new("/usr/sbin/diskutil").args([
                        "apfs",
                        "unlockVolume",
                        uuid,
                        "-stdinpassphrase",
                    ]),
                    &password,
                    "unlock and mount the VM storage volume",
                )?;
                check_output(&output, "unlock and mount the VM storage volume")?;
            }
            VolumeState::Mounted => {}
        }
        ensure!(
            matches!(self.volume_state(name, uuid)?, VolumeState::Mounted),
            "VM storage volume is not mounted at /Volumes/{name}"
        );
        let volume = volume_info(uuid)?;
        ensure!(volume.writable_volume, "VM storage volume is read-only");
        private_storage_dir(&self.storage_root())?;
        NamedTempFile::new_in(self.storage_root())
            .and_then(NamedTempFile::close)
            .context("VM storage volume is not writable by this user")?;
        Ok(())
    }

    fn volume_password(&self, name: &str, uuid: &str, access: StorageAccess) -> Result<Vec<u8>> {
        if let Some(password) = keychain_password(&self.paths, uuid, VOLUME_PASSWORD_SERVICE)?
            && volume_password_valid(uuid, &password)?
        {
            return Ok(password);
        }
        match access {
            StorageAccess::Background => bail!(
                "the VM storage password is missing or invalid; run 'gremvm start' from an interactive terminal"
            ),
            StorageAccess::Interactive => {}
        }

        let password = hidden_line(&format!("password to unlock /Volumes/{name}: "))?;
        ensure!(
            volume_password_valid(uuid, &password)?,
            "incorrect password for VM storage volume {name}"
        );
        store_keychain_password(&self.paths, uuid, VOLUME_PASSWORD_SERVICE, &password)?;
        Ok(password)
    }

    fn ensure_current_keychain(&self) -> Result<()> {
        if !keychain_unlocked(&self.paths)? {
            ensure!(
                unlock_keychain(&self.paths, &interactive_terminal()?)?,
                "failed to unlock the host login Keychain"
            );
        }
        Ok(())
    }

    fn ensure_keychain(&self, access: KeychainAccess) -> Result<()> {
        match access {
            KeychainAccess::Current => self.ensure_current_keychain()?,
            KeychainAccess::BackgroundInteractive => self.ensure_background_keychain()?,
            KeychainAccess::Background => ensure!(
                keychain_unlocked(&self.paths)?,
                "host login Keychain is locked; run 'gremvm start' from an interactive terminal"
            ),
        }
        Ok(())
    }

    fn ensure_background_keychain(&self) -> Result<()> {
        if !self.run_keychain_helper(KeychainRequest::Check)? {
            ensure!(
                self.run_keychain_helper(KeychainRequest::Unlock {
                    terminal: interactive_terminal()?,
                })?,
                "failed to unlock the login Keychain for the background service"
            );
        }
        Ok(())
    }

    fn run_keychain_helper(&self, request: KeychainRequest) -> Result<bool> {
        let _lock = exclusive_lock(&self.paths.state.join("keychain.lock"), false)?;
        let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
        match request {
            KeychainRequest::Check => {
                self.run_keychain_helper_on("check", "/dev/null", 5, &mut signals)
            }
            KeychainRequest::Unlock { terminal } => {
                ensure!(terminal.starts_with("/dev/tty"), "invalid terminal path");
                let _hidden = HiddenInput::new(&terminal)?;
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
        self.cleanup_keychain_helper_unlocked()?;

        let plist = KeychainHelperPlist {
            label: KEYCHAIN_HELPER_LABEL,
            program_arguments: vec![
                utf8(&std::env::current_exe()?)?,
                "internal-keychain".into(),
                mode.into(),
            ],
            environment_variables: BTreeMap::from([("HOME", utf8(&self.paths.home)?)]),
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
        let plist_path_string = utf8(&plist_path)?;

        let outcome = (|| {
            self.launchctl(
                &["bootstrap", &user_domain(), &plist_path_string],
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
                ensure!(Instant::now() < deadline, "Keychain helper timed out");
                thread::sleep(Duration::from_millis(50));
            }
        })();
        let cleanup = self.cleanup_keychain_helper_unlocked();
        combine(outcome, cleanup, "unloading the Keychain helper")
    }

    fn cleanup_keychain_helper(&self) -> Result<()> {
        let _lock = exclusive_lock(&self.paths.state.join("keychain.lock"), false)?;
        self.cleanup_keychain_helper_unlocked()
    }

    fn cleanup_keychain_helper_unlocked(&self) -> Result<()> {
        let target = format!("{}/{KEYCHAIN_HELPER_LABEL}", user_domain());
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the Keychain helper")?;
        }
        remove_if_present(&self.paths.state.join("keychain.plist"))?;
        remove_if_present(&self.paths.state.join("keychain.result"))
    }

    fn guest_password(&self, policy: KeyPolicy) -> Result<String> {
        self.ensure_current_keychain()?;
        if let Some(bytes) = keychain_password(&self.paths, "admin", PASSWORD_SERVICE)? {
            let password = String::from_utf8(bytes)?;
            ensure!(
                password.len() == 48 && password.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "the stored guest password is invalid"
            );
            return Ok(password);
        }
        if let KeyPolicy::Existing = policy {
            bail!("the guest password is missing and cannot be regenerated for an existing VM");
        }

        let mut random = [0_u8; 24];
        getrandom::fill(&mut random)?;
        let password = hex::encode(random);
        store_keychain_password(&self.paths, "admin", PASSWORD_SERVICE, password.as_bytes())?;
        Ok(password)
    }

    fn lume(&self) -> Result<Command> {
        ensure!(
            is_executable(&self.paths.bin("lume")),
            "Lume is not installed"
        );
        let config_home = self.ensure_lume_config()?;
        let mut command = Command::new(self.paths.bin("lume"));
        command
            .env("XDG_CONFIG_HOME", config_home)
            .env("LUME_TELEMETRY_ENABLED", "0")
            .env("LUME_UPDATE_CHECK", "0")
            .env("LUME_LOG_LEVEL", "error");
        Ok(command)
    }

    fn lume_run(&self) -> Result<Command> {
        let mut random = [0_u8; 6];
        getrandom::fill(&mut random)?;
        let mut command = self.lume()?;
        command
            .arg("run")
            .arg(&self.config.vm_name)
            .arg("--no-display")
            .arg(format!("--vnc-password={}", URL_SAFE_NO_PAD.encode(random)))
            .arg("--storage")
            .arg(self.storage_root());
        Ok(command)
    }

    fn ensure_lume_config(&self) -> Result<PathBuf> {
        let root = self.storage_root();
        let root = utf8(&root)?;
        ensure!(
            !root.contains(['\n', '\r']),
            "storage path contains a newline"
        );
        let config_home = self.paths.state.join("lume-config");
        let directory = config_home.join("lume");
        private_dir(&directory)?;
        let path = directory.join("config.yaml");
        let contents = format!(
            "defaultLocationName: home\ncacheDirectory: {root}/cache\ncachingEnabled: false\ntelemetryEnabled: false\nvmLocations:\n  - name: home\n    path: {root}\n"
        );
        if fs::read_to_string(&path).ok().as_deref() != Some(&contents) {
            let mut temporary = NamedTempFile::new_in(&directory)?;
            temporary.write_all(contents.as_bytes())?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            temporary.persist(path)?;
        }
        Ok(config_home)
    }

    fn lume_info(&self) -> Result<VmInfo> {
        let root = self.storage_root();
        let json = checked(
            self.lume()?
                .arg("get")
                .arg(&self.config.vm_name)
                .args(["--format", "json", "--storage"])
                .arg(&root),
            "inspect the VM",
        )?;
        let mut entries: Vec<LumeInfoDto> =
            serde_json::from_str(&json).context("Lume returned malformed VM information")?;
        ensure!(
            entries.len() == 1,
            "Lume returned unexpected VM information"
        );
        let info = VmInfo::try_from(entries.remove(0))?;
        ensure!(
            info.name == self.config.vm_name,
            "Lume returned the wrong VM"
        );
        Ok(info)
    }

    fn verify_config(&self) -> Result<()> {
        let info = self.lume_info()?;
        ensure!(
            info.cpu_count == self.config.cpu_count,
            "VM CPU count differs from configuration"
        );
        ensure!(
            info.memory_size == u64::from(self.config.memory_gb) * gib(),
            "VM memory differs from configuration"
        );
        ensure!(
            info.disk_total == u64::from(self.config.disk_gb) * gib(),
            "VM disk size differs from configuration"
        );
        ensure!(info.os.eq_ignore_ascii_case("macos"), "VM is not macOS");
        ensure!(
            info.display == DISPLAY,
            "VM display differs from configuration"
        );
        ensure!(
            info.network_mode.as_deref() == Some("bridged:en0"),
            "VM network differs from bridged:en0"
        );
        Ok(())
    }

    fn preflight(&self) -> Result<()> {
        ensure!(
            self.paths.launch_agent.is_file(),
            "run 'gremvm install' first"
        );
        self.ensure_current_keychain()?;
        self.ensure_keychain(KeychainAccess::BackgroundInteractive)?;
        self.ensure_storage(StorageAccess::Interactive)?;
        match self.vm_installation() {
            VmInstallation::Absent => bail!("VM does not exist; run 'gremvm provision'"),
            VmInstallation::Unconfigured => {
                bail!("VM setup is unfinished; run 'gremvm provision' to resume")
            }
            VmInstallation::Provisioning | VmInstallation::Incomplete => {
                bail!(
                    "VM provisioning is incomplete at {}",
                    self.vm_dir().display()
                )
            }
            VmInstallation::Ready => {}
        }
        self.verify_config()?;
        validate_bridge()
    }

    fn provision(&self) -> Result<()> {
        ensure!(
            self.paths.launch_agent.is_file() && is_executable(&self.paths.bin("lume")),
            "run 'gremvm install' first"
        );
        self.ensure_current_keychain()?;
        self.ensure_keychain(KeychainAccess::BackgroundInteractive)?;
        self.ensure_storage(StorageAccess::Interactive)?;
        self.validate_host()?;
        let installation = self.vm_installation();
        let key_policy = match installation {
            VmInstallation::Absent => KeyPolicy::Create,
            VmInstallation::Incomplete
            | VmInstallation::Provisioning
            | VmInstallation::Unconfigured
            | VmInstallation::Ready => KeyPolicy::Existing,
        };
        self.install_ssh_key(key_policy)?;
        let password = self.guest_password(key_policy)?;

        match installation {
            VmInstallation::Absent => self.build_vm(&password)?,
            VmInstallation::Ready => self.verify_config()?,
            VmInstallation::Unconfigured => {
                self.verify_config()?;
                self.recover_guest(&password)?;
            }
            VmInstallation::Provisioning | VmInstallation::Incomplete => {
                bail!(
                    "VM provisioning is incomplete at {}; GremVM will not delete it",
                    self.vm_dir().display()
                )
            }
        }

        remove_if_present(&self.storage_root().join(format!(".gremvm-{IPSW_NAME}")))?;
        self.start_ready()
    }

    fn build_vm(&self, password: &str) -> Result<()> {
        remove_if_present(&self.paths.run_marker)?;
        remove_if_present(&self.paths.provisioned)?;
        remove_if_present(&self.paths.config_dir.join("known_hosts"))?;
        let ipsw = self.ensure_ipsw()?;
        println!("creating {} from macOS Tahoe 26.6...", self.config.vm_name);
        let root = self.storage_root();
        let status = self
            .lume()?
            .arg("create")
            .arg(&self.config.vm_name)
            .args(["--os", "macOS", "--ipsw"])
            .arg(&ipsw)
            .args(["--unattended", "tahoe", "--cpu"])
            .arg(self.config.cpu_count.to_string())
            .arg("--memory")
            .arg(format!("{}GB", self.config.memory_gb))
            .arg("--disk-size")
            .arg(format!("{}GB", self.config.disk_gb))
            .args([
                "--display",
                DISPLAY,
                "--network",
                "bridged:en0",
                "--storage",
            ])
            .arg(&root)
            .status()
            .context("cannot create the VM")?;
        ensure!(status.success(), "Lume failed to create the VM");
        ensure!(
            matches!(self.vm_installation(), VmInstallation::Unconfigured),
            "Lume completed without creating a usable VM"
        );
        self.verify_config()?;
        self.configure_guest(password)?;
        touch(&self.paths.provisioned)
    }

    fn ensure_ipsw(&self) -> Result<PathBuf> {
        let storage = self.storage_root();
        private_dir(&storage)?;
        let path = storage.join(format!(".gremvm-{IPSW_NAME}"));
        if self.valid_ipsw(&path)? {
            return Ok(path);
        }
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= IPSW_SIZE)
        {
            remove_if_present(&path)?;
        }
        println!("downloading the pinned macOS restore image...");
        let mut curl = Command::new("/usr/bin/curl");
        curl.args(["--fail", "--location", "--retry", "3"]);
        if path.exists() {
            curl.args(["--continue-at", "-"]);
        }
        let status = curl
            .arg("--output")
            .arg(&path)
            .arg(IPSW_URL)
            .status()
            .context("cannot download the macOS restore image")?;
        ensure!(status.success(), "restore image download failed");
        ensure!(
            self.valid_ipsw(&path)?,
            "downloaded restore image failed size or SHA-256 verification"
        );
        Ok(path)
    }

    fn valid_ipsw(&self, path: &Path) -> Result<bool> {
        if path.metadata().map(|metadata| metadata.len()).ok() != Some(IPSW_SIZE) {
            return Ok(false);
        }
        let digest = checked(
            Command::new("/usr/bin/shasum")
                .args(["-a", "256"])
                .arg(path),
            "hash the macOS restore image",
        )?;
        Ok(digest.split_whitespace().next() == Some(IPSW_SHA256))
    }

    fn configure_guest(&self, password: &str) -> Result<()> {
        self.with_temporary_runtime("guest setup", |child| {
            let ip = self.wait_for_guest_ssh_port(child)?;
            self.run_guest_setup(&ip, password)
        })
    }

    fn recover_guest(&self, password: &str) -> Result<()> {
        self.stop_lume_if_running()?;
        self.with_temporary_runtime("provisioning recovery", |child| {
            let ip = self.wait_for_guest_ssh_port(child)?;
            let admin_ready = self.wait_for_admin(&ip, Duration::from_secs(60))?;
            if admin_ready && self.guest_marker(&ip, "/var/db/gremvm-ready")? {
                return touch(&self.paths.provisioned);
            }
            if admin_ready {
                self.run_guest_setup_as_admin(&ip, password)?;
            } else {
                self.run_guest_setup(&ip, password)?;
            }
            ensure!(
                self.guest_marker(&ip, "/var/db/gremvm-ready")?,
                "guest provisioning recovery did not finish"
            );
            touch(&self.paths.provisioned)
        })
    }

    fn run_guest_setup(&self, ip: &str, password: &str) -> Result<()> {
        let mut askpass = NamedTempFile::new_in(&self.paths.state)?;
        askpass.write_all(b"#!/bin/sh\nprintf '%s\\n' lume\n")?;
        askpass.flush()?;
        askpass
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
        let mut command = self.ssh_base();
        command
            .args([
                "-o",
                "BatchMode=no",
                "-o",
                "PreferredAuthentications=password",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "NumberOfPasswordPrompts=1",
            ])
            .env("SSH_ASKPASS", askpass.path())
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("DISPLAY", "gremvm")
            .arg(format!("lume@{ip}"))
            .arg("/bin/bash -s");
        self.apply_guest_setup(&mut command, password)
    }

    fn run_guest_setup_as_admin(&self, ip: &str, password: &str) -> Result<()> {
        let mut command = self.ssh_command(ip);
        command.arg("/bin/bash -s");
        self.apply_guest_setup(&mut command, password)
    }

    fn apply_guest_setup(&self, command: &mut Command, password: &str) -> Result<()> {
        let script = fs::read(self.paths.guest_setup()).context("cannot read guest setup")?;
        let public_key = fs::read(self.paths.ssh_key.with_extension("pub"))?;
        let mut payload = format!(
            "set -- '{}' '{}'\n",
            BASE64.encode(public_key),
            BASE64.encode(password)
        )
        .into_bytes();
        payload.extend(script);
        let output = output_with_input(command, &payload, "configure the guest")?;
        check_output(&output, "configure the guest")?;
        self.wait_for_ssh(Duration::from_secs(180))?;
        Ok(())
    }

    fn with_temporary_runtime<T>(
        &self,
        activity: &str,
        action: impl FnOnce(&mut Child) -> Result<T>,
    ) -> Result<T> {
        let mut child = self
            .lume_run()?
            .spawn()
            .with_context(|| format!("cannot start Lume for {activity}"))?;
        let outcome = action(&mut child);
        let cleanup = self.stop_runtime(&mut child);
        combine(outcome, cleanup, &format!("stopping Lume after {activity}"))
    }

    fn wait_for_guest_ssh_port(&self, child: &mut Child) -> Result<String> {
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("Lume exited during guest setup with {status}");
            }
            if let VmState::Running { ip: Some(ip), .. } = self.lume_info()?.state
                && remote_port_open(&ip, 22)?
            {
                return Ok(ip);
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for the unattended guest"
            );
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn wait_for_admin(&self, ip: &str, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.ssh_probe(ip)? {
                return Ok(true);
            }
            thread::sleep(Duration::from_secs(2));
        }
        Ok(false)
    }

    fn guest_marker(&self, ip: &str, path: &str) -> Result<bool> {
        Ok(self
            .ssh_command(ip)
            .arg(format!("/usr/bin/test -f {path}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success())
    }

    fn start(&self) -> Result<()> {
        self.preflight()?;
        self.start_ready()
    }

    fn start_ready(&self) -> Result<()> {
        touch(&self.paths.run_marker)?;
        self.start_service()?;
        let desktop = self.wait_for_desktop(Duration::from_secs(300))?;
        println!("running: admin@{}", desktop.ip);
        Ok(())
    }

    fn restart(&self) -> Result<()> {
        self.preflight()?;
        self.stop_vm()?;
        self.start_ready()
    }

    fn stop(&self) -> Result<()> {
        self.stop_vm()?;
        println!("stopped");
        Ok(())
    }

    fn stop_vm(&self) -> Result<()> {
        remove_if_present(&self.paths.run_marker)?;
        remove_if_present(&self.paths.desktop)?;
        let helper = self.cleanup_keychain_helper();
        let target = service_target();
        let service_was_loaded = self.service_loaded(&target)?;
        let unload = (|| {
            if service_was_loaded {
                self.launchctl(&["bootout", &target], "unload the service")?;
            }
            Ok(())
        })();
        let stop = (|| {
            let storage_available = match &self.config.storage {
                Storage::Default => true,
                Storage::Volume { name, uuid } => {
                    matches!(self.volume_state(name, uuid)?, VolumeState::Mounted)
                }
            };
            if is_executable(&self.paths.bin("lume")) && storage_available {
                match self.vm_installation() {
                    VmInstallation::Ready | VmInstallation::Unconfigured => {
                        if service_was_loaded && self.wait_for_lume_stop(Duration::from_secs(35))? {
                            Ok(())
                        } else {
                            self.stop_lume_if_running()
                        }
                    }
                    VmInstallation::Provisioning => {
                        bail!("VM provisioning is still in progress; stop it before retrying")
                    }
                    VmInstallation::Absent | VmInstallation::Incomplete => Ok(()),
                }
            } else {
                Ok(())
            }
        })();
        let unload = combine(helper, unload, "unloading the background service");
        combine(unload, stop, "stopping the VM")
    }

    fn wait_for_lume_stop(&self, timeout: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.lume_info()?.state {
                VmState::Stopped => return Ok(true),
                VmState::Provisioning { .. } => bail!("cannot stop a provisioning VM"),
                VmState::Running { .. } if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                VmState::Running { .. } => return Ok(false),
            }
        }
    }

    fn stop_lume_if_running(&self) -> Result<()> {
        match self.lume_info()?.state {
            VmState::Stopped => Ok(()),
            VmState::Provisioning { .. } => bail!("cannot stop a provisioning VM"),
            VmState::Running { .. } => success(
                self.lume()?
                    .arg("stop")
                    .arg(&self.config.vm_name)
                    .arg("--storage")
                    .arg(self.storage_root()),
                "stop the VM",
            ),
        }
    }

    fn stop_runtime(&self, child: &mut Child) -> Result<()> {
        let stop = self.stop_lume_if_running();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait()?.is_some() {
                return stop;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                stop?;
                bail!("Lume did not exit after stopping the VM");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn ssh_base(&self) -> Command {
        let mut command = Command::new("/usr/bin/ssh");
        command
            .args(["-F", "/dev/null"])
            .arg("-i")
            .arg(&self.paths.ssh_key)
            .args([
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=accept-new",
            ])
            .arg("-o")
            .arg(format!(
                "UserKnownHostsFile=\"{}\"",
                self.paths
                    .config_dir
                    .join("known_hosts")
                    .display()
                    .to_string()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            ));
        command
    }

    fn ssh_command(&self, ip: &str) -> Command {
        let mut command = self.ssh_base();
        command
            .args(["-o", "BatchMode=yes"])
            .arg(format!("admin@{ip}"));
        command
    }

    fn reverse_tunnel(&self, ip: &str, local_vnc_port: u16) -> Command {
        let mut command = self.ssh_base();
        command
            .args([
                "-o",
                "BatchMode=yes",
                "-N",
                "-T",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-R",
            ])
            .arg(format!("0.0.0.0:5900:127.0.0.1:{local_vnc_port}"))
            .arg(format!("admin@{ip}"));
        command
    }

    fn ssh_probe(&self, ip: &str) -> Result<bool> {
        Ok(self
            .ssh_command(ip)
            .args(["-o", "ConnectTimeout=5", "-o", "ConnectionAttempts=1"])
            .arg("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("cannot probe guest SSH")?
            .success())
    }

    fn guest_console_ready(&self, ip: &str) -> Result<bool> {
        let output = self
            .ssh_command(ip)
            .args(["-o", "ConnectTimeout=5", "-o", "ConnectionAttempts=1"])
            .arg("/usr/bin/stat -f %Su /dev/console")
            .stdin(Stdio::null())
            .output()
            .context("cannot inspect the guest console user")?;
        Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "admin")
    }

    fn wait_for_ssh(&self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let info = self.lume_info()?;
            match info.state {
                VmState::Running { ip: Some(ip), .. } if self.ssh_probe(&ip)? => return Ok(ip),
                VmState::Stopped if !self.paths.run_marker.exists() => {
                    bail!("VM is not running; run 'gremvm start'")
                }
                VmState::Running { .. } | VmState::Stopped => {}
                VmState::Provisioning { operation } => bail!(
                    "VM is still provisioning{}",
                    operation
                        .map(|value| format!(": {value}"))
                        .unwrap_or_default()
                ),
            }
            ensure!(Instant::now() < deadline, "timed out waiting for guest SSH");
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn wait_for_desktop(&self, timeout: Duration) -> Result<Desktop> {
        let deadline = Instant::now() + timeout;
        loop {
            let desktop: Option<Desktop> = match fs::File::open(&self.paths.desktop) {
                Ok(file) => Some(
                    serde_json::from_reader(file)
                        .context("the guest desktop readiness file is invalid")?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            match (self.lume_info()?.state, desktop) {
                (
                    VmState::Running {
                        ip: Some(ip),
                        vnc_url: Some(local_url),
                        ..
                    },
                    Some(desktop),
                ) if desktop.ip == ip
                    && desktop.url == guest_vnc_url(&local_url, &ip)?
                    && remote_port_open(&ip, 5900)? =>
                {
                    return Ok(desktop);
                }
                (VmState::Running { .. }, _) => {}
                (VmState::Stopped, _) if !self.paths.run_marker.exists() => {
                    bail!("VM is not running; run 'gremvm start'")
                }
                (VmState::Stopped, _) => {}
                (VmState::Provisioning { .. }, _) => bail!("VM is still provisioning"),
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for the guest desktop"
            );
            thread::sleep(Duration::from_secs(2));
        }
    }

    fn ssh(&self, arguments: &[OsString]) -> Result<()> {
        self.preflight()?;
        let ip = self.wait_for_ssh(Duration::from_secs(120))?;
        let mut command = self.ssh_command(&ip);
        command.args(arguments);
        Err(anyhow!("cannot execute ssh: {}", command.exec()))
    }

    fn screen_share(&self, url_only: bool) -> Result<()> {
        if !url_only {
            ensure_graphical_session("screen-share")?;
        }
        self.preflight()?;
        let desktop = self.wait_for_desktop(Duration::from_secs(120))?;
        if url_only {
            println!("{}", desktop.url);
            return Ok(());
        }
        open_screen_sharing(&desktop.url, "open guest Screen Sharing")?;
        println!("screen sharing: admin@{}", desktop.ip);
        Ok(())
    }

    fn console(&self) -> Result<()> {
        ensure_graphical_session("console")?;
        self.preflight()?;
        match self.lume_info()?.state {
            VmState::Running { .. } => {}
            VmState::Stopped => bail!("VM is not running; run 'gremvm start'"),
            VmState::Provisioning { .. } => bail!("VM is still provisioning"),
        }

        println!("waiting for Lume's local console...");
        let deadline = Instant::now() + Duration::from_secs(120);
        let url = loop {
            let candidate = match self.lume_info()?.state {
                VmState::Running { vnc_url, .. } => vnc_url,
                VmState::Stopped => bail!("VM stopped before its console became ready"),
                VmState::Provisioning { .. } => bail!("VM is still provisioning"),
            };
            if let Some(url) = candidate
                && local_vnc_port(&url).is_some_and(local_port_open)
            {
                break url;
            }
            ensure!(
                Instant::now() < deadline,
                "timed out waiting for Lume's local console"
            );
            thread::sleep(Duration::from_millis(500));
        };
        open_screen_sharing(&url, "open the recovery console")?;
        println!("console opened; closing it leaves the VM running");
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

    fn status(&self) -> Result<()> {
        if let Storage::Volume { name, uuid } = &self.config.storage
            && !matches!(self.volume_state(name, uuid)?, VolumeState::Mounted)
        {
            println!("state: storage-unavailable");
            println!("name: {}", self.config.vm_name);
            println!("storage: /Volumes/{name}/GremVM");
            return Ok(());
        }
        match self.vm_installation() {
            VmInstallation::Absent => {
                println!("state: not-provisioned");
                return Ok(());
            }
            VmInstallation::Incomplete | VmInstallation::Unconfigured => {
                println!("state: incomplete");
                return Ok(());
            }
            VmInstallation::Provisioning => {
                println!("state: provisioning");
                return Ok(());
            }
            VmInstallation::Ready => {}
        }

        let info = self.lume_info()?;
        let (state, ip) = match &info.state {
            VmState::Running { ip: Some(ip), .. } => ("running", Some(ip.as_str())),
            VmState::Running { ip: None, .. } => ("running-address-unknown", None),
            VmState::Stopped if self.paths.run_marker.exists() => ("starting", None),
            VmState::Stopped => ("stopped", None),
            VmState::Provisioning { .. } => ("provisioning", None),
        };
        println!("state: {state}");
        if let Some(ip) = ip {
            println!("ip: {ip}");
        }
        println!("name: {}", info.name);
        println!("cpu: {}", info.cpu_count);
        println!("memory-gb: {}", info.memory_size / gib());
        println!("disk-gb: {}", info.disk_total / gib());
        println!("disk-allocated-gb: {}", info.disk_allocated / gib());
        println!("display: {}", info.display);
        println!(
            "network: {}",
            info.network_mode.as_deref().unwrap_or("unknown")
        );
        println!("storage: {}", self.storage_root().display());
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        self.stop_vm()?;
        remove_if_present(&self.paths.launch_agent)?;
        if fs::symlink_metadata(&self.paths.command_link)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
            && fs::read_link(&self.paths.command_link)? == self.paths.bin("gremvm")
        {
            remove_if_present(&self.paths.command_link)?;
        }
        if fs::symlink_metadata(&self.paths.runtime)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            remove_if_present(&self.paths.runtime)?;
        }
        println!(
            "uninstalled; VM data preserved at {}",
            self.vm_dir().display()
        );
        Ok(())
    }

    fn internal_run(&self) -> Result<()> {
        if !self.paths.run_marker.exists() {
            return Ok(());
        }
        self.ensure_keychain(KeychainAccess::Background)?;
        self.ensure_storage(StorageAccess::Background)?;
        ensure!(
            matches!(self.vm_installation(), VmInstallation::Ready),
            "VM data is incomplete"
        );
        validate_bridge()?;
        self.verify_config()?;

        let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
        let mut owner = match self.lume_info()?.state {
            VmState::Running { .. } => RuntimeOwner::External,
            VmState::Stopped => {
                RuntimeOwner::Supervisor(self.lume_run()?.spawn().context("cannot start Lume")?)
            }
            VmState::Provisioning { .. } => bail!("VM is still provisioning"),
        };
        self.watch_vm(&mut owner, &mut signals)
    }

    fn watch_vm(&self, owner: &mut RuntimeOwner, signals: &mut Signals) -> Result<()> {
        remove_if_present(&self.paths.desktop)?;
        let outcome = self.watch_runtime(owner, signals);
        let marker = remove_if_present(&self.paths.desktop);
        let runtime = self.stop_watched_runtime(owner);
        let cleanup = combine(marker, runtime, "stopping the VM");
        combine(outcome, cleanup, "stopping the VM")
    }

    fn watch_runtime(&self, owner: &mut RuntimeOwner, signals: &mut Signals) -> Result<()> {
        let boot_deadline = Instant::now() + Duration::from_secs(900);
        let (ip, vnc_port) = loop {
            if signals.pending().next().is_some() {
                return Ok(());
            }
            match owner {
                RuntimeOwner::Supervisor(process) => {
                    if let Some(status) = process.try_wait()? {
                        bail!("Lume exited before the guest desktop was ready: {status}");
                    }
                }
                RuntimeOwner::External => {}
            }
            let ready = match self.lume_info()?.state {
                VmState::Running {
                    ip: Some(ip),
                    vnc_url: Some(url),
                } if self.ssh_probe(&ip)? && self.guest_console_ready(&ip)? => local_vnc_port(&url)
                    .filter(|port| local_port_open(*port))
                    .map(|port| (ip, port)),
                VmState::Running { .. } | VmState::Stopped => None,
                VmState::Provisioning { .. } => bail!("VM returned to provisioning"),
            };
            if let Some(ready) = ready {
                break ready;
            }
            ensure!(
                Instant::now() < boot_deadline,
                "guest desktop did not become ready"
            );
            thread::sleep(Duration::from_secs(2));
        };

        ensure!(
            !remote_port_open(&ip, 5900)?,
            "guest port 5900 is already in use"
        );
        let mut tunnel = self
            .reverse_tunnel(&ip, vnc_port)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .context("cannot expose the guest desktop")?;
        let outcome = (|| {
            let tunnel_deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = tunnel.try_wait()? {
                    bail!("cannot expose the guest desktop: {status}");
                }
                if remote_port_open(&ip, 5900)? {
                    break;
                }
                ensure!(
                    Instant::now() < tunnel_deadline,
                    "timed out exposing the guest desktop"
                );
                thread::sleep(Duration::from_millis(100));
            }
            let local_url = match self.lume_info()?.state {
                VmState::Running {
                    ip: Some(current_ip),
                    vnc_url: Some(url),
                } if current_ip == ip && local_vnc_port(&url) == Some(vnc_port) => url,
                _ => bail!("Lume's VNC console changed during startup"),
            };
            write_json(
                &self.paths.desktop,
                &Desktop {
                    url: guest_vnc_url(&local_url, &ip)?,
                    ip: ip.clone(),
                },
            )?;

            let mut next_probe = Instant::now();
            loop {
                if signals.pending().next().is_some() {
                    return Ok(());
                }
                if let RuntimeOwner::Supervisor(process) = owner
                    && let Some(status) = process.try_wait()?
                {
                    bail!("Lume exited: {status}");
                }
                if let Some(status) = tunnel.try_wait()? {
                    bail!("guest desktop tunnel exited; restarting the VM: {status}");
                }
                if Instant::now() >= next_probe {
                    ensure!(
                        local_port_open(vnc_port),
                        "the guest desktop disappeared; restarting the VM"
                    );
                    next_probe = Instant::now() + Duration::from_secs(2);
                }
                thread::sleep(Duration::from_millis(250));
            }
        })();
        let tunnel_cleanup = stop_child(&mut tunnel, "guest desktop tunnel");
        combine(outcome, tunnel_cleanup, "stopping the guest desktop tunnel")
    }

    fn stop_watched_runtime(&self, owner: &mut RuntimeOwner) -> Result<()> {
        match owner {
            RuntimeOwner::Supervisor(process) => self.stop_runtime(process),
            RuntimeOwner::External => self.stop_lume_if_running(),
        }
    }
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("path is not UTF-8: {}", path.display()))
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

fn private_dir(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(path.is_dir(), "not a directory: {}", path.display());
        return Ok(());
    }
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    Ok(())
}

fn management_lock(paths: &Paths) -> Result<fs::File> {
    exclusive_lock(&paths.state.join("management.lock"), true)
}

fn exclusive_lock(path: &Path, fail_fast: bool) -> Result<fs::File> {
    private_dir(path.parent().context("lock file has no parent directory")?)?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    let operation = libc::LOCK_EX | if fail_fast { libc::LOCK_NB } else { 0 };
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), operation) } == 0,
        "another GremVM operation is already using {}",
        path.display()
    );
    Ok(lock)
}

fn private_storage_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "refusing unsafe storage path: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn touch(path: &Path) -> Result<()> {
    private_dir(path.parent().context("file has no parent directory")?)?;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("file has no parent directory")?;
    private_dir(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    writeln!(temporary)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

fn checked(command: &mut Command, description: &str) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("cannot {description}"))?;
    check_output(&output, description)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn success(command: &mut Command, description: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot {description}"))?;
    check_output(&output, description)
}

fn combine<T>(outcome: Result<T>, cleanup: Result<()>, description: &str) -> Result<T> {
    match (outcome, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("{description} also failed: {cleanup:#}")))
        }
    }
}

fn stop_child(child: &mut Child, description: &str) -> Result<()> {
    if child
        .try_wait()
        .with_context(|| format!("cannot inspect {description}"))?
        .is_some()
    {
        return Ok(());
    }
    child
        .kill()
        .with_context(|| format!("cannot stop {description}"))?;
    child
        .wait()
        .with_context(|| format!("cannot reap {description}"))?;
    Ok(())
}

fn check_output(output: &Output, description: &str) -> Result<()> {
    ensure!(
        output.status.success(),
        "failed to {description}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn output_with_input(command: &mut Command, input: &[u8], description: &str) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot {description}"))?;
    let mut stdin = child.stdin.take().context("child stdin is unavailable")?;
    stdin.write_all(input)?;
    if !input.ends_with(b"\n") {
        stdin.write_all(b"\n")?;
    }
    drop(stdin);
    child
        .wait_with_output()
        .with_context(|| format!("cannot {description}"))
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

fn hidden_line(prompt: &str) -> Result<Vec<u8>> {
    let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
    interactive_terminal()?;
    let value = rpassword::prompt_password(prompt)?.into_bytes();
    ensure!(
        signals.pending().next().is_none(),
        "password input was interrupted"
    );
    ensure!(!value.is_empty(), "password cannot be empty");
    ensure!(value.len() <= 1024, "password is too long");
    Ok(value)
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
    let _hidden = HiddenInput::new(terminal)?;
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

fn keychain_password(paths: &Paths, account: &str, service: &str) -> Result<Option<Vec<u8>>> {
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-a", account, "-s", service, "-w"])
        .arg(login_keychain(paths))
        .output()
        .context("cannot read a password from the host login Keychain")?;
    if output.status.success() {
        let mut password = output.stdout;
        while password
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            password.pop();
        }
        ensure!(!password.is_empty(), "the stored password is empty");
        return Ok(Some(password));
    }
    if output.status.code() == Some(44) {
        return Ok(None);
    }
    bail!(
        "cannot read a password from the host login Keychain: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn store_keychain_password(
    paths: &Paths,
    account: &str,
    service: &str,
    password: &[u8],
) -> Result<()> {
    valid_name(account).map_err(anyhow::Error::msg)?;
    valid_name(service).map_err(anyhow::Error::msg)?;
    let keychain = login_keychain(paths);
    let keychain = keychain
        .to_str()
        .with_context(|| format!("Keychain path is not UTF-8: {}", keychain.display()))?;
    let command = format!(
        "add-generic-password -a {account} -s {service} -U -X {} {}\n",
        hex::encode(password),
        shell_word(keychain)
    );
    let output = output_with_input(
        Command::new("/usr/bin/security").arg("-i"),
        command.as_bytes(),
        "store a password in the host login Keychain",
    )?;
    check_output(&output, "store a password in the host login Keychain")
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn volume_password_valid(uuid: &str, password: &[u8]) -> Result<bool> {
    let output = output_with_input(
        Command::new("/usr/sbin/diskutil").args([
            "apfs",
            "unlockVolume",
            uuid,
            "-stdinpassphrase",
            "-verify",
        ]),
        password,
        "verify the VM storage password",
    )?;
    Ok(output.status.success())
}

fn volume_info(selector: &str) -> Result<VolumeInfo> {
    volume_info_optional(selector)?
        .with_context(|| format!("VM storage volume is not attached: {selector}"))
}

fn volume_info_optional(selector: &str) -> Result<Option<VolumeInfo>> {
    let output = Command::new("/usr/sbin/diskutil")
        .args(["info", "-plist", selector])
        .output()
        .context("cannot inspect the VM storage volume")?;
    if !output.status.success() {
        return Ok(None);
    }
    plist::from_bytes(&output.stdout)
        .map(Some)
        .context("diskutil returned malformed volume information")
}

fn sysctl(name: &str) -> Result<u64> {
    checked(
        Command::new("/usr/sbin/sysctl").args(["-n", name]),
        "run sysctl",
    )?
    .parse()
    .context("sysctl returned a non-number")
}

const fn gib() -> u64 {
    1024 * 1024 * 1024
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

fn service_target() -> String {
    format!("{}/{LABEL}", user_domain())
}

fn ensure_graphical_session(command: &str) -> Result<()> {
    ensure!(
        Command::new("/bin/launchctl")
            .args(["print", &format!("gui/{}", unsafe { libc::getuid() })])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("cannot inspect the graphical login session")?
            .success(),
        "gremvm {command} requires an active graphical host session"
    );
    Ok(())
}

fn open_screen_sharing(url: &str, description: &str) -> Result<()> {
    let fresh = Command::new("/usr/bin/open")
        .args(["-n", "-a", "Screen Sharing"])
        .arg(url)
        .status()
        .with_context(|| format!("cannot {description}"))?;
    if fresh.success() {
        return Ok(());
    }
    success(Command::new("/usr/bin/open").arg(url), description)
}

fn local_vnc_port(url: &str) -> Option<u16> {
    let authority = url.strip_prefix("vnc://")?.split('/').next()?;
    let host_and_port = authority.rsplit('@').next()?;
    let (host, port) = host_and_port.rsplit_once(':')?;
    matches!(host, "127.0.0.1" | "localhost")
        .then(|| port.parse().ok())
        .flatten()
}

fn local_port_open(port: u16) -> bool {
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(250),
    )
    .is_ok()
}

fn remote_port_open(host: &str, port: u16) -> Result<bool> {
    Ok((host, port)
        .to_socket_addrs()?
        .any(|address| TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()))
}

fn guest_vnc_url(local_url: &str, ip: &str) -> Result<String> {
    local_vnc_port(local_url).context("Lume returned an invalid VNC URL")?;
    let authority = local_url
        .strip_prefix("vnc://")
        .context("Lume returned an invalid VNC URL")?
        .split('/')
        .next()
        .context("Lume returned an invalid VNC URL")?;
    let credentials = authority
        .rsplit_once('@')
        .map(|(value, _)| format!("{value}@"))
        .unwrap_or_default();
    let host = if ip.contains(':') {
        format!("[{ip}]")
    } else {
        ip.to_owned()
    };
    Ok(format!("vnc://{credentials}{host}:5900"))
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
