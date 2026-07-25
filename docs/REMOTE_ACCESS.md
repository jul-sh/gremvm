# Secure remote access

The remote desktop lives **inside the guest**. Lume's host VNC server is a local break-glass console, not the remote-access product.

Examples use `GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"` as the installed command.

## Required topology

```text
your device ── private Tailscale tailnet ── macOS guest
                                             ├─ Screen Sharing (GUI)
                                             └─ SSH (key only, if needed)

Mac Studio host ── local-only Lume console
```

- Keep the Lume VM network in NAT mode. GremVM passes `--network nat` explicitly.
- Install Tailscale inside the guest and use its system service so the node returns at guest boot.
- Permit only your identity/devices through tailnet ACLs; require MFA/device approval.
- Enable macOS Screen Sharing only for the dedicated guest work account.
- Keep guest Remote Login (sshd) enabled: GremVM's forced shutdown key always needs it. Add your own key and disable password authentication after setup if interactive SSH is needed.
- Never forward router ports for SSH, Screen Sharing, or VNC.
- Do not share host directories or enable Lume clipboard integration for a SIP-disabled guest.

Tailscale publishes its [macOS client variants and service behavior](https://tailscale.com/docs/concepts/macos-variants). Authentication keys, OAuth credentials, tailnet policy, and expiry rules belong to the tailnet/secret manager, not this repository.

## Bootstrap checklist

Lume's unattended setup intentionally starts with an insecure convenience account. Before adding work data, source-control tokens, Apple signing identities, or cloud credentials:

1. Run `"$GREMVM" console` locally.
2. Sign in as `lume` / `lume` and change the password immediately.
3. Confirm `csrutil status` says `disabled`.
4. Install OS updates, then re-check SIP and Lume compatibility.
5. Install Tailscale in the guest and authenticate it to the intended tailnet.
6. Enable Screen Sharing for only the work account.
7. Keep Remote Login enabled for the `lume` account so clean host shutdown and backups continue to work. Add your own SSH public key; verify it from an external network; then disable password authentication if desired—do not disable sshd or remove GremVM's forced-command key.
8. Decide whether guest automatic login is necessary. If not, disable it and remove the stale `/etc/kcpassword`; re-enable screen locking.
9. Verify remote access from a genuinely off-LAN network before leaving the Mac Studio unattended.

After completing and testing the checklist, clear the persistent status reminder explicitly:

```sh
"$GREMVM" acknowledge-hardening --confirm
```

Lume's SIP Recovery automation currently accepts administrator passwords made only from lowercase ASCII letters, digits, and hyphens. A long multiword passphrase in that alphabet preserves the ability to run `nix develop path:. -c "$GREMVM" sip-off` later; the pinned Nix shell supplies Lume's optional `vncdo` dependency. If you disable SSH password authentication, future SIP changes require temporarily restoring that access from the local console because Lume 0.4.0's SIP preflight is password-based.

## Host VNC boundary

`lume run --no-display` still starts a VNC listener. Lume 0.4.0 creates an external-host URL as well as a localhost URL, and VNC authentication has legacy limitations. Therefore `"$GREMVM" install` requires macOS Application Firewall to be enabled and adds a deny-inbound rule for the exact notarized `lume.app`:

```sh
"$GREMVM" firewall-check
```

The LaunchAgent runs with umask `077`, private VM/log directories, telemetry disabled, error-only Lume logs, a dynamic VNC port, no shared directories, no clipboard, and no Lume HTTP API. Test the firewall from another LAN machine while the VM is running; the random VNC port must not be reachable. The local `"$GREMVM" console` must still work.

If a version upgrade changes the app path, rerunning `"$GREMVM" install` installs and verifies a rule for that exact version before provisioning/start. Do not set `GREMVM_REQUIRE_APPLICATION_FIREWALL=false` unless an independently verified host firewall provides the same deny-inbound boundary.

## Host recovery boundary

This deployment starts only when the owning account logs in. Keep that account logged in and lock the screen instead of logging out. After a cold boot with FileVault, someone must unlock and log in locally before the VM and guest Tailscale node can return. GremVM does not enable host automatic login, alter FileVault, enable host SSH, or install a host overlay.

If unattended recovery after a power outage is non-negotiable, the owner needs an independent out-of-band path to unlock/login to the Mac Studio. Neither Lume nor Tart provides a supported way around the modern login-keychain requirement without reintroducing stored credentials or automatic login.

## Acceptance tests

From outside the home LAN:

1. Lock (do not log out of) the host account; verify guest Screen Sharing/SSH still works.
2. Restart the guest; verify Tailscale and access return without opening public ports.
3. Break guest Tailscale deliberately; use the local host console to diagnose it.
4. Reboot the host and exercise the documented FileVault/login boundary.
5. From another LAN device, verify Lume's VNC port is blocked.
6. Restore a backup under a new VM name and repeat remote access plus `csrutil status` checks.

Record the host/guest OS versions, Lume pin, FileVault state, firewall result, tailnet policy revision, SSH-key fingerprints, and last successful cold-boot/restore test.
