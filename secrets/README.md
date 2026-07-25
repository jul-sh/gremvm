# Repository secrets

Every `*.age` file here must have exactly one recipient: the age identity
derived by `keytap` with the name `keytap`. Do not add ClipKitty recipients or
pass `--to`/`-R` to an encryption command.

- `CLOUDFLARE_API_TOKEN.age` is used only by
  `scripts/cloudflare-setup.sh` to reconcile the Tunnel, DNS record, and Access
  policy. It is not installed on the Mac Studio for unattended use.
- `CLOUDFLARE_TUNNEL_CREDENTIALS.age` is created by
  `scripts/cloudflare-setup.sh apply`. It can run only the one `gremvm` Tunnel;
  it cannot manage the Cloudflare account.

On the Mac Studio, `scripts/cloudflare-install-host.sh` decrypts the second
envelope and writes one mode-0600 operational copy under
`~/Library/Application Support/GremVM/cloudflare/`. That plaintext copy is
required because the tunnel starts unattended after the owning user logs in.
It is never committed or printed.

Import an updated source API token with:

```sh
nix develop path:. -c ./scripts/import-cloudflare-api-token.sh
```
