# Neev credential volumes

This helper is the storage-only encrypted-volume boundary for future personal
agents. It does not authenticate a supervisor, contact a provider, refresh a
session, expose HTTP routes, run jobs, or enable agent execution.

## Installation and fixed paths

Install `scripts/operations/neev_credential_store.py` and
`scripts/operations/neev_credential_volumes.py` as root-owned, non-writable
program files. Before the first initialization, the host installer must create
the parent explicitly:

```text
/var/lib/neev-supervisor                 root:root 0700
```

`init` then creates only these immediate private roots and the store metadata:

```text
/var/lib/neev-supervisor/credentials     root:root 0700
/etc/neev-supervisor                     root:root 0700
/etc/neev-supervisor/credential-master.key root:root 0600
```

There is deliberately no CLI option for a root, key, image, command, binary,
environment, mount option, or device. The root-only CLI is:

```text
python3 neev_credential_volumes.py init
python3 neev_credential_volumes.py create --org-id N --user-id N --provider claude|codex
python3 neev_credential_volumes.py open --org-id N --user-id N --provider P --handle H
python3 neev_credential_volumes.py close --org-id N --user-id N --provider P --handle H
python3 neev_credential_volumes.py status --org-id N --user-id N --provider P --handle H
```

Run `init` exactly for first installation or deliberate recovery setup. A
normal service restart must not run provisioning and must not infer state from
the existence of a file. The CLI emits only bounded JSON fields `handle`,
`state`, or `error`; it never emits a derived key, filesystem path, provider
account state, or tool output.

The reviewed Linux fixture uses the following fixed root-owned tools. There is
no `PATH` search. On the acceptance host, `/usr/sbin/mkfs.ext4` is the canonical
symlink to the root-owned `0755` `/usr/sbin/mke2fs`; the helper verifies that
resolution. It also verifies `/usr/sbin/cryptsetup`, `/usr/bin/findmnt`, and
`/usr/sbin/losetup` as root-owned `0755` regular files, and `/usr/bin/mount` and
`/usr/bin/umount` as root-owned `04755` regular files. The reviewed cryptsetup
version is 2.8.4. Tool calls use `LC_ALL=C`, a minimal fixed `PATH`, no shell, a
30-second deadline, and a 64 KiB combined-output limit.

## Lifecycle and ownership

Every operation takes the global credential-store lock. A manifest MAC binds
the numeric organization, numeric user, provider (`claude` or `codex`), and
random handle to the persistent root key. The derived volume key is supplied to
cryptsetup only on standard input. It is never placed in arguments, the
environment, a file, JSON, or logs.

Explicit `create` exclusively allocates a new entry and a `0600` regular
`volume.luks`; it never formats an existing path. It reserves a 96 MiB host
image only when at least 512 MiB free remains afterward. LUKS2 uses
AES-XTS-plain64 with a 512-bit key and the fixed PBKDF2 parameters in source.
The filesystem command creates exactly 16,384 4096-byte ext4 blocks (64 MiB)
inside the mapping. ext4 metadata consumes some of that capacity, so 64 MiB of
user payload is not guaranteed and ENOSPC is the quota boundary.

`open` requires a ready authenticated manifest and an actual LUKS UUID equal to
the signed value. Before reusing resources, it verifies that the mapping's loop
device refers to the exact image device and inode and that any mount refers to
that mapping's major/minor. The protected mount has `nodev,nosuid,noexec`; its
filesystem root is owned by UID/GID 65532 and mode `0700`. Foreign mappings,
foreign mounts, nested mounts, malformed or ambiguous status, and incomplete
provisioning all fail closed.

`close` normally unmounts and then closes only a mapping proven to belong to
the exact image. It never uses force or lazy unmount. A busy unmount leaves the
mapping and mount open for an administrator to inspect. A failed create retains
its image and provisioning manifest; it closes a mapping only when that exact
invocation opened it. There is no automatic filesystem repair, partial-state
deletion, lost-key replacement, or global device scan. Ordinary ext4 journal
recovery on remount is allowed, but unflushed application writes are not
promised to survive power loss.

## Backup, recovery, and security boundary

Credential images and the master key are excluded from ordinary WorkOS/native
application backup and restore. Existing legacy services, images, sources, and
backup archives are unchanged; these new roots are outside their current
archive sources. Future native backup coverage is not accepted by this slice.

A missing, unreadable, changed, or corrupt key fails closed without replacing
the key or deleting ciphertext. Safe recovery is personal reauthentication and
explicit provisioning of a new volume after an administrator has preserved and
diagnosed the old artifact. Do not copy a key or encrypted image into database
records, telemetry, application logs, browser storage, screenshots, Git, or an
ordinary backup.

This is host-key sector encryption, not TPM-backed storage, tamper-proof media,
or forensic erasure. Host administrators and full-host compromise remain
outside the protection claim.

## Test and release gates

Ordinary tests use real temporary store/image files and a simulated privileged
tool boundary; they never invoke cryptsetup, mount, or other storage tools:

```text
python3 -m unittest scripts.tests.test_neev_credential_store scripts.tests.test_neev_credential_volumes
```

After committed source has passed review and a secret scan, the controller may
run the opt-in fixture on Linux as root:

```text
sudo env NEEV_CREDENTIAL_VOLUME_ACCEPTANCE=1 /usr/bin/python3 -m unittest \
  scripts.tests.test_neev_credential_volumes.LinuxCredentialVolumeAcceptance
```

It creates a new private directory under `/var/tmp`, exercises real
create/open/write+fsync/close/process-reopen/read, scope rejection, ciphertext
inspection, mount flags and ownership, ENOSPC and recovery after deleting only
its invented filler, and missing/corrupt-key refusal. It retains the encrypted
artifact and key for controller inspection and prints the exact fixture path.
It closes only its owned safe mapping/mount identifiers in `finally` and reports
them if cleanup fails. This is process-reopen evidence, not a host-reboot test.

The Linux acceptance remains pending until the controller runs that command.
Authenticated supervisor controls, egress enforcement, browser/session and
generation handling, real provider/account tests, HTTP/job wiring, host reboot,
provider refresh, native-backup exclusion validation, and public release are
separate missing gates. No runtime may consume this helper until those gates
are accepted.
