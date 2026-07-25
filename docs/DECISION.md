# Architecture decisions

Decision date: 2026-07-24. Reassess pins when Tart, `cloudflared`, or a new
macOS release changes behavior relied on here.

## VM engine: Tart

| Requirement | Custom Virtualization.framework service | Lume 0.4.0 | Tart 2.34.0 |
|---|---|---|---|
| Maintained VM engine | This repository | Cua | OpenAI/Cirrus Labs |
| Latest host-supported Apple IPSW | Custom code | Possible, but prior design constrained it for SIP | **First-class `--from-ipsw=latest`** |
| Persistent local macOS VM | Yes | Yes | Yes |
| New macOS release setup | Custom maintenance | Unattended flow needs qualification | Manual Setup Assistant; least release-specific |
| SIP disable | Custom work | Automated only on qualified Recovery flow | Manual Recovery; not guaranteed |
| Portable stopped backup | Custom format | Directory clone | **Compressed `.tvm` export/import** |
| Pre-login start on macOS 15+ | Custom keychain work | Needs keychain workaround | Needs keychain workaround |
| Signing responsibility | Personal signing/notary | Upstream | Upstream |
| Local code | Large service | Lifecycle policy | Small lifecycle/export wrapper |

Use pinned Tart 2.34.0. The newest host-supported macOS guest is more important
than automating SIP disablement. Tart passes `latest` to Apple's supported
restore-image lookup, and its
[Quick Start](https://tart.run/quick-start/) documents the manual installation
flow.

Manual Setup Assistant is intentional. It avoids release-specific UI
automation and does not reject a new macOS solely because an older Recovery
workflow was the last qualified one. `latest` resolves only at initial
creation; the persistent VM is not recreated on reruns.

Lume was previously attractive because `lume sip off` paired Recovery and
verification on a known release. That conflicts with the final latest-first
priority. Tart exposes Recovery through `tart run --recovery`; GremVM treats
any SIP change as optional operator maintenance and never infers success.

Tart's `export` and `import` preserve configuration, disk, NVRAM, and hardware
identity in an Apple Archive. This keeps the backup boundary upstream-owned.

## Host lifecycle boundary

Use one per-user Aqua LaunchAgent with `KeepAlive` tied to an explicit
`WANTS-RUNNING` file. It starts Tart after login, restarts the runner after
failure, and handles TERM by stopping the remote connector first and then
requesting guest shutdown over a forced-command SSH key.

This deliberately does not meet the original pre-user-session requirement.
Starting with macOS 15, Virtualization.framework needs an unlocked
`login.keychain`. Tart documents two
[headless workarounds](https://tart.run/faq/#headless-machines): store/unlock a
login password, or create and select an empty-password login keychain. The
first adds a boot-time host credential and the second can alter the owning
user's keychain behavior. Tart describes the automated workaround as unstable,
and [fresh-host failures](https://github.com/openai/tart/issues/1146) show that
a one-time GUI login may create additional state. A dedicated service-account
LaunchDaemon would isolate that workaround but would still be experimental and
require cold-boot qualification on the target host. The implementation instead
requires one host login after cold boot and never changes FileVault or
automatic login.

## Remote access: OpenSSH through Cloudflare Tunnel

Use the guest's existing macOS `sshd`, a locally managed Cloudflare Tunnel on
the host, and native OpenSSH on the client with:

```sshconfig
ProxyCommand /absolute/path/cloudflared access ssh --hostname %h
```

No custom network protocol or guest agent is maintained. There is no browser
UI, VNC, WebRTC, TURN, signaling Worker, public port 22, or router forward.
Cloudflare Access adds an identity allowlist before the normal guest SSH
authentication.

The host connector is smaller than a guest installation: the existing
supervisor already knows when Tart starts, can resolve its current NAT address,
and can terminate a sibling process before shutdown. Installing a connector in
the guest would add another LaunchDaemon, binary-update path, and secret
transfer to every restored VM.

A local Tunnel configuration is used because the guest origin address is
dynamic. The account API token is used only during setup; runtime receives a
non-expiring credential scoped to running that one Tunnel. The supervisor
atomically renders one ingress route to `ssh://<guest-ip>:22` plus a 404
catch-all, validates it, and retries the pinned `cloudflared` process after
failure.

Cloudflare's basic published SSH flow transports SSH through WebSocket and
requires client-side `cloudflared`. Truly raw TCP SSH needs WARP private routing
or Spectrum and is not the default minimal design.

## Deliberate limits

1. A cold boot requires FileVault unlock when enabled and one login by the
   owning user before the VM and Tunnel return.
2. First provision requires local macOS installation and a generated guest
   bootstrap. Interrupted creation can be resumed.
3. `launchd` restarts a failed Tart process, not every guest hang. A stale
   Virtualization.framework state may still require operator intervention.
4. Shutdown evidence combines the forced-command acknowledgement with Tart
   releasing the VM. If evidence is missing, backup refuses export.
5. A changed guest SSH host key fails closed for the clean SSH shutdown path
   and Tunnel exposure. Stop may then require Tart's destructive fallback, and
   the mismatch requires local investigation.
6. SIP Recovery may fail on a new release or change after an OS update. Back up
   first and verify `csrutil status` inside normal macOS.
7. A `.tvm` is compressed, not encrypted or self-retaining. Encryption,
   retention, off-site copies, and restore tests remain separate operations.
8. `ssh: configured` means only that the local tunnel credential exists; it is
   not an end-to-end health check.

## Supply chain, signing, and secret scope

The deployment verifies and uses the versioned, upstream-signed Tart 2.34.0
release. Tart self-update is not used. `cloudflared` 2026.5.2 comes from the
locked Nixpkgs input and is copied as a local CLI runtime. The Apple IPSW is
deliberately dynamic only when `latest` is resolved during first creation.

There is no locally distributed application to Developer-ID sign or notarize.
Importing the user's Developer ID certificate, App Store Connect API key, or
notary secret would be unused and contrary to secret minimization.

Keytap is the only age recipient for the active Cloudflare API-token and
tunnel-credential envelopes. No ClipKitty recipient is present. The host must
materialize one mode-0600 tunnel credential for unattended connector startup;
the broader API token remains encrypted and is never installed in runtime.

The local Git object database contains Codex checkpoint refs from superseded
work, including historical two-recipient signing envelopes. They are not in the
active tree and are not included by an ordinary branch push, but mirroring the
entire `.git` directory would include them. Removing those refs is destructive
history cleanup and is intentionally not done by installation or uninstall.

Tart uses a Fair Source license; a personal workstation is covered by its free
tier, while broader organizational use may require a subscription. Review
[Tart licensing](https://tart.run/licensing/) before expanding deployment.
