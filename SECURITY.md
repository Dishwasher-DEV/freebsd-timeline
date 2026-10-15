# Security

## Threat model

freebsd-timeline runs a root daemon that can destroy snapshots, roll back datasets and
switch boot environments. The design goal is that this power is only
reachable by people you've chosen, and that nothing freebsd-timeline does leaks data
off the machine.

### Who can talk to freebsd-timelined

- The daemon listens on a Unix socket (`/var/run/freebsd-timelined.sock`) and nothing
  else. It never opens a network port.
- The socket is mode `0660`, owned by `root:freebsd-timeline`. Only root and members of
  the `freebsd-timeline` group can connect. Without a `freebsd-timeline` group the socket is
  root-only.
- Treat membership in `freebsd-timeline` like membership in `operator`: those users can
  roll back and destroy snapshots.

### Input handling

- Every dataset, snapshot, boot environment, label and SSH target is checked
  against a strict allow-list before use. Anything starting with `-` is
  refused, so names can't be turned into command-line options.
- Commands are executed directly with an argument vector. No shell is ever
  involved.
- Requests are limited to 64 KiB and must parse as one of a fixed set of
  operations.
- Passphrases sent for `unlock` are passed to `zfs load-key` on stdin. They
  are never logged or written to disk.

### Data that leaves the machine

- None, unless you configure `[backup]`.
- Backups use `zfs send -w` (raw). An encrypted dataset is transferred and
  stored *still encrypted*; the backup host has the ciphertext but not the key.
  Unencrypted datasets are protected in transit by SSH but stored in the clear
  on the target. Encrypt the datasets you back up.
- SSH runs with `BatchMode=yes` and your existing keys and `known_hosts`. freebsd-timeline
  never accepts unknown host keys on your behalf.
- There is no telemetry, update check or crash reporting.

### What freebsd-timeline will not do

- Prune snapshots it did not create, or manual / pre-upgrade snapshots.
- Destroy an active boot environment.
- Lock the encryption root that holds `/`.
- Overwrite a backup target that shares no snapshot with the source.

## Reporting a vulnerability

Please open a private security advisory on GitHub rather than a public issue.
