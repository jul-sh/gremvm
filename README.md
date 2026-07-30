# GremVM

An agent needs a computer.

You can give it your personal Mac, but then it can damage your files, credentials, and environment. You can create a separate user account, but Macs do not handle multi-tenancy well for development work. You can buy another Mac, but now you need another Mac. A VM is the natural middle ground: isolated, snapshotable, and disposable.

The desired abstraction is simple: an extra Mac on your local network.

It should have its own IP address. You should be able to SSH or Screen Share into it. It should keep running when you disconnect and recover when it stops. The fact that it is a VM should mostly disappear.

## Behavior

- After `gremvm start`, the VM continues running after SSH disconnects and restarts after guest shutdowns or Tart failures.
- Normal Screen Sharing does not change the VM lifecycle.
- The guest keeps a 1512x982-pixel virtual display in background mode.
- A graphical login on the host is not required.

## Requirements

- A macOS host with Apple silicon
- Nix with flakes enabled

## Install

From the repository root, for example, assuming `/Volumes/BuildVM/gremvm` already exists:

```sh
nix run . -- install gremvm \
  --cpu-count 8 \
  --memory-gb 32 \
  --disk-gb 500 \
  --guest-user builder \
  --ask-password \
  --storage /Volumes/BuildVM/gremvm
```

This creates the VM and persistent `gremvm` command. The name and options are saved for the VM. Omit `--ask-password` to generate the initial guest password, and omit `--storage` to use Tart's default location under `~/.tart`. The password is stored in the host login Keychain rather than in the configuration file or command line.

Each VM stores its own settings: `gremvm` under `~/Library/Application Support/GremVM/config`, and other names under `~/Library/Application Support/GremVM/instances/<name>/config`. Hardware, guest user, storage, and password are fixed for that VM after creation; a new name can use different settings. Editing the files directly is unsupported.

On the first run, the Nix installer configures the tooling and service, downloads the pinned image, and creates the VM. It leaves the VM stopped; run `gremvm start` when you want to start it. Creation can take several minutes. If it is interrupted, rerun the same command and GremVM will safely retry the incomplete installation.

### Starting from SSH

After a host reboot without a graphical login, SSH into the host and run:

```sh
gremvm start
```

## Commands

```sh
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

## Storage

Omitting `--storage` leaves `TART_HOME` unset, so Tart uses its normal location under `~/.tart`. When `--storage` is present, its value is the exact Tart home: for example, `--storage /Volumes/Work/gremvm` stores VM data under `/Volumes/Work/gremvm/vms`.

`--storage` accepts one thing: an existing absolute directory. These are all valid when the directories already exist and are writable:

```sh
--storage /Users/me/vms
--storage /Volumes/Work
--storage /Volumes/Work/gremvm
```

## Removal

`gremvm uninstall` stops and deletes the VM, then removes the service definition, runtime link, and `~/.local/bin/gremvm`. It preserves the configuration, SSH key, Keychain credentials, and logs. Reinstall with `nix run . -- install gremvm` and the same options.

To run multiple VMs, install another name—for example, `nix run . -- install foovm`; that VM is then managed with `foovm start`, `foovm status`, and the other commands.

## Development

```sh
nix develop
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
nix flake check
```
