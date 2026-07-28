# GremVM

An agent needs a computer.

You can give it your personal Mac, but then it can damage your files, credentials, and environment. You can create a separate user account, but Macs do not handle multi-tenancy well for development work. You can buy another Mac, but now you need another Mac. A VM is the natural middle ground: isolated, snapshotable, and disposable.

The desired abstraction is simple: one more Mac on your local network.

It should have its own IP address. You should be able to SSH or Screen Share into it. It should keep running when you disconnect and recover when it stops. The fact that it is a VM should mostly disappear.

GremVM approximates this with [Tart](https://tart.run/): a persistent macOS Tahoe VM, supervised by the host and exposed directly to the LAN. It is intended to feel like an always-on Mac for agentic work, without requiring another physical machine.

## Behavior

- After a host reboot, SSH into host and run `gremvm start`.
- The VM continues running after SSH disconnects and restarts after guest shutdowns or Tart failures.
- The guest appears as a normal machine on the LAN, reachable through SSH and Screen Sharing.
- A graphical login on the host is not required.

## Requirements

- A macOS host with Apple silicon
- Nix with flakes enabled
- `~/.local/bin` in the user's shell `PATH`
- An existing login Keychain for the host account
- An encrypted APFS volume only when using `--volume-name`
- An active `en0` interface connected to the LAN
- At least the configured CPU count and more memory than the guest allocation
- Enough host storage for provisioning data and subsequent VM disk growth

On macOS 15 and later, Tart requires the host account's login Keychain to be unlocked when a VM starts. See [Tart's headless-machine guidance](https://tart.run/faq/#headless-machines).

## Install

From the repository root:

```sh
nix run .#gremvm -- install
gremvm provision
```

`install` writes the configuration, creates credentials, registers the Nix bundle as a garbage-collection root, and installs the per-user service definition. `provision` downloads the pinned image, creates the VM, starts it, and waits for SSH. Provisioning can take several minutes and requires an uninterrupted network connection.

On macOS 15 and later, Packer may trigger a one-time Local Network privacy prompt. For provisioning without a graphical session, apply [Tart's documented noninteractive workaround](https://tart.run/faq/#avoiding-the-local-network-permission-pop-up) before running `provision`.

Installation creates the persistent user command `~/.local/bin/gremvm`, which points to the managed runtime under `~/Library/Application Support/GremVM`.

### Starting from SSH

After a host reboot without a graphical login, SSH into the host and run:

```sh
gremvm start
```

An SSH login alone does not start the VM. `start` explicitly loads the service into `user/<uid>`, starts it, and waits for guest SSH. Once it succeeds, disconnecting from the host does not stop the VM.

[Apple-silicon Macs on macOS 26 or later can unlock FileVault over SSH](https://support.apple.com/guide/deployment/intro-to-filevault-dep82064ec40/web) when Remote Login and a supported network connection are available. After the host finishes booting, run `gremvm start` as above.

If the host login Keychain needs unlocking, `start` asks for the host account password in the invoking terminal with input hidden. GremVM never stores that password or passes it on the command line, and the long-running service never prompts. Use `ssh -t` when invoking `start` as a one-shot SSH command.

## Configuration

Configuration is accepted as `install` flags:

| Flag | Default | Accepted values |
| --- | --- | --- |
| `--vm-name` | `gremvm` | 1–64 letters, numbers, dots, underscores, or hyphens; starts with a letter or number |
| `--cpu-count` | `6` | 1–64 |
| `--memory-gb` | `24` GiB | 4–256 GiB |
| `--disk-gb` | `192` GB | 50–350 GB |
| `--volume-name` | Tart default (`~/.tart`) | Existing encrypted APFS volume; same name syntax as the VM |

For example:

```sh
nix run .#gremvm -- install \
  --vm-name build-vm \
  --cpu-count 8 \
  --memory-gb 32 \
  --disk-gb 250 \
  --volume-name BuildVM
```

The first install saves settings in `~/Library/Application Support/GremVM/config/config.json`. Subsequent installs must use the same settings, and GremVM does not reconfigure an existing VM. Editing this file is unsupported.

## Commands

```sh
gremvm status
gremvm ssh
gremvm ssh sw_vers
gremvm gui
gremvm stop
gremvm start
gremvm restart
gremvm logs --follow
gremvm uninstall
```

`stop` removes the run marker, unloads the user-domain service, and stops the VM. Automatic restart remains disabled until `start` is called. `restart`, `status`, `ssh`, and `logs` work from SSH.

## Guest Screen Sharing and the host console

The guest receives its own DHCP address on the `en0` network, which `status` reports when available. There is no NAT or port-forwarding layer.

The pinned base image enables Screen Sharing. Because the connection goes directly to the guest's bridged IP, the host does not need a graphical login. From another Mac:

```sh
open 'vnc://GUEST_IP'
```

Retrieve the `admin` password from the host Keychain:

```sh
security find-generic-password -a admin -s io.gremvm.tart.gui-password -w
```

The generated password is stored in the host Keychain and used for both guest login and the guest login Keychain. Password rotation is unsupported; the host and guest copies must remain synchronized.

`gremvm gui` is different: it opens Tart's console on the host. It must be invoked from a graphical host session and returns a clear error from SSH or another Background session, even if the same account is logged in graphically elsewhere. When available, it temporarily stops the background instance and restores background supervision when the console closes if the VM was previously enabled.

Security note: provisioning disables the macOS application firewall in the guest. Any enabled service bound to a non-loopback interface is directly reachable from the LAN, subject only to network-level filtering or client isolation.

The `ssh` command uses the dedicated private key stored on the host. To connect over SSH from another computer, add that computer's public key to `/Users/admin/.ssh/authorized_keys` in the guest.

## Storage

Without `--volume-name`, GremVM leaves `TART_HOME` unset and Tart stores data normally under `~/.tart`. With `--volume-name BuildVM`, Tart's complete home is stored under `/Volumes/BuildVM`, including VM data in `/Volumes/BuildVM/vms`. A configured volume must already exist as encrypted APFS, and its password must be saved in the host login Keychain. Installation records its APFS UUID so a different volume with the same name cannot be substituted later.

Before starting or provisioning a VM with configured volume storage, GremVM unlocks the login Keychain when necessary, asks macOS to mount that volume by UUID, and verifies that the exact `/Volumes/<name>` mount is encrypted APFS and writable by the current user. A missing or incorrect mount is an error; GremVM never creates a similarly named directory or falls back to the host system volume. The VM is created directly on the configured volume.

Tart's raw disk image is sparse: `--disk-gb` sets its virtual capacity, while physical usage grows as the guest writes data and may eventually approach that capacity.

## Removal

`uninstall` removes the service definition, runtime link, and `~/.local/bin/gremvm`. It preserves the configuration, SSH keys, guest password in Keychain, logs, and VM data in the selected storage location.

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
nix flake check
```
