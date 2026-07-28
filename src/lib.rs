use anyhow::{Context, Result, anyhow, bail, ensure};
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
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

const LABEL: &str = "io.gremvm.tart";
const KEYCHAIN_HELPER_LABEL: &str = "io.gremvm.keychain";
const PASSWORD_SERVICE: &str = "io.gremvm.tart.gui-password";
const BRIDGE: &str = "en0";

#[derive(Parser)]
#[command(
    name = "gremvm",
    version,
    about = "Manage one persistent Tart macOS VM",
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
    /// Start the VM and wait for SSH.
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
    ScreenShare,
    /// Open Tart's local recovery console.
    Console,
    /// Show VM logs.
    Logs {
        #[arg(long)]
        follow: bool,
    },
    /// Remove the service and runtime, preserving VM data.
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
    /// Name of the Tart VM.
    #[arg(long, default_value = "gremvm", value_parser = valid_name)]
    vm_name: String,
    /// Number of virtual CPUs.
    #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..=64))]
    cpu_count: u32,
    /// Guest memory in GiB.
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u32).range(4..=256))]
    memory_gb: u32,
    /// Virtual disk size in decimal GB.
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
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Storage {
    Default,
    Volume { name: String, uuid: String },
}

impl InstallOptions {
    fn resolve(self) -> Result<Config> {
        let storage = match self.volume_name {
            Some(name) => {
                let volume = volume_info(&name)?;
                ensure!(
                    volume.volume_name == name,
                    "diskutil resolved the wrong volume"
                );
                Storage::Volume {
                    name,
                    uuid: volume.volume_uuid,
                }
            }
            None => Storage::Default,
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
    home: PathBuf,
    root: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state: PathBuf,
    logs: PathBuf,
    runtime: PathBuf,
    run_marker: PathBuf,
    provisioned: PathBuf,
    launch_agent: PathBuf,
    command_link: PathBuf,
    ssh_key: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
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

enum ConsoleSource {
    Snapshot,
    ColdBoot,
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

pub fn run() -> Result<()> {
    let command = Action::parse();
    let paths = Paths::discover()?;
    let _lock = match &command {
        Action::Install(_)
        | Action::Provision
        | Action::Start
        | Action::Stop
        | Action::Restart
        | Action::Console
        | Action::Uninstall => Some(management_lock(&paths)?),
        _ => None,
    };
    match command {
        Action::Install(options) => App {
            paths,
            config: options.resolve()?,
        }
        .install(),
        Action::InternalKeychain { mode } => internal_keychain(&paths, mode),
        Action::Status if !is_executable(&paths.bin("tart")) => {
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
            Action::ScreenShare => self.screen_share(),
            Action::Console => self.console(),
            Action::Logs { follow } => self.logs(follow),
            Action::Uninstall => self.uninstall(),
            Action::InternalRun => self.internal_run(),
            Action::InternalKeychain { .. } => unreachable!(),
        }
    }

    fn install(&self) -> Result<()> {
        self.clear_keychain_helper()?;
        self.config.check_existing(&self.paths)?;
        self.ensure_keychain(KeychainMode::Current)?;
        self.ensure_storage()?;
        self.validate_host()?;
        self.install_runtime()?;
        self.install_command()?;
        let exists = self.vm_exists()?;
        if exists {
            self.verify_config()?;
        }
        let policy = if exists {
            CredentialPolicy::Existing
        } else {
            CredentialPolicy::CreateIfMissing
        };
        self.install_ssh_key(policy)?;
        self.guest_password(policy)?;
        let restart = exists && self.paths.run_marker.exists();
        if restart {
            self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        }
        self.config.persist(&self.paths)?;
        self.install_service()?;
        if restart {
            self.start_service()?;
        }
        println!("installed: {}", self.paths.command_link.display());
        if exists {
            println!("VM: {}", self.config.vm_name);
        } else {
            println!("next: gremvm provision");
        }
        Ok(())
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
            "share/gremvm/auto-login.pl",
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

    fn guest_password(&self, policy: CredentialPolicy) -> Result<String> {
        self.ensure_keychain(KeychainMode::Current)?;
        let output = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-a",
                "admin",
                "-s",
                PASSWORD_SERVICE,
                "-w",
            ])
            .output()?;
        if output.status.success() {
            let password = String::from_utf8(output.stdout)?.trim().to_owned();
            ensure!(!password.is_empty(), "the stored guest password is empty");
            return Ok(password);
        }
        ensure!(
            output.status.code() == Some(44),
            "cannot read the guest password from Keychain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        match policy {
            CredentialPolicy::Existing => bail!(
                "the guest password is missing from Keychain and cannot be regenerated for an existing VM"
            ),
            CredentialPolicy::CreateIfMissing => {}
        }
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random)?;
        let password = hex::encode(random);
        let mut security = Command::new("/usr/bin/security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot store the guest password in Keychain")?;
        writeln!(
            security
                .stdin
                .take()
                .context("security stdin is unavailable")?,
            "add-generic-password -a admin -s {PASSWORD_SERVICE} -U -w {password}"
        )?;
        check_output(
            &security.wait_with_output()?,
            "store the guest password in Keychain",
        )?;
        Ok(password)
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
        let string = |path: &Path| {
            path.to_str()
                .map(str::to_owned)
                .with_context(|| format!("path is not UTF-8: {}", path.display()))
        };
        let runtime = string(&self.paths.runtime)?;
        let plist = AgentPlist {
            label: LABEL,
            program_arguments: vec![string(&self.paths.bin("gremvm"))?, "internal-run".into()],
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
        let target = service_target();
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the service")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.service_loaded(&target)? {
                ensure!(Instant::now() < deadline, "service did not unload");
                thread::sleep(Duration::from_millis(50));
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

    fn provision(&self) -> Result<()> {
        self.ensure_keychain(KeychainMode::Current)?;
        self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        self.ensure_storage()?;
        self.validate_host()?;
        if self.vm_exists()? {
            ensure!(
                self.paths.provisioned.exists(),
                "VM provisioning is incomplete"
            );
            self.verify_config()?;
        } else {
            self.build_vm()?;
            ensure!(
                self.vm_exists()?,
                "Packer completed without creating the VM"
            );
            self.verify_config()?;
            touch(&self.paths.provisioned)?;
        }
        touch(&self.paths.run_marker)?;
        self.start_service()?;
        println!("ready: admin@{}", self.wait_for_ssh(300)?);
        Ok(())
    }

    fn build_vm(&self) -> Result<()> {
        let public_key = fs::read_to_string(self.paths.ssh_key.with_extension("pub"))?;
        let password = self.guest_password(CredentialPolicy::Existing)?;
        println!(
            "creating {} from the pinned macOS image...",
            self.config.vm_name
        );
        self.packer(public_key.trim(), &password)
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
                "the host login Keychain is locked; run 'gremvm start' from an interactive terminal"
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
            label: KEYCHAIN_HELPER_LABEL,
            program_arguments: vec![
                string(&std::env::current_exe()?)?,
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
        let target = format!("{}/{KEYCHAIN_HELPER_LABEL}", user_domain());
        if self.service_loaded(&target)? {
            self.launchctl(&["bootout", &target], "unload the Keychain helper")?;
        }
        remove_if_present(&self.paths.state.join("keychain.plist"))?;
        remove_if_present(&self.paths.state.join("keychain.result"))
    }

    fn tart_home(&self) -> PathBuf {
        match &self.config.storage {
            Storage::Default => self.paths.home.join(".tart"),
            Storage::Volume { name, .. } => Path::new("/Volumes").join(name),
        }
    }

    fn ensure_volume(&self, name: &str, uuid: &str) -> Result<()> {
        let mount_point = Path::new("/Volumes").join(name);
        let mut volume = volume_info(uuid)?;
        ensure!(
            volume.volume_name == name && volume.volume_uuid == uuid,
            "diskutil resolved the wrong VM storage volume"
        );
        ensure!(
            volume.filesystem_type == "apfs" && volume.file_vault,
            "VM storage volume must be encrypted APFS"
        );
        if volume
            .mount_point
            .as_ref()
            .is_none_or(|path| path.as_os_str().is_empty())
        {
            ensure!(
                !mount_point.exists(),
                "VM storage mount path already exists: {}",
                mount_point.display()
            );
            success(
                Command::new("/usr/sbin/diskutil")
                    .args(["mount", uuid])
                    .stdin(Stdio::null()),
                "mount the VM storage volume",
            )?;
            volume = volume_info(uuid)?;
        }
        ensure!(
            volume.volume_name == name
                && volume.volume_uuid == uuid
                && volume.mount_point.as_deref() == Some(mount_point.as_path()),
            "VM storage volume {} is not mounted at {}",
            name,
            mount_point.display()
        );
        ensure!(volume.writable_volume, "VM storage volume is read-only");
        NamedTempFile::new_in(&mount_point)
            .and_then(NamedTempFile::close)
            .context("VM storage volume is not writable by this user")?;
        Ok(())
    }

    fn volume_mounted(&self, name: &str, uuid: &str) -> Result<bool> {
        let volume = volume_info(uuid)?;
        Ok(volume.volume_name == name
            && volume.volume_uuid == uuid
            && volume.mount_point.as_deref() == Some(Path::new("/Volumes").join(name).as_path()))
    }

    fn ensure_storage(&self) -> Result<()> {
        match &self.config.storage {
            Storage::Default => Ok(()),
            Storage::Volume { name, uuid } => self.ensure_volume(name, uuid),
        }
    }

    fn configure_tart_storage(&self, command: &mut Command) -> Result<()> {
        match &self.config.storage {
            Storage::Default => {
                command.env_remove("TART_HOME");
                Ok(())
            }
            Storage::Volume { name, uuid } => {
                self.ensure_volume(name, uuid)?;
                command.env("TART_HOME", Path::new("/Volumes").join(name));
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
            vm.disk == u64::from(self.config.disk_gb),
            "VM disk differs from configuration"
        );
        ensure!(vm.os == "darwin", "VM operating system is not macOS");
        Ok(())
    }

    fn preflight_start(&self) -> Result<()> {
        ensure!(
            self.paths.launch_agent.is_file(),
            "run 'gremvm install' first"
        );
        self.ensure_keychain(KeychainMode::Current)?;
        self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        self.ensure_storage()?;
        ensure!(
            self.vm_exists()?,
            "VM does not exist; run 'gremvm provision'"
        );
        ensure!(
            self.paths.provisioned.exists(),
            "VM provisioning is incomplete"
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
        println!("running: admin@{}", self.wait_for_ssh(300)?);
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
            Storage::Volume { name, uuid } => self.volume_mounted(name, uuid)?,
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
        let target = service_target();
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
        let target = service_target();
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
            .arg(format!("admin@{ip}"));
        command
    }

    fn ssh(&self, arguments: &[OsString]) -> Result<()> {
        let ip = self.wait_for_ssh(120)?;
        let mut command = self.ssh_command(&ip);
        command.args(arguments);
        Err(anyhow!("cannot execute ssh: {}", command.exec()))
    }

    fn screen_share(&self) -> Result<()> {
        self.ensure_storage()?;
        ensure!(
            self.vm_exists()?,
            "VM does not exist; run 'gremvm provision'"
        );
        match self.vm_info()?.state {
            VmState::Running => {}
            VmState::Suspended | VmState::Stopped => {
                bail!("VM is not running; run 'gremvm start'")
            }
        }
        let ip = self.vm_ip(120)?;
        ensure_graphical_session("screen-share")?;
        success(
            Command::new("/usr/bin/open").arg(format!("vnc://{ip}")),
            "open guest Screen Sharing",
        )?;
        println!("screen sharing: admin@{ip}");
        Ok(())
    }

    fn console(&self) -> Result<()> {
        ensure_console_session()?;
        self.clear_keychain_helper()?;
        self.ensure_keychain(KeychainMode::Current)?;
        let resume_background = self.paths.run_marker.exists();
        if resume_background {
            self.ensure_keychain(KeychainMode::BackgroundInteractive)?;
        }
        self.ensure_storage()?;
        ensure!(self.vm_exists()?, "VM does not exist");
        validate_bridge()?;
        if resume_background {
            match self.vm_info()?.state {
                VmState::Running => {}
                VmState::Suspended | VmState::Stopped => {
                    self.start_service()?;
                    self.wait_for_ssh(300)?;
                }
            }
        }
        let mut signals = Signals::new(TERM_SIGNALS.iter().copied().chain([SIGHUP]))?;
        let console = (|| {
            ensure_console_session()?;
            self.prepare_console(resume_background)?;
            ensure_console_session()?;
            ensure!(
                signals.pending().next().is_none(),
                "console launch was interrupted"
            );
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
                    ensure_console_session()?;
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
                    "background restart was withheld to avoid a cold boot; inspect the VM, then run 'gremvm start' when ready",
                ))
            } else {
                Err(error)
            };
        }
        if resume_background {
            touch(&self.paths.run_marker)?;
            self.start_service()?;
            println!("running: admin@{}", self.wait_for_ssh(300)?);
        }
        Ok(())
    }

    fn status(&self) -> Result<()> {
        if !self.vm_exists()? {
            println!("state: not-provisioned");
            return Ok(());
        }
        let vm = self.vm_info()?;
        let ip = match vm.state {
            VmState::Running => self.vm_ip(0).ok(),
            VmState::Suspended | VmState::Stopped => None,
        };
        let state = match (vm.state, ip.as_deref(), self.paths.run_marker.exists()) {
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
        let stop = self.stop_vm();
        remove_if_present(&self.paths.launch_agent)?;
        if fs::symlink_metadata(&self.paths.command_link)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
            && fs::read_link(&self.paths.command_link)? == self.paths.bin("gremvm")
        {
            remove_if_present(&self.paths.command_link)?;
        }
        remove_if_present(&self.paths.runtime)?;
        stop?;
        println!(
            "uninstalled; VM data preserved at {}",
            self.tart_home()
                .join("vms")
                .join(&self.config.vm_name)
                .display()
        );
        Ok(())
    }

    fn internal_run(&self) -> Result<()> {
        if !self.paths.run_marker.exists() || !self.paths.provisioned.exists() {
            return Ok(());
        }
        self.ensure_keychain(KeychainMode::Background)?;
        self.ensure_storage()?;
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
        "another gremvm management command is running"
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

fn ensure_graphical_session(command: &str) -> Result<()> {
    ensure!(
        launchd_session()? == "Aqua",
        "gremvm {command} requires an active graphical host session"
    );
    Ok(())
}

fn ensure_console_session() -> Result<()> {
    ensure_graphical_session("console")?;
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
        "gremvm console requires this user's on-console graphical session to be unlocked so macOS can encrypt the VM snapshot"
    );
    Ok(())
}

fn service_target() -> String {
    format!("{}/{LABEL}", user_domain())
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
