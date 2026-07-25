# SSH remote access through Cloudflare

GremVM exposes only the guest's normal macOS SSH service. There is no browser
desktop, VNC, WebRTC, TURN, custom signaling protocol, guest agent, router
port-forward, or public port 22.

```text
OpenSSH client
  └─ cloudflared ProxyCommand + Cloudflare Access
       └─ Cloudflare Tunnel on the Mac Studio
            └─ ssh://<current Tart guest IP>:22
                 └─ macOS guest sshd
```

The client application protocol remains SSH end to end. Cloudflare's published
non-HTTP service transport uses WebSocket, so client-side `cloudflared` is
required. This is not WebRTC. Raw `ssh hostname` without a `ProxyCommand`
requires a different Cloudflare product/topology such as WARP private routing
or Spectrum.

Cloudflare documents this flow in
[SSH with client-side cloudflared](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/use-cases/ssh/ssh-cloudflared-authentication/).

## Provision Cloudflare

### Cloudflare API token

Create a **user API token** in Cloudflare Dashboard: **My Profile** → **API
Tokens** → **Create Token** → **Create Custom Token**. Use a name such as
`gremvm-provisioner`, and grant exactly the following permissions:

| Scope | Permission | Resource selection |
| --- | --- | --- |
| Zone | Zone / Read | Include the specific `eviljuliette.com` zone |
| Zone | DNS / Edit | Include the specific `eviljuliette.com` zone |
| Account | Cloudflare Tunnel / Edit | Include the account that owns `eviljuliette.com` |
| Account | Access: Apps and Policies / Edit | Include the account that owns `eviljuliette.com` |

Cloudflare sometimes calls the `Edit` operations `Write` in API responses;
select `Edit` in the Dashboard. Do not use a Global API Key, an all-accounts
scope, an all-zones scope, or a token shared with another project. Set an
expiration you will rotate (one year is a practical default); add an IP filter
only if the administrative egress address is stable.

Cloudflare shows the value once. Do not paste it into chat or a shell command.
At the GremVM checkout, use the hidden prompt to create the one-recipient
Keytap envelope:

```sh
cd /Users/julsh/git/gremvm
nix develop path:. -c ./scripts/store-cloudflare-api-token.sh
```

The token is used only by `cloudflare-setup.sh` during reconciliation. It is
not copied to the Mac Studio service; that service receives only the
single-Tunnel connector credential created by `apply`.

Run this on a trusted machine with the repository and the Keytap identity:

```sh
cd /Users/julsh/git/gremvm
export GREMVM_CLOUDFLARE_ACCESS_EMAIL='you@example.com'

nix develop path:. -c ./scripts/cloudflare-setup.sh check
nix develop path:. -c ./scripts/cloudflare-setup.sh apply
```

`check` is read-only. It verifies the API token, zone, same-named Tunnel, exact
hostname record, Access application, and exclusive email policy before any
mutation. `apply` creates only missing managed resources and is safe to rerun.
It refuses conflicting or partially owned resources instead of overwriting or
deleting them.

The resulting resources are:

- one locally managed Tunnel named `gremvm`;
- proxied CNAME `gremvm.eviljuliette.com` to that Tunnel; and
- a self-hosted Access application with one allow policy for
  `GREMVM_CLOUDFLARE_ACCESS_EMAIL`.

A local Tunnel configuration is intentional here: Tart's NAT address is known
only after the VM starts. The supervisor resolves it each run and generates a
private ingress file. This avoids retaining the account API token at runtime or
adding a separate TCP forwarder. Cloudflare still documents
[locally managed ingress configuration](https://developers.cloudflare.com/tunnel/advanced/local-management/configuration-file/),
including `ssh://` origins.

## Install the connector credential

`apply` backs up the one-Tunnel credential as a Keytap-only envelope. On the Mac
Studio, materialize the operational copy:

```sh
cd /Users/julsh/git/gremvm
nix develop path:. -c ./scripts/cloudflare-install-host.sh

GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
"$GREMVM" restart
"$GREMVM" status
```

The installed JSON is mode 0600 under the owning account's GremVM directory.
Cloudflare documents that this credential can run its one Tunnel and cannot
manage the account. The broader API token is decrypted only during setup and is
never copied into host runtime.

`status` reports either:

```text
ssh: configured (gremvm.eviljuliette.com)
ssh: not-configured
```

`configured` means the tunnel credential is present. It does not prove that
the connector, Access login, guest `sshd`, or guest credentials are healthy;
use the external acceptance test below.

The tunnel shares the VM supervisor. It waits for guest SSH, generates ingress
for the current private IP, retries `cloudflared` after failure, and exits
before GremVM requests guest shutdown. Connector messages are in the normal
logs:

```sh
"$GREMVM" logs --lines 200
"$GREMVM" logs --follow
```

On macOS 15 or newer, the first connection from `cloudflared` to Tart's private
address may trigger a Local Network privacy prompt. Approve it once while the
owning host user is logged in. GremVM does not modify that preference.

## Configure an SSH client

Install `cloudflared` on the client. Its absolute path varies by platform; on a
Nix client use `command -v cloudflared`, and with Homebrew use
`brew --prefix cloudflared`.

Add to `~/.ssh/config`:

```sshconfig
Host gremvm
  HostName gremvm.eviljuliette.com
  User grem
  ProxyCommand /absolute/path/cloudflared access ssh --hostname %h
  StrictHostKeyChecking yes
```

The first connection opens a browser for the Cloudflare Access login, then
macOS `sshd` asks for the guest password or uses a normal authorized key.

### Pin the guest host key

Do not accept a first-use key remotely without comparison. GremVM records the
guest's Ed25519 host key during local bootstrap at:

```text
~/Library/Application Support/GremVM/ssh/guest-host-key
```

On the Mac Studio, inspect its fingerprint over a trusted local path:

```sh
ssh-keygen -lf "$HOME/Library/Application Support/GremVM/ssh/guest-host-key"
```

Securely copy the key line to the client and prefix it with the public hostname
in `~/.ssh/known_hosts`:

```text
gremvm.eviljuliette.com ssh-ed25519 AAAA...
```

Now ordinary tools work:

```sh
ssh gremvm
scp ./file gremvm:~/
sftp gremvm
```

The bootstrap's separate forced-command key is only for managed shutdown. Do
not reuse or broaden it. After the first password-authenticated session, add
your own public key to the guest account's `~/.ssh/authorized_keys` and verify a
new key-authenticated session before changing guest SSH policy.

## Acceptance test

From a genuinely off-LAN client:

1. Confirm Access rejects an identity other than the configured email.
2. Confirm the configured identity reaches the SSH host-key check.
3. Compare the fingerprint with the locally recorded key, then log in.
4. Verify `ssh`, `scp`, and `sftp` all work.
5. Restart the guest and verify access returns without changing client config.
6. Lock the Mac Studio screen and verify SSH continues.
7. Log out or cold-boot the Mac Studio and confirm the documented boundary:
   access returns only after the owning host account logs in.
8. Stop the VM and verify SSH becomes unreachable; start it and verify recovery.
9. Restore a stopped `.tvm` under a new name and test its SSH service directly
   from the host's private Tart network. The one production hostname remains
   bound to the managed `work` VM.

## Troubleshooting and lifecycle

- `ssh: not-configured`: rerun `cloudflare-install-host.sh` after a successful
  Cloudflare `apply`.
- Access login fails: rerun `cloudflare-setup.sh check` with the exact email
  used during `apply`.
- Tunnel repeatedly reconnects: inspect `gremvm logs`, Internet connectivity,
  and host Local Network permission.
- Access succeeds but SSH fails: use `gremvm console` and verify Remote Login,
  the `grem` account, and its authentication settings inside the guest.
- Host-key mismatch: GremVM refuses the clean SSH shutdown path and tunnel
  exposure. A stop may use Tart's destructive fallback. Investigate from the
  local console; never overwrite the pinned key merely to silence the warning.

`gremvm uninstall` preserves the runtime tunnel credential and never deletes
Cloudflare resources. This keeps data and remote configuration recoverable.
Deletion or credential rotation is deliberately manual and destructive: first
stop remote use, remove/recreate the Tunnel in Cloudflare, delete the old
runtime credential and recovery envelope, then rerun `apply` and the host
installer. Do not reuse a credential after suspected disclosure.
