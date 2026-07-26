# GremVM

GremVM is a deliberately small wrapper around upstream [Lume](https://cua.ai/docs/how-to-guides/lume/install-lume). It manages one persistent macOS VM named `work` at `~/Library/Application Support/GremVM/vms/work`.

It does only this:

- installs a pinned, verified Lume release;
- creates the VM with Lume's built-in unattended Tahoe preset and disables SIP;
- starts Lume after the owning host user logs in; and
- restarts Lume if its runner exits or the guest remains unavailable.

There is no remote-access setup, guest-management layer, backup implementation, configuration file, or tunable VM settings. VM data is never deleted by default.

## Host behavior

The host must meet Lume's upstream requirements: an Apple-silicon Mac running macOS 13 or later. GremVM does not pin the host to a specific macOS release. Lume and Apple's Virtualization framework decide whether the Tahoe restore image is supported by the current host; provisioning fails normally if it is not.

The VM starts after the owning host account signs in. GremVM never changes FileVault or automatic-login settings. Consequently, after a cold boot with FileVault enabled, someone must unlock the Mac and sign in; with no signed-in owner, the VM remains stopped.

Lume itself supplies the guest setup needed to disable SIP. GremVM does not expose or configure a way to connect to the guest. `install` requires the macOS Application Firewall and blocks inbound traffic to Lume, because Lume creates its own local VNC control listener.

The supervisor uses Lume's existing guest SSH-readiness result as a health signal; it does not install a guest agent or authenticate into the guest. Guest Remote Login must therefore remain enabled. It allows roughly ten minutes for boot and fifteen minutes for a previously healthy guest to recover, reducing false restarts during macOS updates. Sustained failure terminates the stuck Lume runner, and launchd starts it again.

## Install

Run setup locally on the Mac Studio:

```sh
cd /Users/julsh/git/gremvm
nix develop path:.
./bin/gremvm install

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` is idempotent. It downloads the exact Lume release in [`versions/lume.env`](versions/lume.env), verifies its checksum and macOS signing identity, installs the user LaunchAgent, and preserves any existing VM data. `provision` is resumable and uses Lume to create `work`, disable SIP, and start it.

Keep the Nix shell open for `provision`: it supplies `vncdo`, which Lume needs for the paired-Recovery SIP operation.

## Commands

```sh
"$GREMVM" status
"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" logs
"$GREMVM" uninstall
```

`logs` prints the latest 200 service log lines. `stop` uses Lume's normal stop operation; this intentionally does not add a custom guest shutdown protocol. `uninstall` removes the wrapper, pinned Lume runtime, and LaunchAgent while preserving the VM, state, and logs.

## Development

```sh
nix develop path:. -c ./scripts/check.sh
```
