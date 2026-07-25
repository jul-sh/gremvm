# GremVM

GremVM is a small deployment layer around upstream
[Tart](https://tart.run/), not a VM engine. Tart owns Virtualization.framework
and the persistent macOS VM. This repository pins Tart 2.34.0 and
`cloudflared` 2026.5.2, adds host lifecycle wiring, creates stopped `.tvm`
exports, and routes ordinary SSH through Cloudflare.

At first creation, `--from-ipsw=latest` asks Apple for the newest macOS restore
image supported by that Mac. GremVM will not select an older release to make
SIP automation work. The VM is persistent: rerunning setup neither recreates it
nor silently changes its macOS version. Apply later macOS updates inside the
guest.

## Important host boundary

The service starts after the owning host account logs in. macOS 15 and newer
require an unlocked `login.keychain` for Virtualization.framework. Tart offers
headless workarounds that either store/unlock a password or create an
empty-password login keychain; this minimal deployment deliberately does
neither. Consequently, it does **not** satisfy pre-login recovery after a cold
boot. Someone must unlock FileVault when enabled and log in locally; locking
the host screen afterward is fine.

GremVM never reads or changes FileVault or automatic-login settings. Tart's
empty-keychain workaround is explicitly not a stable contract, and
[fresh-host failures](https://github.com/openai/tart/issues/1146) show that a
one-time GUI login may create additional state that the documented keychain
commands do not. A dedicated service-account LaunchDaemon would therefore be
an experimental, host-qualified deployment rather than a reliable fix; it is
intentionally outside this small wrapper.

Other boundaries:

- Apple silicon is required.
- The LaunchAgent restarts a failed Tart runner. It is not a guest-health
  monitor and cannot recover every guest hang.
- macOS installation, Setup Assistant, and the guest bootstrap are one-time
  interactive steps.
- SIP is optional manual maintenance. It is not part of VM readiness and is
  never reported as disabled without an in-guest check.
- On macOS 15+, `cloudflared` may need one-time Local Network approval to reach
  Tart's private guest address. GremVM does not change that privacy setting.

## Install and provision

Requirements are an Apple-silicon Mac, Nix with flakes, at least 16 GiB host
RAM recommended, and enough storage for the IPSW, VM, and backups.

```sh
cd /Users/julsh/git/gremvm
nix run path:. -- install

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` copies the reviewed, upstream-signed Tart release, a Nix-pinned
`cloudflared` executable, and a private user LaunchAgent. It does not compile a
VM engine or require personal Apple signing credentials.

On first `provision`, GremVM creates the persistent VM from
`--from-ipsw=latest`, opens Tart's local console, and mounts a generated
read-only bootstrap directory. Complete macOS installation and Setup Assistant
with the dedicated work account (`grem` by default), then run the displayed
bootstrap command in the guest and shut down from the Apple menu. Rerunning
`provision` resumes an interrupted setup and refuses to adopt or overwrite an
unrelated same-named Tart VM.

The bootstrap enables macOS Remote Login, installs a forced-command SSH key
that can only request clean shutdown, and disables guest sleep. It does not
configure Cloudflare, change SIP, or alter host settings.

## Operations

```sh
"$GREMVM" status
"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" console
"$GREMVM" logs --follow --lines 200
"$GREMVM" runtime-path

"$GREMVM" backup --destination "/Volumes/GremVM Backup/tart"
"$GREMVM" uninstall
```

Managed runs use NAT networking, no graphics, no clipboard, and no persistent
host-directory share. `console` is the local break-glass path. `stop`, host
logout, and host shutdown request guest shutdown through the restricted key,
then require Tart to release the VM. Backups refuse to export a running VM.

`uninstall` removes the wrapper, LaunchAgent, Tart runtime, and `cloudflared`
runtime. It preserves the VM, configuration, logs, lifecycle keys, Cloudflare
tunnel credential, and backups. There is deliberately no purge command.

## SSH through Cloudflare

Cloudflare setup is separate and idempotent. It owns one locally managed
Tunnel, one proxied CNAME for `gremvm.eviljuliette.com`, and one Access
application whose allow policy contains exactly one email address.

```sh
cd /Users/julsh/git/gremvm
export GREMVM_CLOUDFLARE_ACCESS_EMAIL='you@example.com'

nix develop path:. -c ./scripts/cloudflare-setup.sh check
nix develop path:. -c ./scripts/cloudflare-setup.sh apply
nix develop path:. -c ./scripts/cloudflare-install-host.sh
"$GREMVM" restart
```

The setup token needs Zone Read, DNS Write, Cloudflare Tunnel Write, and
Access: Apps and Policies Write. `check` is read-only and runs every inventory
check before `apply` mutates anything. Runtime keeps only a tunnel-specific
credential; the account API token is never installed into the host service.

On the client, install `cloudflared` and add:

```sshconfig
Host gremvm
  HostName gremvm.eviljuliette.com
  User grem
  ProxyCommand /absolute/path/cloudflared access ssh --hostname %h
```

Then use normal OpenSSH tools: `ssh gremvm`, `scp ... gremvm:...`, and
`sftp gremvm`. This is SSH proxied over Cloudflare's WebSocket transport, not
WebRTC. A basic Cloudflare Tunnel cannot expose raw public TCP port 22; removing
the `ProxyCommand` requires WARP/private routing or Cloudflare Spectrum.

Cloudflare Access authenticates the allowed identity first. The guest still
performs normal SSH authentication with its macOS password or a public key you
add to `~/.ssh/authorized_keys`. See [docs/REMOTE_ACCESS.md](docs/REMOTE_ACCESS.md)
for host-key verification and acceptance tests.

## Backups

`backup` cleanly stops the VM and exports it with `tart export` to a timestamped
`.tvm`. It validates the Apple Archive members, computes a SHA-256 digest, and
writes a completion manifest last. A `.tvm` is compressed but not encrypted.
Use an APFS (Encrypted) external disk for the first copy and optionally restic
for encrypted off-site history. Retention and restore drills are in
[docs/BACKUPS.md](docs/BACKUPS.md).

## SIP

If SIP still matters, make a verified stopped export first, stop the managed
service, and use Tart Recovery manually with the managed `TART_HOME`:

```sh
TART="$("$GREMVM" runtime-path)"
TART_HOME="$HOME/Library/Application Support/GremVM/tart" \
  "$TART" run --recovery work
```

In Recovery Terminal run `csrutil disable`, then boot normally and check
`csrutil status` inside the guest. This may stop working as Apple changes the
latest release; GremVM neither automates nor records the result.

## Secrets, signing, and checks

The Cloudflare API token is stored as
`secrets/CLOUDFLARE_API_TOKEN.age`. `apply` creates
`secrets/CLOUDFLARE_TUNNEL_CREDENTIALS.age`. Every active-tree age envelope
has exactly one recipient: the Keytap-derived `keytap` identity. No ClipKitty
recipient is used. The tunnel connector necessarily has one mode-0600
plaintext operational copy outside the repository.

Tart is already signed and notarized upstream; `cloudflared` is a pinned local
CLI dependency. Importing Developer ID, App Store Connect, or notarization
secrets would add unused sensitive material, so this deployment does not do it.

```sh
nix develop path:. -c ./scripts/check.sh
```

Before relying on the Mac Studio remotely, test initial provisioning, guest
shutdown, runner restart, cold boot with the actual FileVault setting, external
SSH, stopped export, import under a new name, and a booted restore.
