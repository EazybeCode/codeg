# gVisor job runtime

## Current host preparation

On 2026-09-09 the owner approved an optional gVisor runtime on the existing Hetzner host. Installed the complete official `release-20260831.0` archive (including sidecars) under `/opt/workos/gvisor/release-20260831.0`. The downloaded archive passed its published SHA-512 check. `bzip2` was the only newly installed OS package; no package upgrades were performed.

Docker runtime registration:

```json
{
  "workos-runsc": {
    "path": "/opt/workos/gvisor/release-20260831.0/runsc",
    "runtimeArgs": ["--platform=systrap"]
  }
}
```

The registration was prepared in a candidate daemon configuration, validated with `dockerd --validate`, and checked to preserve all non-runtime configuration. Docker was reloaded, not restarted. Its default runtime remains `runc`. Existing app and PostgreSQL containers retained their original start times and healthy status.

The previous daemon configuration is preserved in the root-only directory `/var/lib/workos-bootstrap/gvisor-20260909/daemon.before.json`. Do not blindly restore it over subsequent unrelated Docker configuration changes. To remove this runtime later, stop only its verified job containers, remove only the `workos-runsc` registration, validate the resulting configuration, and reload Docker. Do not change the default runtime or restart unrelated applications.

## Verified smoke test

Two disposable containers used the explicitly selected `workos-runsc` runtime with:

- network `none`, no published ports, no host mounts or Docker socket;
- read-only root, uid/gid `65534`, dropped capabilities, no-new-privileges;
- separate 8 MiB temporary workspaces;
- 0.5 CPU, 128 MiB memory with no extra swap allowance, 64-process limit.

Both ran successfully. A synthetic marker created in one workspace was absent from the other, and neither exposed the Docker socket. Runtime selection was checked from the host's Docker configuration/inspection; guest `dmesg` output was supplementary only. Both fixture containers were removed. No application data or GitHub data was deleted.

## Not yet an enabled agent service

Installing a runtime does not establish application tenant isolation or enable cloud jobs. `WORKOS_ENABLE_CATALOG_RUNTIME` remains disabled. The job integration must independently enforce:

- server-derived organization/user/chat identities and current membership/chat authorization;
- creator/private versus organization-wide chat audiences;
- per-job filesystem roots without shared unpublished Git objects or another chat's working data;
- no operator credentials, Docker socket, host directories or another user's agent credentials in a job;
- restricted provider/Git network access, no access to app/database/metadata/private-network endpoints;
- bounded CPU, memory, process count, disk, run duration, concurrency and output;
- durable job ownership, cancellation, restart recovery and no automatic replay of prompts;
- real provider compatibility and two-organization authorization/runner acceptance before enabling execution.

The smoke test is not an adversarial sandbox assessment, provider compatibility test or proof that the remaining application layer is complete.

Sources: [official installation guide](https://gvisor.dev/docs/user_guide/install/), [Docker runtime configuration and reload behavior](https://docs.docker.com/reference/cli/dockerd/), [gVisor production/platform guidance](https://gvisor.dev/docs/user_guide/production/).
