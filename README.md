# GremVM

GremVM is now a small deployment layer around upstream [Lume](https://cua.ai/docs/how-to-guides/lume/install-lume), not a home-grown hypervisor. Lume owns the macOS VM, Recovery environment, Virtualization.framework integration, and SIP policy. This repository pins and verifies one notarized Lume release, supplies login-time lifecycle wiring, and makes stopped backups.

The selected release is Lume 0.4.0. Its archive checksum, Cua signing team, bundle identifier, and source commit are fixed in [`versions/lume.env`](versions/lume.env). Upgrades are deliberate version/hash reviews; neither Lume's floating installer nor its self-updater is used.

## Why Lume

Lume is the only compared option with a first-class, verified SIP workflow:

```sh
lume sip off work --yes
```

It boots the VM's paired Recovery environment, runs `csrutil disable`, boots normally, verifies the canonical result, and leaves the VM stopped. Tart can boot Recovery, but disabling SIP there is still a manual console procedure. The full comparison and the requirements that necessarily changed are in [`docs/DECISION.md`](docs/DECISION.md).

## Honest host-start boundary

This replacement does **not** claim the old pre-login guarantee. On current macOS, Virtualization.framework VM startup depends on an unlocked user login keychain, and Tart documents the same underlying headless limitation. Lume has no built-in VM-autostart service (its optional LaunchAgent is for the unused API daemon), so GremVM supplies a user LaunchAgent that starts the VM after its owning host account logs in.

- Host FileVault is never read or changed.
- Host automatic login is never read or changed.
- With FileVault enabled, someone must unlock the Mac and log in after a cold boot.
- With FileVault disabled but no host login, the VM stays stopped.
- Locking the host screen is fine; logging the owning account out stops its LaunchAgent.

That trade is what removes the bespoke root daemon, service keychain, custom Virtualization.framework code, signing pipeline, and Apple developer secrets.

## Install and provision

Requirements: Apple silicon, macOS 26 or newer, at least 16 GiB host RAM recommended, 150 GiB free logical VM capacity, Nix with flakes, an enabled macOS Application Firewall, and one local setup session. This deployment deliberately qualifies only a macOS 26 (Tahoe) guest because that is Lume 0.4.0's currently verified unattended Recovery/SIP workflow. On a macOS 26 host, `GREMVM_IPSW=latest` selects Tahoe; on a newer host, configure an absolute path to a reviewed Tahoe IPSW before provisioning.

```sh
nix develop path:.
./bin/gremvm install
GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` is idempotent. It downloads the exact Lume archive, verifies SHA-256, the complete code signature, Developer ID team `YCK386LBJ7`, bundle identifier `com.trycua.lume`, hardened-runtime flag, Gatekeeper acceptance, and reported version. It disables Lume telemetry before ordinary use, adds a deny-inbound Application Firewall rule for Lume, and installs a private user LaunchAgent. It never runs Lume's unauthenticated HTTP service. The optional `~/.local/bin/gremvm` symlink is created when that path is free; the absolute command above always works.

`provision` creates a vanilla Tahoe VM using NAT networking and Lume's unattended setup; installs a host-key-pinned, shutdown-only SSH key; disables SIP through Recovery; verifies `csrutil status`; verifies clean guest shutdown; and starts the LaunchAgent. Its explicit phases (`creating`, `created`, `sip-disabled`, and `ready`) make reruns resume safely without treating a partial VM as complete. The default `GREMVM_IPSW=latest` is allowed only on a macOS 26 host. For a newer host or exact guest-build reproducibility, set it to an absolute, reviewed Tahoe IPSW in the generated config before provisioning; GremVM reads the IPSW's `ProductVersion` and rejects non-Tahoe images.

Lume's unattended bootstrap temporarily creates the guest administrator `lume` with password `lume`, enables guest SSH and guest automatic login, and disables guest sleep/lock. These are **guest** changes, not host changes. Immediately after provisioning:

```sh
"$GREMVM" console
```

Change the guest password before adding work data or remote access. If future `gremvm sip-off` use matters, choose a long passphrase made from lowercase ASCII letters, digits, and hyphens; that is the character set Lume 0.4.0's Recovery automation accepts. Then follow [`docs/REMOTE_ACCESS.md`](docs/REMOTE_ACCESS.md).

## Commands

```sh
"$GREMVM" status
"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" logs --follow --lines 200
"$GREMVM" console
nix develop path:. -c "$GREMVM" sip-off
"$GREMVM" firewall-check
"$GREMVM" runtime-path
"$GREMVM" acknowledge-hardening --confirm

"$GREMVM" backup --destination "/Volumes/GremVM Backup/lume"
"$GREMVM" uninstall
```

The LaunchAgent uses a `PathState`-conditioned `KeepAlive` to restart the Lume runner after a process failure, but only after provisioning reaches `ready`. On logout or host shutdown, its small supervisor asks the guest to shut down through a forced-command SSH key, requires Remote Login to remain unavailable for repeated polls plus a disk-settle interval, then reaps the exact managed runner. This explicit reap is necessary because Lume 0.4.0's foreground process remains resident after a guest-initiated halt. If shutdown evidence cannot be established, logs say so and fall back to Lume stop. An interactive `stop` reports that fallback as an error. Backups never use the destructive fallback.

Ordinary uninstall removes the LaunchAgent, wrapper, and vendored Lume runtime. It preserves the VM store, configuration, shutdown key, logs, and every backup. There is deliberately no purge flag.

## Backups

`gremvm backup` requires an existing writable directory on a different mounted volume and a running managed supervisor. It holds a crash-recoverable lifecycle lock, sends the authenticated guest shutdown, requires sustained SSH disappearance plus the settle interval, terminates and reaps only the recorded managed Lume process, unloads the supervisor, makes a Lume clone containing `disk.img`, `nvram.bin`, and `config.json` together, writes a completion manifest last, and restarts the VM. No existing backup is pruned.

Use an APFS (Encrypted) external volume for the first copy and restic for encrypted, deduplicated off-site history. The exact setup, retention policy, and restore drill are in [`docs/BACKUPS.md`](docs/BACKUPS.md).

## Development

```sh
nix develop path:. -c ./scripts/check.sh
```

Static checks cover shell formatting/linting, Nix formatting/evaluation, lifecycle-state smoke tests, the exact upstream pin, and absence of personal signing material. Provisioning, Recovery, firewall reachability, cold boot, and restore still require acceptance tests on the actual Mac Studio.
