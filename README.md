# GremVM

GremVM is a deliberately small wrapper around upstream [Lume](https://cua.ai/docs/how-to-guides/lume/install-lume). It manages one persistent macOS VM named `work` at `~/Library/Application Support/GremVM/vms/work`.

It does only this:

- installs a pinned, verified Lume release;
- creates the VM with Lume's built-in unattended Tahoe preset and disables SIP;
- starts Lume after the owning host user logs in; and
- lets launchd restart Lume if its runner exits.

There is no remote-access setup, guest-management layer, backup implementation, configuration file, or tunable VM settings. VM data is never deleted by default.

## Host behavior

This supports an Apple-silicon Mac running macOS 26 (Tahoe). The Tahoe restriction keeps the SIP-disable path on Lume's supported unattended preset with the least wrapper code.

The VM starts after the owning host account signs in. GremVM never changes FileVault or automatic-login settings. Consequently, after a cold boot with FileVault enabled, someone must unlock the Mac and sign in; with no signed-in owner, the VM remains stopped.

Lume itself supplies the guest setup needed to disable SIP. GremVM does not expose or configure a way to connect to the guest. `install` requires the macOS Application Firewall and blocks inbound traffic to Lume, because Lume creates its own local VNC control listener.

The launchd policy restarts Lume when its runner exits. It does not add a separate guest-health agent: if macOS inside the VM is deliberately shut down while the Lume runner stays alive, use `gremvm restart`.

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
