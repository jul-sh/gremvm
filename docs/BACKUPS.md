# Backups and recovery

The supported backup unit is a **stopped Tart `.tvm` export**. Never copy Tart's live `disk.img` or export a running VM. A complete export keeps the VM configuration, disk, NVRAM, and Apple-silicon hardware identity together.

Set the installed paths once:

```sh
GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
TART="$("$GREMVM" runtime-path)"
TART_HOME="$HOME/Library/Application Support/GremVM/tart"
```

## Recommended layout

Keep three copies on two kinds of storage, with one copy off-site:

1. the live Tart VM on the Mac Studio;
2. at least two stopped `.tvm` generations on a separately mounted APFS (Encrypted) SSD; and
3. an encrypted off-site copy, such as restic object storage.

A `.tvm` is LZFSE-compressed Apple Archive data. It is **not encrypted**. Protect the external disk passphrase outside the guest, and do not put a sensitive export on an unencrypted volume.

## Create a stopped export

Prepare the destination explicitly so a missing external disk cannot be mistaken for a host directory:

```sh
mkdir -m 700 "/Volumes/GremVM Backup/tart"
"$GREMVM" backup --destination "/Volumes/GremVM Backup/tart"
```

The backup operation serializes lifecycle changes, sends the restricted guest shutdown request when the managed VM is running, and requires the Tart runner to release the VM and report it stopped before invoking `tart export`. It then:

1. writes a timestamped `.tvm` to a temporary path;
2. asks `/usr/bin/aa` to enumerate the Apple Archive;
3. requires archive entries for `config.json`, `disk.img`, and `nvram.bin`;
4. computes a SHA-256 digest;
5. moves the completed export into place; and
6. writes an adjacent JSON completion manifest last.

The command prints the final export and manifest paths. A `.tvm` without its completion manifest is incomplete or unmanaged and must not be selected automatically. The source VM is never deleted, and completed older exports are never pruned by GremVM.

`aa list` proves that Apple Archive can parse the container and enumerate the expected VM members. The recorded SHA-256 detects later changes to that exact file. Neither proves that macOS and the work data are usable; only an import and booted restore drill does that.

You can repeat the structural check without extracting the archive:

```sh
EXPORT='/Volumes/GremVM Backup/tart/work--20260724T210000Z.tvm'
/usr/bin/aa list -i "$EXPORT" -list-format json >/dev/null
/usr/bin/shasum -a 256 "$EXPORT"
```

Verify the filename, size, and digest against the adjacent completion manifest. Do not edit either file after creation:

```sh
MANIFEST="$EXPORT.manifest.json"
EXPECTED_NAME=$(/usr/bin/plutil -extract archive raw -o - "$MANIFEST")
EXPECTED_BYTES=$(/usr/bin/plutil -extract archiveBytes raw -o - "$MANIFEST")
EXPECTED_SHA=$(/usr/bin/plutil -extract archiveSha256 raw -o - "$MANIFEST")
ACTUAL_BYTES=$(/usr/bin/stat -f %z "$EXPORT")
ACTUAL_SHA=$(/usr/bin/shasum -a 256 "$EXPORT" | /usr/bin/awk '{print $1}')

[ "$(basename "$EXPORT")" = "$EXPECTED_NAME" ]
[ "$ACTUAL_BYTES" = "$EXPECTED_BYTES" ]
[ "$ACTUAL_SHA" = "$EXPECTED_SHA" ]
```

## Optional encrypted off-site history

The Nix shell provides restic and Keytap. Keytap can derive the restic password without adding a plaintext password or a ClipKitty recipient to this repository:

```sh
nix develop path:.
keytap remember gremvm-restic

export RESTIC_REPOSITORY='s3:s3.example.net/my-private-bucket/gremvm'
restic \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  init
```

Back up the completed pair printed by `gremvm backup`:

```sh
EXPORT='/Volumes/GremVM Backup/tart/work--20260724T210000Z.tvm'
MANIFEST="$EXPORT.manifest.json"

restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  backup \
  --group-by host,tags \
  --tag gremvm \
  --tag work \
  "$EXPORT" \
  "$MANIFEST"
```

Use the manifest path actually printed by the command if its name differs from the example. Save a copy of the small GremVM configuration separately if you want the same host defaults after total host loss; the `.tvm` remains the authoritative guest-data backup.

Because each `.tvm` is a compressed whole-VM archive, cross-snapshot deduplication may be limited. Measure repository growth before choosing a long retention period. Keep `--group-by host,tags` on both backup and retention commands: timestamped export paths differ, so path-based grouping would otherwise give every generation its own retention group.

Repository credentials and the Keytap passkey are separate recovery dependencies. Keep both outside the guest. `keytap remember` uses the owning user's login keychain on macOS, so unattended restic work is available only after that user logs in.

## Retention and checks

Keep at least two verified local exports. Apply off-site retention only after reviewing a dry run:

```sh
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  forget \
  --group-by host,tags \
  --tag gremvm,work \
  --keep-daily 7 \
  --keep-weekly 5 \
  --keep-monthly 12 \
  --keep-yearly 3 \
  --dry-run
```

When the selection is correct, repeat without `--dry-run` and add `--prune`. This does not remove the external `.tvm` files; delete an old local export and its matching manifest only after a newer off-site snapshot and restore drill succeed.

Run a restic metadata check weekly and read a rotating subset of data monthly:

```sh
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  check

restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  check --read-data-subset=1/12
```

Rotate `1/12` through `12/12`. Restic documents [repository setup](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html), [retention](https://restic.readthedocs.io/en/stable/060_forget.html), and [restore](https://restic.readthedocs.io/en/stable/050_restore.html).

## Restore without overwriting production

Always import under a new VM name and into the managed `TART_HOME`. Stop the
original first and keep it intact until the recovery copy has booted and its
work data has been checked. Importing beside the original lets Tart detect the
hardware-identity collision and regenerate the restored VM's MAC address.

For a direct external export:

```sh
EXPORT='/Volumes/GremVM Backup/tart/work--20260724T210000Z.tvm'
"$GREMVM" stop

# Compare this digest with the completion manifest first.
/usr/bin/shasum -a 256 "$EXPORT"
/usr/bin/aa list -i "$EXPORT" -list-format json >/dev/null

TART_HOME="$TART_HOME" "$TART" import "$EXPORT" work-restore-test
TART_HOME="$TART_HOME" "$TART" run work-restore-test
```

For restic, restore to a new scratch directory and then import:

```sh
mkdir -m 700 "/Volumes/Restore Scratch/gremvm"
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  restore latest \
  --tag gremvm,work \
  --target "/Volumes/Restore Scratch/gremvm"

EXPORT='/Volumes/Restore Scratch/gremvm/path/to/work--20260724T210000Z.tvm'
MANIFEST="$EXPORT.manifest.json"

# Repeat the filename, size, SHA-256, and Apple Archive checks above against
# the restored pair before importing it.
TART_HOME="$TART_HOME" "$TART" import "$EXPORT" work-restore-test
TART_HOME="$TART_HOME" "$TART" run work-restore-test
```

Inside the restored guest, verify the macOS version, work files, application
state, and any boot-policy assumption you rely on. Test its SSH service directly
from the host over Tart's private address; the one production Cloudflare
hostname remains bound to `work`. SIP is not asserted by the backup manifest;
run `csrutil status` if it matters. Shut the restore guest down from macOS, then
return the managed VM to service:

```sh
"$GREMVM" start
```

Delete `work-restore-test` only after recording the successful drill, or keep it
stopped for further inspection.

Perform this drill quarterly from a second Apple-silicon Mac whose macOS version supports the restored guest. Never import over `work`, run original and restored identities simultaneously, or prune the final known-good local and off-site copies together.
