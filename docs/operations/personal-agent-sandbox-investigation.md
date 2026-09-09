# Personal-agent sandbox integration findings

September 9, 2026. Read-only investigation for the approved personal-connection
design. No supervisor, network policy or encrypted credential mount is implemented
by this document; no host setting was changed. The lifecycle implementation can
proceed without any live provider access.

## Network boundary still to prove

The approved design requires enforced provider-only egress, not just proxy
environment variables. gVisor documents an isolated in-sandbox loopback and a
network-disabled mode. Neither is evidence that our eventual provider clients can
authenticate under the required policy.
[Official networking guide](https://gvisor.dev/docs/user_guide/networking/).

The source for the installed release, `release-20260831.0`, distinguishes host
Unix-domain socket opening from creation, with explicit `none`, `open`, `create`
and `all` settings. This is a configurable host communication surface, not a
security guarantee for arbitrary mounted sockets.
[Pinned runtime configuration source](https://github.com/google/gvisor/blob/release-20260831.0/runsc/config/config.go).

An alternative worth a synthetic experiment is a network-disabled login guest
with an internal HTTP proxy bridge to one narrowly mounted egress-only Unix socket.
The host-side proxy would enforce the fixed provider profile. This is an inference
from the capabilities above, **not the selected deployment design**. It would need
review before replacing the approved dedicated-network approach. In particular:

- Verify the exact runtime flags with the installed release, without changing the
  host's default runtime or restarting unrelated applications.
- Demonstrate that only the intended socket is reachable: no host abstract-socket
  access, unintended socket bind/open, descriptor transfer or mount traversal.
  Never expose Docker, supervisor-control, database or host-directory sockets.
- Prove both pinned official clients work with the bridge, then unset all proxy
  variables and attempt direct IPv4/IPv6/private/metadata access from the guest.
  Those bypass attempts must fail independently of client cooperation.
- The egress proxy must validate exact destinations, resolve and validate every
  address before dialing, pin the checked address for that connection, and reject
  private/link-local/loopback and alternate address representations. CONNECT must
  not become a general-purpose tunnel. Provider URL validation is separate.
- Bound proxy headers, connections, bytes and lifetimes; never log authentication
  bodies, challenges or headers. Exercise malformed frames and reconnects.

The existing approved dedicated-network approach also needs actual denial tests
and an exact firewall/proxy policy. A shared proxy reachable from a bridge network
does not by itself block guest-to-host or guest-to-guest traffic. No generic host
firewall rewrite or permissive temporary internet access is authorized here.

gVisor's documented model still permits access to intentionally mapped files and
network services; host resource limits and network policy remain necessary. The
host administrator is trusted in this design.
[Official security model](https://gvisor.dev/docs/architecture_guide/security/).

## Backup compatibility is a separate migration gate

The existing [backup implementation](backups.md) validates four source archives,
including the legacy Claude and Codex operator homes. Personal credential volumes
must be newly allocated outside **every effective backup include path**; they must
not reuse those homes. Merely excluding a new directory from one command is not
evidence that a native Coolify volume job or broader archive excludes it.

Before enabling personal sign-in, inventory the actual native backup mounts and
archive sources using synthetic markers only. Prove that ordinary application and
off-server snapshots omit the new credential volumes. Do not print or extract real
credentials to test the rule. Personal account recovery requires reauthentication.

Do not simply disable the two legacy provider backup jobs: the current transport
requires all four archives and would reject a missing source. Any removal of
legacy mounts or archives needs a separately reviewed backup-configuration/test
change and owner-scoped data handling. Existing backup archives are preserved.
Restic encryption does not establish encryption at rest for a mounted live home.

## Evidence required before enabling a provider

| Gate | Required evidence |
| --- | --- |
| Client profile | Verified unmodified Linux binary, pinned image digest, exact status/login/logout schema and timeout/cancellation behavior |
| Credential custody | Encrypted live storage, bounded per-connection capacity, protected recovery key custody and verified ordinary-backup exclusion |
| Isolation | Actual runtime/mount inspection and denied foreign handles, host/private networking and cross-sandbox access |
| Recovery | Generation-bound cleanup survives supervisor restart without deleting a newer resource or replaying a login |
| Personal use | Owner and second participant authenticate directly and exercise status/disconnect plus person/org denial controls |

These gates remain open. The owner's Claude terms confirmation is recorded in
[provider contracts](personal-agent-provider-contracts.md); it is not a substitute
for these implementation and acceptance results.

Context: [org-memory: wiki/engineering/workos.md]. Its legacy operator homes are
not the personal multi-tenant credential boundary.
