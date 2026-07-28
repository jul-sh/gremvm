# GremVM

An agent needs a computer.

You can give it your personal Mac, but then it can damage your files, credentials, and environment. You can create a separate user account, but Macs do not handle multi-tenancy well for development work. You can buy another Mac, but now you need another Mac. A VM is the natural middle ground: isolated, snapshotable, and disposable.

The desired abstraction is simple: one more Mac on your local network.

It should have its own IP address. You should be able to SSH or Screen Share into it. It should keep running when you disconnect and recover when it stops. The fact that it is a VM should mostly disappear.

GremVM bundles the pinned [Lume 0.4.0 release binary](https://github.com/trycua/cua/releases/tag/lume-v0.4.0); its corresponding [source revision](https://github.com/trycua/cua/tree/ee15ae942cefe809fd97a565220eca9c6a295ac0/libs/lume) is linked for inspection. It manages one persistent macOS Tahoe VM as an always-on Mac for agentic work.

## Behavior

- After a host reboot, SSH into the host and run `gremvm start`.
- A per-user `Background` service works with either an SSH-only login or a graphical login and survives the SSH connection closing.
- While the host login Keychain remains unlocked, the service restarts the VM after a guest shutdown, a Lume failure, or loss of its desktop tunnel.
- Lume runs with `--no-display`, but the guest retains a 1512x982 logical display for Screen Sharing and UI automation.
- The guest has one bridged connection on `en0`, with its own DHCP address on the LAN. There is no NAT variant.
- Opening or closing Screen Sharing does not change the VM lifecycle.

## Requirements

- An Apple silicon Mac running macOS 14 or newer that supports the pinned Tahoe 26.6 restore image
- Nix with flakes enabled
- `~/.local/bin` in the user's shell `PATH`
- An active `en0` interface connected to the LAN
- At least the configured CPU count and more memory than the guest allocation
- Enough storage for the restore image and the VM's growing sparse disk
- For `--volume-name`: an existing encrypted APFS volume and its password

The bridged guest and its VNC endpoint are intended for a trusted LAN. See [Guest desktop access](#guest-desktop-access) for the security boundary.

## Install

From the repository root:

```sh
nix run .#gremvm -- install
gremvm provision
```

`install` saves the configuration, creates a dedicated guest SSH key, generates the guest password in the host Keychain, registers the packaged runtime as a Nix garbage-collection root, installs `~/.local/bin/gremvm`, and writes the per-user service definition. It does not create a VM.

`~/.local/bin/gremvm` is the persistent entry point in every shell on the account; no shell-specific alias is needed.

`provision` downloads and verifies the pinned macOS Tahoe 26.6 restore image, creates and configures the VM, starts supervision, and waits for both guest SSH and the desktop tunnel. Provisioning can take a while.

## Configuration

Configuration is accepted only as `install` flags:

| Flag | Default | Accepted values |
| --- | --- | --- |
| `--vm-name` | `gremvm` | 1–64 letters, numbers, dots, underscores, or hyphens; starts with a letter or number |
| `--cpu-count` | `6` | 1–64 |
| `--memory-gb` | `24` GiB | 4–256 GiB |
| `--disk-gb` | `192` GiB | 50–350 GiB |
| `--volume-name` | none (`~/.lume`) | Existing encrypted APFS volume; same name syntax as the VM |

For example:

```sh
nix run .#gremvm -- install \
  --vm-name build-vm \
  --cpu-count 8 \
  --memory-gb 32 \
  --disk-gb 250 \
  --volume-name BuildVM
gremvm provision
```

The first install saves settings in `~/Library/Application Support/GremVM/config/config.json`. Later installs must use the same settings, and GremVM does not reconfigure an existing VM. Editing the file by hand is unsupported.

## Host Keychain

On recent macOS versions, Lume may need the host login Keychain to be unlocked. GremVM checks the current command's Keychain context before `start`, `provision`, and credential access. Before starting supervision, it also checks the `Background` user-domain context in which the service will run.

If either context is locked and the command is running interactively, macOS prompts for the host password with terminal echo disabled. The password is never put on a command line or saved by GremVM. The long-running supervisor only checks; it never prompts and instead reports that `gremvm start` must be run interactively.

When starting through SSH, allocate a terminal so a prompt can be shown if needed:

```sh
ssh -t HOST '$HOME/.local/bin/gremvm start'
```

When optional volume storage is configured, GremVM separately asks for that APFS volume's password with echo disabled. It verifies the password without changing the volume state and saves it in the login Keychain under service `io.gremvm.volume-password`, keyed by the volume UUID. The password is sent to `diskutil apfs unlockVolume` through stdin; it is never stored in configuration, a file, an environment variable, or a process argument. The background service reads the saved password only after its login-Keychain context is unlocked and never prompts.

The host login password and optional APFS volume password are different credentials. Tahoe's SSH/FileVault unlock support applies to the host's boot volume, not a separate VM storage volume.

GremVM does not weaken the login Keychain's auto-lock policy. If that Keychain locks later, a future automatic Lume restart cannot proceed until `gremvm start` unlocks it again; `gremvm logs` reports this condition.

## Commands

```sh
gremvm status
gremvm start
gremvm stop
gremvm restart
gremvm ssh
gremvm ssh sw_vers
gremvm screen-share
gremvm screen-share --url
gremvm console
gremvm logs
gremvm logs --follow
gremvm uninstall
```

- `status` reports lifecycle state, address, resources, storage, and network details.
- `start` enables supervision, starts the guest if needed, and waits for the desktop tunnel.
- `stop` disables supervision and stops the guest. It remains stopped until `start` is called.
- `restart` performs an intentional stop and supervised start.
- `ssh` connects as `admin` with the dedicated key; trailing arguments run a guest command.
- `screen-share` opens the guest desktop from a graphical session on the host.
- `screen-share --url` prints a connection URL for another Mac without opening an app.
- `console` opens Lume's host-local recovery console without changing VM lifecycle.
- `logs` prints the last 200 supervisor log lines; `--follow` continues streaming them.
- `uninstall` removes host integration while preserving VM data and credentials.

Every command except the two commands that open a local app, `screen-share` and `console`, works from an SSH-only host session. `screen-share --url` also works over SSH.

## Guest desktop access

The native macOS Screen Sharing service inside a Virtualization.framework guest cannot capture Lume's framebuffer, so GremVM does not use it for the guest desktop. Instead, the supervisor reads Lume's host-local VNC endpoint and opens an SSH reverse forward from the host into the guest:

```text
guest LAN address:5900 -> SSH tunnel -> Lume VNC on the host
```

This keeps Lume's headless framebuffer available at the guest's LAN address. The tunnel is part of supervision: if it exits, the supervisor stops the stale runtime and launchd starts it again while the run marker exists.

From a graphical session on the host:

```sh
gremvm screen-share
```

From another Mac, ask the host for the current URL:

```sh
ssh -t HOST '$HOME/.local/bin/gremvm screen-share --url'
```

Then open the returned URL on that Mac:

```sh
open 'RETURNED_VNC_URL'
```

The URL contains a random, per-run VNC credential. It is not the `admin` password; treat the complete URL as a secret and fetch it again after a VM restart.

VNC between the client Mac and `guest-address:5900` is authenticated but not encrypted. The guest-to-host leg is carried by SSH. Use this only on a trusted LAN; do not forward port 5900 to the internet. Provisioning also disables the guest application firewall so other guest services bound to non-loopback addresses are reachable from that LAN.

## Recovery console

`gremvm console` opens the already-running VM through Lume's host-local VNC endpoint. It requires a graphical login on the host. The command announces that it is waiting for Lume's console and may wait up to two minutes before opening a fresh Screen Sharing window.

The console is an attachment only and does not change the VM lifecycle. Closing the window leaves the VM and supervision running.

## Guest credentials

The generated guest account is `admin`. It logs in automatically after boot, and SSH password authentication is disabled in favor of the dedicated key. The guest password is stored in the host Keychain under service `io.gremvm.guest-password` and is also used for the guest login Keychain.

Retrieve it on the host with:

```sh
security find-generic-password -a admin -s io.gremvm.guest-password -w
```

Guest password rotation is unsupported because the host Keychain and guest copies must remain synchronized. To connect over SSH from another computer, add that computer's public key to `/Users/admin/.ssh/authorized_keys` in the guest.

Automatic login requires guest FileVault to remain off and stores a reversible login secret in the guest's `/etc/kcpassword` file.

## Storage

Without `--volume-name`, the storage root is `~/.lume`. With `--volume-name BuildVM`, it is the private mode-0700 directory `/Volumes/BuildVM/GremVM`. GremVM directs Lume's VM home and cache to the selected root and stages the restore image there, so the VM and provisioning artifacts are created on the selected storage rather than copied there later. The staged restore image is removed after successful provisioning.

For volume storage, installation records the encrypted APFS volume's UUID and saves its verified password in the login Keychain. Before provisioning or starting, GremVM unlocks that UUID and verifies its name, UUID, encryption, mount point, and writability. A missing or incorrect mount is an error; GremVM never creates a similarly named directory or falls back to the system volume.

The VM disk is sparse. `--disk-gb` sets its maximum virtual capacity, while physical use grows as the guest writes data and can eventually approach that capacity. GremVM does not preallocate or reserve host space.

If creation is interrupted before Lume finishes the VM files, `status` reports `provisioning` while Lume's marker remains and `incomplete` otherwise. `install` and `provision` print the exact directory rather than deleting data. Confirm no Lume process is using that directory, remove it deliberately, and run `gremvm provision` again. If Lume finished creating the VM, `provision` resumes guest setup or verifies its durable guest marker automatically.

## Lifecycle supervision

`start` creates a run marker and explicitly bootstraps `io.gremvm` in the user's `user/<uid>` launchd domain with session type `Background`. That domain is available without a graphical login, also works during a GUI login, and outlives the SSH connection that invoked `start`.

An SSH login by itself does not start the VM. After a host reboot, run `gremvm start`. While the run marker exists, launchd keeps the supervisor alive and it keeps Lume, guest SSH, and the desktop tunnel healthy. `stop` removes the marker and unloads the service before stopping the VM, so an intentional stop is not restarted.

## Removal

`gremvm uninstall` stops and unloads the service, then removes its definition, the managed runtime link, and `~/.local/bin/gremvm`. It preserves the saved configuration, SSH keys, Keychain credentials, logs, and VM data in the selected storage root.

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
nix flake check
```

The packaged compatibility target is Lume 0.4.0 with macOS Tahoe 26.6 build 25G72. Automated checks validate the CLI, guest setup script, package contents, Lume signature, and required entitlements without creating a VM. A real reboot, SSH-only start, auto-login, Screen Sharing, restart, and stop acceptance run is required whenever either pin changes.
