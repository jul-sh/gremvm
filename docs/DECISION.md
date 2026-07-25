# VM engine decision: Lume over Tart and custom code

Decision date: 2026-07-24. Reassess this document when upgrading the pinned VM engine.

| Requirement | Previous custom service | Lume 0.4.0 | Tart 2.34.0 |
|---|---|---|---|
| Maintained VM engine | This repository | Cua upstream | OpenAI upstream |
| Persistent macOS VM | Yes | Yes | Yes |
| Automated SIP disable | Not implemented | **Yes: `lume sip off`** | No; manual Recovery console |
| Paired disk/NVRAM clone | Custom bundle backup | `lume clone` | `tart clone`/export |
| Headless foreground run | Yes | `lume run --no-display` | `tart run --no-graphics` |
| Built-in VM autostart | Custom system daemon | No | No |
| Supported start before login | Intended, hardware qualification pending | No | No on modern macOS |
| Guest-clean `stop` | Custom guest agent | No native command; GremVM requests guest shutdown, then reaps Lume | No; destructive VZ stop |
| Ongoing local code | About 13,000 lines of Swift/tests | Small shell policy layer | Small shell policy layer plus manual SIP |
| Signing responsibility | User Developer ID/notary | Upstream notarized app | Upstream notarized app |

## Decision

Use pinned Lume. Its maintained Recovery automation is decisive because SIP must be disabled in the guest. Lume explains that SIP is signed LocalPolicy paired with `disk.img`, `nvram.bin`, and virtual hardware identity; its workflow changes that policy in paired Recovery and verifies it after reboot. Lume 0.4.0's verified unattended workflow is Tahoe, so this deliberately minimal deployment supports a macOS 26 host only and always uses that host's current Apple restore image. There is no guest-image selector or fallback path. See [Lume's SIP guide](https://cua.ai/docs/how-to-guides/lume/change-sip), [policy model](https://cua.ai/docs/concepts/how-sip-works-in-lume-vms), and [limits](https://cua.ai/docs/reference/lume/limits).

Tart 2.34.0 is the stronger choice for CI-style OCI image distribution and export/import, but it exposes only Recovery boot. The operator must drive `csrutil disable` manually. Tart also documents that macOS 15 and later require an unlocked `login.keychain` to start a VM on a headless host; its workarounds are a GUI/automatic login or a manually managed keychain. See [Tart's headless FAQ](https://tart.run/faq/#headless-machines) and [Quick Start](https://tart.run/quick-start/).

The previous native service could pursue pre-login startup with a root service keychain, authenticated guest agent, custom backup format, Developer ID signing, and ongoing Virtualization.framework compatibility work. It was not installed and no VM data exists in this checkout, so replacing it loses no live VM. Keeping that code would contradict the new goal of using a VM stack maintained elsewhere.

## Requirements deliberately relaxed

Neither upstream tool provides all of the previous host-service contract. This deployment changes three claims instead of disguising custom infrastructure as an upstream solution:

1. Startup is at the owning user's Aqua login, not at the login window. Host FileVault and automatic login remain untouched.
2. `launchd` restarts a failed Lume process, but Lume's foreground loop does not reliably detect every guest kernel panic. Process failure recovery is supported; guest health recovery is not promised.
3. Lume's own `stop` is not guest-clean, and its 0.4.0 foreground loop stays resident after a guest-initiated halt. The local policy layer uses a narrowly restricted, host-key-pinned SSH key to invoke `/sbin/shutdown -h now`, requires sustained SSH disappearance plus a settle interval, and then terminates the exact recorded runner. SSH disappearance is conservative operational evidence, not a Virtualization.framework state attestation; the strict alternative is to wait for an upstream Lume release whose runner exits on the guest-stopped event. This is the only lifecycle glue retained here.

Apple documents that `VZVirtualMachine.stop` is destructive because it gives the guest no opportunity to shut down. That is why backups refuse to proceed unless the SSH shutdown was confirmed. See [Apple's stop API](https://developer.apple.com/documentation/virtualization/vzvirtualmachine/stop%28completionhandler%3A%29).

## Supply-chain policy

The repository downloads only the versioned Lume 0.4.0 arm64 archive from its [official release](https://github.com/trycua/cua/releases/tag/lume-v0.4.0). It requires the reviewed SHA-256 and verifies the complete app signature, Developer ID identity, hardened-runtime flag, Gatekeeper acceptance, and reported version. No floating Lume installer, `latest` release alias, self-update, ad-hoc rebuild, or re-signing is accepted. Apple's restore image remains a one-time dynamic input because Lume asks Virtualization.framework for the image supported by that host; the resulting persistent VM is not recreated on reruns.

Upstream Lume already carries the signature that macOS evaluates. The previous Apple certificate/notary envelopes and Keytap signing flow have therefore been removed from this repo. The original credentials remain owned by ClipKitty; they are not copied, decrypted, re-encrypted, or used here.
