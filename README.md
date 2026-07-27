# GremVM

GremVM is a deliberately small wrapper around [Tart](https://tart.run/). Tart owns the VM, disk, NVRAM, networking, and virtualization process; GremVM only pins the inputs and wires one persistent VM named `work` into the host user's login session.

The VM lives at:

```text
~/Library/Application Support/GremVM/tart/vms/work
```

GremVM does only this:

- installs a Nix-pinned, upstream-signed Tart app;
- clones one pinned Cirrus Labs macOS image the first time it is provisioned;
- assigns that local clone a unique persistent MAC address;
- keeps the first, credential-hardening boot on Tart's private NAT;
- runs Tart after the owning host user logs in and restarts the runner if it exits;
- bridges the guest onto the host's LAN; and
- reports direct SSH and macOS Screen Sharing endpoints.

There is no SIP automation, recovery automation, tunnel, backup layer, or guest-management API. VM data is never deleted by a GremVM command.

## Requirements and boundaries

Use an Apple-silicon Mac with Nix and flakes. Tart runs through Apple's Virtualization.framework. On recent macOS versions the owning user must have a GUI login session with an unlocked login keychain, so the service starts **after login**, not before FileVault unlock. Locking the host screen is fine; logging out stops the user LaunchAgent.

The guest is a real peer on the LAN. The network must allow an additional bridged MAC address; client-isolated Wi-Fi, 802.1X networks, VPN default routes, and restrictive DHCP policies can prevent this. GremVM records the active physical `enN` default interface during installation. It rejects VPN, loopback, inactive, and nonexistent interfaces. Select a different physical interface with `gremvm bridge en0` (or the appropriate `enN`) and the service will restart on it.

The pinned Cirrus Labs vanilla image explicitly enables Remote Login and Screen Sharing. Its initial account is `admin` with password `admin`, auto-login, and passwordless sudo. Its disk also contains SSH host keys created before publication. GremVM therefore refuses to bridge a fresh clone immediately. It opens the first boot graphically on Tart's private NAT, requires the password and SSH host identity to be replaced and the guest shut down, then asks for an explicit confirmation before creating the `ready` lifecycle variant. The vanilla image has no trusted guest agent, so this confirmation prevents accidental exposure but cannot independently prove what happened inside the guest. The image is pinned by OCI digest, so a future `latest` image cannot silently change an existing deployment.

## Install and provision

```sh
cd /Users/julsh/git/gremvm
nix run path:. -- install

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` copies the complete signed `tart.app` supplied by the flake, because Tart must run inside its app bundle to use its embedded provisioning profile. The installed service does not depend on an open Nix shell or a live Nix store path.

`provision` clones the pinned image once and then opens its local setup window. In the guest Terminal, run the commands printed by GremVM to delete and regenerate `/etc/ssh/ssh_host_*`, record the new Ed25519 fingerprint, and change `admin/admin` with `passwd`. Then shut down macOS. Back in the host Terminal, type the exact requested confirmation phrase. Only then does GremVM start the persistent bridged login service. Run this command from an interactive Terminal; a noninteractive invocation stops safely at the hardening phase.

Re-running `provision` preserves the local VM and disk. An interrupted clone resumes without overwriting a completed local clone. MAC randomization may safely repeat if the process was interrupted between that Tart operation and the atomic state transition. An unrelated same-named Tart VM is preserved and refused rather than adopted or overwritten.

After the VM starts, verify both services and print its current LAN address:

```sh
"$GREMVM" address
```

The result looks like:

```text
ip: 192.168.1.42
ssh: ssh admin@192.168.1.42
screen-sharing: vnc://admin@192.168.1.42
```

From another computer on the same LAN, use the printed SSH command or open the `vnc://` URL in macOS Screen Sharing. The address comes from Tart's ARP resolver because its ordinary DHCP resolver does not work with bridged networking. A DHCP reservation for the guest is useful if a stable address matters.

## Commands

```sh
"$GREMVM" status
"$GREMVM" address
"$GREMVM" ssh
"$GREMVM" screen

"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" bridge
"$GREMVM" bridge en0
"$GREMVM" logs
"$GREMVM" uninstall
```

`stop` stops the VM for the current login session. Because the LaunchAgent remains installed and the `ready` lifecycle variant remains present, the VM starts again the next time the owning user logs in. `uninstall` removes only the replaceable Tart runtime, wrapper, pin, and LaunchAgent; it preserves the Tart VM, lifecycle state, and logs.

Tart's normal root disk is read-write with full synchronization. GremVM never uses read-only or `sync=none` disk options. Tart's stop operation is destructive from the guest's perspective: it does not perform an in-guest graceful shutdown. APFS recovery protects ordinary persistence, but guest-consistent writes are not guaranteed. Shut down important guest workloads normally before host logout or an explicit `stop` when practical.

## Acceptance check

Before depending on the VM remotely:

1. From a second LAN computer, use `address`, confirm that SSH presents the Ed25519 fingerprint recorded in the local setup window, and verify authenticated SSH plus actual keyboard/mouse control through Screen Sharing.
2. Create a persistence marker in the guest, run `/bin/sync`, restart GremVM, and verify that the marker remains.
3. Log out of the host account, log back in, and confirm `status` returns to `running` without another `start` command.
4. Reboot with the host's real FileVault setting and confirm the documented login boundary is acceptable.

## Development

```sh
nix develop path:. -c ./scripts/check.sh
nix flake check path:.
nix build --no-link path:.#tart path:.#default

# Optional, after provisioning on the real host:
GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm" ./tests/lan-smoke.sh
```
