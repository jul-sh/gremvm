# GremVM

An agent needs a computer.

You can give it your personal Mac, but then it can damage your files, credentials, and environment. You can create a separate user account, but Macs do not handle multi-tenancy well for development work. You can buy another Mac, but now you need another Mac. A VM is the natural middle ground: isolated, snapshotable, and disposable.

The desired abstraction is simple: an extra Mac on your local network.

It should have its own IP address. You should be able to SSH or Screen Share into it. It should keep running when you disconnect and recover when it stops. The fact that it is a VM should mostly disappear.

GremVM approximates this with [Tart](https://tart.run/): a persistent macOS Tahoe VM, supervised by the host and exposed directly to the LAN. It is intended to feel like an always-on Mac for agentic work, without requiring another physical machine.

## Behavior

- The VM does not start merely because the host rebooted or a user logged in through SSH or the GUI. Run `gremvm start` explicitly.
- After `gremvm start`, the VM continues running after SSH disconnects and restarts after guest shutdowns or Tart failures.
- Normal Screen Sharing does not change the VM lifecycle.
- Opening Tart's recovery console preserves the live guest session.
- The guest keeps a 1512x982-pixel virtual display in background mode.
- The guest appears as a normal machine on the LAN, reachable through SSH and Screen Sharing.
- A graphical login on the host is not required.

## Requirements

- A macOS host with Apple silicon
- Nix with flakes enabled
- `~/.local/bin` in the user's shell `PATH`
- An existing login Keychain for the host account
- An existing writable directory when using `--storage`; encrypted non-system volumes must use APFS
- An active `en0` interface connected to the LAN
- Enough host CPU, memory, and storage for the configured VM, including disk growth and a suspend image that can approach the configured guest memory

On macOS 15 and later, Tart requires the host account's login Keychain to be unlocked when a VM starts. See [Tart's headless-machine guidance](https://tart.run/faq/#headless-machines).

## Install

From the repository root:

```sh
nix run . -- install
```

This creates the VM and persistent `gremvm` command. To change the defaults, pass the desired flags to `install`; those choices are saved for the VM.

On the first run, `install` configures the tooling and service, downloads the pinned image, and creates the VM. It leaves the VM stopped; run `gremvm start` when you want to start it. Creation can take several minutes. If it is interrupted, rerun the same command and GremVM will safely retry the incomplete installation.

Rerunning `install` updates and verifies the managed tooling without rebuilding the VM. It also leaves the VM stopped, so start it explicitly afterward.

On macOS 15 and later, Packer may trigger a one-time Local Network privacy prompt. For the first installation without a graphical session, apply [Tart's documented noninteractive workaround](https://tart.run/faq/#avoiding-the-local-network-permission-pop-up) before running `install`.

### Starting from SSH

After a host reboot without a graphical login, SSH into the host and run:

```sh
gremvm start
```

An SSH or GUI login alone does not start the VM. `start` explicitly loads the service into `user/<uid>`, starts it, and waits for guest SSH. Once it succeeds, disconnecting from the host does not stop the VM.

[Apple-silicon Macs on macOS 26 or later can unlock FileVault over SSH](https://support.apple.com/guide/deployment/intro-to-filevault-dep82064ec40/web) when Remote Login and a supported network connection are available. After the host finishes booting, run `gremvm start` as above.

If the host login Keychain needs unlocking, `start` asks for the host account password in the invoking terminal with input hidden. GremVM never stores that password or passes it on the command line, and the long-running service never prompts. Use `ssh -t` when invoking `start` as a one-shot SSH command.

## Configuration

Configuration is accepted as flags to `install`:

| Flag | Default | Accepted values |
| --- | --- | --- |
| `--cpu-count` | `6` | 1–64 |
| `--memory-gb` | `24` GiB | 4–256 GiB |
| `--disk-gb` | `192` GB | 50 GB or more |
| `--guest-user` | `admin` | 1–32 lowercase letters, numbers, underscores, or hyphens; starts with a letter and is not a reserved macOS account |
| `--ask-password` | Generated password | Prompt twice for the initial guest password with input hidden |
| `--storage` | Tart default (`~/.tart`) | Existing, writable absolute directory |

For example, assuming `/Volumes/BuildVM/gremvm` already exists:

```sh
nix run . -- install \
  --cpu-count 8 \
  --memory-gb 32 \
  --disk-gb 500 \
  --guest-user builder \
  --ask-password \
  --storage /Volumes/BuildVM/gremvm
```

Without `--ask-password`, GremVM generates the initial guest password. Either way, it stores the password in the host login Keychain rather than in the configuration file or command line.

Settings are saved in `~/Library/Application Support/GremVM/config/config.json`. The hardware, guest user, storage path, and guest password are creation-time choices. Subsequent installs must use the same settings, and GremVM does not reconfigure an existing VM or rotate its password. Editing the configuration file is unsupported.

## Commands

```sh
gremvm install [options]
gremvm status
gremvm ssh
gremvm ssh sw_vers
gremvm screen-share
gremvm console
gremvm tailscale setup
gremvm tailscale status
gremvm stop
gremvm start
gremvm restart
gremvm logs --follow
gremvm uninstall
```

`stop` unloads the user-domain service and stops the VM. Automatic restart remains disabled until `start` is called. `restart`, `status`, `ssh`, and `logs` work from SSH.

## Guest Screen Sharing and the recovery console

The guest receives its own DHCP address on the `en0` network, which `status` reports when available. There is no NAT or port-forwarding layer.

The pinned base image enables Screen Sharing. Because the connection goes directly to the guest's bridged IP, the host does not need a graphical login. From another Mac:

```sh
open 'vnc://GUEST_IP'
```

From a graphical session on the host, the convenience command resolves the same guest IP and opens it in macOS Screen Sharing:

```sh
gremvm screen-share
```

Opening or closing Screen Sharing does not start, stop, suspend, or restart the VM. Use it for normal desktop access.

Retrieve the guest password from the host Keychain, replacing `<guest-user>` with the configured guest user:

```sh
security find-generic-password \
  -a '<guest-user>' \
  -s io.gremvm.tart.gui-password \
  -w
```

The password is used for the guest account and its login Keychain. The configured user logs in automatically after boot; the password is still required to authenticate a Screen Sharing connection. GremVM does not support password rotation after VM creation because the host and guest copies must remain synchronized.

`console` is the recovery path for guest boot, networking, or Screen Sharing failures. It opens Tart's local console on the host and must be invoked from that user's unlocked, on-console graphical session. It returns a clear error from SSH, a locked session, or another Background session. GremVM uses [Tart's suspendable mode](https://tart.run/blog/2023/09/20/tart-200-and-community-updates/) to save the running VM, resume the same session in the console, save it again when the console closes, and restore background supervision when it was previously enabled. It prevents idle display and system sleep while the console is open because macOS needs the unlocked graphical session to encrypt the saved state. Do not manually lock or log out of the host until the handoff completes.

An already running guest does not reboot during a successful handoff; a stopped VM has no live state to preserve and cold-boots. If Tart cannot produce a verified saved state, GremVM leaves automatic background restart disabled rather than silently cold-booting the guest. Inspect the error and run `gremvm start` when a cold boot is acceptable.

Security note: initial VM setup disables the macOS application firewall in the guest. Any enabled service bound to a non-loopback interface is directly reachable from the LAN, subject only to network-level filtering or client isolation. Automatic login requires guest FileVault to remain off and stores a reversible login secret in the guest's `/etc/kcpassword` file.

The `ssh` command uses the dedicated private key stored on the host. To connect over SSH from another computer, add that computer's public key to `/Users/<guest-user>/.ssh/authorized_keys` in the guest.

## Tailscale

Tailscale access is optional and runs directly inside the guest. GremVM uses Tailscale's open-source, CLI-only macOS daemon: there is no guest application, menu-bar item, system-extension approval, or dependency on a graphical login. The existing bridged LAN connection remains unchanged.

With the VM running, install or upgrade the pinned guest daemon and join a tailnet:

```sh
gremvm tailscale setup
```

If the guest is not already authorized, the command prints a Tailscale authentication URL and waits. Open it on any computer and approve the guest. Pressing Control-C is safe; rerun the command to continue. GremVM derives the tailnet hostname from the configured VM name and stores no Tailscale credential. Tailscale keeps its node identity inside the guest, and its system daemon starts before login and restarts if it exits.

Show the stable remote address and connection commands with:

```sh
gremvm tailscale status
```

On another Mac, install the same [CLI-only variant](https://github.com/tailscale/tailscale/wiki/Tailscaled-on-macOS) if it is not already connected:

```sh
brew install --formula tailscale
sudo brew services start tailscale
sudo tailscale up
```

Then connect to the guest from that Mac:

```sh
ssh '<guest-user>@100.x.y.z'
open 'vnc://100.x.y.z'
```

Use the configured guest user and reported address in place of the placeholders. The other Mac's public SSH key must be present in the guest as described above, and Screen Sharing still authenticates with the guest password. A restrictive tailnet policy must permit TCP ports 22 and 5900 to the guest. That policy governs the Tailscale path only; the guest remains directly exposed to its bridged LAN. MagicDNS can provide a name, but GremVM always reports the numeric address because the CLI-only macOS variant does not configure macOS DNS itself.

CLI-only Tailscale does not update itself. GremVM ships a version pinned by `flake.lock`; after updating GremVM, rerun `gremvm tailscale setup` to upgrade the guest without changing its node identity. For durable unattended access, disable key expiry for this device or assign it a [tag in the Tailscale admin console](https://tailscale.com/docs/features/tags). To disconnect or forget the guest, run `gremvm ssh /usr/local/bin/tailscale down` or `gremvm ssh /usr/local/bin/tailscale logout`; `setup` reconnects it. `gremvm uninstall` preserves the VM and therefore preserves its Tailscale installation and identity.

## Storage

Omitting `--storage` leaves `TART_HOME` unset, so Tart uses its normal location under `~/.tart`. When `--storage` is present, its value is the exact Tart home: for example, `--storage /Volumes/Work/gremvm` stores VM data under `/Volumes/Work/gremvm/vms`.

`--storage` accepts one thing: an existing absolute directory. These are all valid when the directories already exist and are writable:

```sh
--storage /Users/me/vms
--storage /Volumes/Work
--storage /Volumes/Work/gremvm
```

There are no volume-name shorthands, prefixes, or separate storage modes to choose. GremVM canonicalizes and saves the exact directory, then detects the filesystem beneath it:

- A directory on the system disk is used directly.
- A directory on an unencrypted non-system volume is tied to that volume's UUID and recorded mount point. GremVM mounts the volume by UUID when necessary, then uses the saved directory within it.
- A directory on an encrypted non-system volume must be on encrypted APFS. On the first interactive install or start, GremVM asks for the volume password with input hidden, verifies it, and saves it in the host login Keychain. The background service can then unlock and mount the volume without a graphical login or interactive prompt.

At each use, GremVM verifies that the recorded volume has the same identity, mount point, encryption state, and writable storage directory. It never creates a missing storage directory, substitutes a volume with the same name, interprets a relative path, or falls back to the system disk.

Tart's raw disk image is sparse: `--disk-gb` sets its virtual capacity, while physical usage grows as the guest writes data and may eventually approach that capacity.

## Removal

`gremvm uninstall` removes the service definition, runtime link, and `~/.local/bin/gremvm`. It preserves the configuration, SSH key, Keychain credentials, logs, and VM data in the selected storage location. Reinstall with `nix run . -- install` and the same options.

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
nix flake check
```

To run multiple VMs, add a name when installing—for example, `nix run . -- install foovm` creates an independent VM managed with `foovm start`, `foovm status`, and the same remaining subcommands.
