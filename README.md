# GremVM

GremVM is a small deployment layer around upstream [Lume](https://cua.ai/docs/how-to-guides/lume/install-lume). Lume owns the persistent macOS VM, Recovery environment, and Virtualization.framework integration. GremVM only pins and verifies Lume, starts it after the owning host account logs in, stops it cleanly, and creates stopped VM clones for backup.

The deployment is intentionally opinionated. There is no GremVM configuration file: it always manages one VM named `work` at `~/Library/Application Support/GremVM/vms/work`, with 4 CPUs, 8 GB memory, a 150 GB disk, NAT networking, and a 1920×1200 display. Environment variables cannot change those choices.

Lume 0.4.0 is pinned in [`versions/lume.env`](versions/lume.env). GremVM verifies the archive checksum, upstream signature, Developer ID team, bundle ID, hardened runtime, Gatekeeper acceptance, and version; it never uses Lume's floating installer or self-updater.

## Host boundary

The VM starts after the owning host account logs in. macOS requires that account's login keychain to use Virtualization.framework.

- GremVM never reads or changes host FileVault or automatic-login settings.
- With FileVault enabled, someone must unlock the Mac and log in after a cold boot.
- With FileVault disabled but no host login, the VM stays stopped.
- Locking the host screen is fine; logging out stops the VM.

## Install and provision

Requirements: an Apple-silicon Mac running macOS 26 (Tahoe), Nix with flakes, an enabled macOS Application Firewall, enough storage for the VM, and one local setup session.

```sh
cd /Users/julsh/git/gremvm
nix develop path:.
./bin/gremvm install

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` is idempotent. It downloads the pinned Lume release, disables Lume telemetry, installs a deny-inbound Application Firewall rule for Lume's VNC listener, and installs a private user LaunchAgent. It also creates `~/.local/bin/gremvm` when that path is unused.

`provision` asks Lume for Tahoe's current supported restore image, creates the fixed VM, disables SIP through paired Recovery, verifies clean shutdown access, and starts the supervisor. A SIP-disable failure stops provisioning rather than producing a VM with an ambiguous security state.

Lume's unattended setup initially creates the guest user `lume` with password `lume`, enables guest SSH and automatic login, and disables guest sleep/lock. These are guest-only settings. Before adding work data or remote access, open the console and change that password:

```sh
"$GREMVM" console
```

Follow [`docs/REMOTE_ACCESS.md`](docs/REMOTE_ACCESS.md) for the private guest-access setup.

## Commands

```sh
"$GREMVM" status
"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" console
"$GREMVM" logs
"$GREMVM" backup
"$GREMVM" uninstall
```

`logs` prints the most recent 200 supervisor log lines. For a live tail, use macOS directly:

```sh
tail -F "$HOME/Library/Application Support/GremVM/logs/vm.log"
```

The LaunchAgent restarts a failed Lume runner once provisioning is complete. On logout and host shutdown, its supervisor requests a guest-clean shutdown through a host-key-pinned, shutdown-only SSH key and then reaps the recorded runner. If that evidence cannot be established, interactive `stop` reports the fallback rather than silently claiming a clean shutdown.

`uninstall` removes the LaunchAgent, wrapper, and vendored Lume runtime. It preserves the VM, shutdown key, logs, and all backups. There is no purge command.

## Backups

GremVM has one backup destination by design: `/Volumes/GremVM Backup/lume`. Format a dedicated external SSD as APFS (Encrypted), mount it as `GremVM Backup`, and create the directory once:

```sh
mkdir -m 700 "/Volumes/GremVM Backup/lume"
"$GREMVM" backup
```

`backup` refuses a missing, unwritable, symbolic-link, or same-volume destination. It cleanly stops the VM, creates a complete Lume clone, verifies the required files, writes a completion manifest last, and starts the VM again. Existing backups are never pruned. Use the restic 3-2-1 policy and restore drill in [`docs/BACKUPS.md`](docs/BACKUPS.md) for encrypted off-site history.

## Development

```sh
nix develop path:. -c ./scripts/check.sh
```

The checks cover shell formatting/linting, Nix evaluation, lifecycle-state behavior, the exact Lume pin, and absence of personal signing material. Provisioning, firewall reachability, cold boot, and restore still require acceptance tests on the actual Mac Studio.
