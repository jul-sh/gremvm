# GremVM

GremVM is a small wrapper around [Lume](https://cua.ai/docs/how-to-guides/lume/install-lume). It manages one persistent macOS VM named `work` at `~/Library/Application Support/GremVM/vms/work`.

It does only this:

- installs a pinned, verified Lume release;
- creates the VM and disables SIP;
- starts Lume after the owning host user logs in; and
- restarts Lume if its runner exits or the guest remains unavailable.

There is no remote-access setup, guest-management layer, backup implementation, configuration file, or tunable VM settings. VM data is never deleted by default.

## Requirements

Use an Apple-silicon Mac supported by the pinned Lume release. GremVM does not map host versions to guest versions; Lume chooses a restore image supported by the host.

The VM starts after the owning host account signs in. GremVM never changes FileVault or automatic-login settings. Consequently, after a cold boot with FileVault enabled, someone must unlock the Mac and sign in; with no signed-in owner, the VM remains stopped.

There is no remote-access setup. `install` requires the macOS Application Firewall and blocks inbound access to Lume. The supervisor uses Lume's SSH-readiness status to restart a guest that stays unavailable.

## Install

Run setup locally on the Mac Studio:

```sh
cd /Users/julsh/git/gremvm
nix develop path:.
./bin/gremvm install

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" provision
```

`install` and `provision` are idempotent and preserve existing VM data. Keep the Nix shell open for `provision`; it supplies the Recovery automation dependency Lume uses to disable SIP.

## Commands

```sh
"$GREMVM" status
"$GREMVM" start
"$GREMVM" stop
"$GREMVM" restart
"$GREMVM" logs
"$GREMVM" uninstall
```

`uninstall` preserves the VM, state, and logs.

## Development

```sh
nix develop path:. -c ./scripts/check.sh
```
