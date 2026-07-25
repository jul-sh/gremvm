# Backups and recovery

The supported backup unit is a **stopped complete Lume VM**, never a live `disk.img`. A SIP-disabled Apple-silicon VM depends on the disk, NVRAM, and virtual hardware identity staying coherent. Lume's clone operation copies that paired state together.

Examples use the canonical installed command path:

```sh
GREMVM="$HOME/Library/Application Support/GremVM/bin/gremvm"
LUME="$("$GREMVM" runtime-path)"
```

## Recommended 3-2-1 layout

1. Live VM in `~/Library/Application Support/GremVM/vms/work`.
2. Bootable stopped clones on a separately mounted APFS (Encrypted) external disk.
3. Encrypted, deduplicated restic snapshots in a second physical or off-site repository.

An external Lume clone is immediately useful for recovery, but it has no independent integrity catalog, retention engine, or off-site protection. Restic supplies those layers. Time Machine may back up a stopped clone, but do not rely on a Time Machine copy of the live VM image.

## Prepare the first backup volume

In Disk Utility, format a dedicated external SSD as APFS (Encrypted). Losing its passphrase loses this copy, so keep recovery material outside the guest. Mount it and create the repository as the host user that owns the VM:

```sh
mkdir -m 700 "/Volumes/GremVM Backup/lume"
```

GremVM deliberately refuses to create missing `/Volumes` paths. If the disk is absent, this prevents a large unencrypted backup from silently landing on the host startup disk. It also refuses a same-filesystem destination because an APFS clone on the live disk is only a checkpoint, not disaster recovery.

## Create a bootable clone

```sh
"$GREMVM" backup --destination "/Volumes/GremVM Backup/lume"
```

The command requires the GremVM supervisor to be loaded and the VM to have reached `state: running`. This restriction excludes raw/unmanaged Lume processes, whose exit cannot be proven safely. If the VM is stopped, run `"$GREMVM" start`, wait for `state: running`, and retry. The command then:

1. requests `/sbin/shutdown -h now` through a shutdown-only SSH key;
2. requires Remote Login to remain unavailable for repeated polls, then waits an additional disk-settle interval;
3. terminates and reaps the exact runner identity recorded by the supervisor, then unloads that supervisor while a crash-recoverable lifecycle lock prevents restart;
4. asks Lume to clone disk, NVRAM, and configuration to a timestamped name;
5. verifies Lume can read the clone and that all three required files exist;
6. writes `gremvm-backup.json`; and
7. restarts only if it was running beforehand.

Lume 0.4.0 does not exit its foreground loop when macOS halts, so runner exit alone cannot prove shutdown. This protocol first authenticates the shutdown request, then uses sustained loss of guest SSH plus a settle interval as conservative operational evidence before reaping only the recorded managed process. It is not a Virtualization.framework state attestation. If that evidence or runner identity cannot be confirmed, the operation aborts before copying and never invokes Lume's destructive stop fallback. Existing VM data and completed backups are preserved. A failed clone directory has no `gremvm-backup.json` completion manifest and must not be used as a backup. No automatic deletion or retention runs.

## Add encrypted off-site history with restic and Keytap

The Nix shell contains pinned restic and Keytap tools. Keytap can deterministically derive the repository password instead of storing a plaintext password or an age envelope in this repo:

```sh
nix develop path:.
keytap remember gremvm-restic

export RESTIC_REPOSITORY='s3:s3.example.net/my-private-bucket/gremvm'
restic --password-command 'keytap reveal gremvm-restic --as hex' init
```

`keytap remember` is an explicit machine-local choice. On macOS it stores the derived key in the owning user's login keychain; consequently unattended restic work is available only after that user logs in, matching the VM LaunchAgent boundary. This repository contains no backup password and no additional ClipKitty recipient.

After creating a stopped clone, back up that exact directory:

```sh
CLONE='/Volumes/GremVM Backup/lume/work--20260724T210000Z'
CONTROL="$HOME/Library/Application Support/GremVM"

restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  backup \
  --group-by host,tags \
  --tag gremvm \
  --tag work \
  --exclude "$CONTROL/state/maintenance.lock" \
  --exclude "$CONTROL/state/runner-owner" \
  "$CLONE" \
  "$CONTROL/config" \
  "$CONTROL/ssh" \
  "$CONTROL/state" \
  "$CONTROL/versions"
```

The small control-plane directories are included because a host-loss recovery needs the pinned configuration, provision state, forced shutdown private key, and pinned guest host key. The transient operation lock is excluded. Restic encrypts this material; keep restored key/state files mode `0600` and directories mode `0700`. The Lume runtime and logs are deliberately excluded because `gremvm install` reconstructs the runtime from its reviewed pin.

Repository credentials for S3/B2/etc. are separate from the restic encryption password. Keep them in an existing OS keychain or backup product, not this repository or the guest.

## Retention and verification

Run a dry run first. `--group-by host,tags` is essential because each clone has a different timestamped source path; without it, restic would apply retention independently to one-snapshot path groups.

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

Review the groups and selections, then repeat without `--dry-run` and add `--prune`:

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
  --prune
```

This restic policy does not remove the directly bootable clones on the external SSD. Review those separately after a successful off-site snapshot and restore drill. Keep at least two known-good local generations, inspect an exact old clone with `"$LUME" get OLD_NAME --storage "/Volumes/GremVM Backup/lume"`, and delete only that named clone with `"$LUME" delete OLD_NAME --storage "/Volumes/GremVM Backup/lume"`. Never automate local deletion and never remove the newest verified copy.

Also run a repository metadata check weekly:

```sh
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  check
```

Each month, replace `N` with the next number from 1 through 12 so the whole repository is read over a year:

```sh
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  check --read-data-subset=N/12
```

Run a full restore/boot drill quarterly.

Restic documents [repository setup and password commands](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html), [retention](https://restic.readthedocs.io/en/stable/060_forget.html), and [restore](https://restic.readthedocs.io/en/stable/050_restore.html).

## Restore without overwriting production

Always restore under a new name/path. Preserve the live VM until the recovery copy has booted and its application data is checked.

Resolve the installed, pinned Lume executable first; it is intentionally not added globally to `PATH`:

```sh
LUME="$("$GREMVM" runtime-path)"
```

Only directories containing a valid `gremvm-backup.json` are complete backups. For a direct Lume clone on the external volume:

```sh
# Inspect the timestamped backup first.
"$LUME" get work--20260724T210000Z \
  --storage "/Volumes/GremVM Backup/lume" \
  --format json

# Clone it back to a NEW VM in live storage.
"$LUME" clone work--20260724T210000Z work-restore-test \
  --source-storage "/Volumes/GremVM Backup/lume" \
  --dest-storage "$HOME/Library/Application Support/GremVM/vms"
```

For restic, restore sparse files to a new scratch directory so the virtual disk does not allocate its full logical size:

```sh
mkdir -m 700 "/Volumes/Restore Scratch/work"
restic \
  --repository "$RESTIC_REPOSITORY" \
  --password-command 'keytap reveal gremvm-restic --as hex' \
  restore latest \
  --tag gremvm,work \
  --sparse \
  --target "/Volumes/Restore Scratch/work"
```

Then locate the restored VM directory and use `"$LUME" get`/`"$LUME" clone` with direct `--storage` paths. Restore `config`, `ssh`, `state` (without `maintenance.lock`), and `versions` separately with directory mode `0700` and file mode `0600`; do not overwrite a working host control plane until the recovery copy has been inspected. Keep the production supervisor stopped while using raw Lume. Boot only the new `work-restore-test`, verify work files and application state, run `csrutil status` inside it, and confirm the result is `disabled`. Shut the test guest down from inside macOS before using `"$LUME" stop` or deleting anything, and never run the restored and original identities simultaneously.

Never restore by copying only `disk.img`, never overwrite `work` in place, and never prune the last verified external and off-site copies together.

The Keytap name/passkey and the storage backend credentials are separate recovery dependencies. Sync the Keytap passkey through its intended mechanism, escrow backend recovery material outside the guest, and perform the quarterly restore from a second Apple-silicon Mac running the same or a newer macOS version than the Tahoe guest so the test covers loss of the Mac Studio itself.
