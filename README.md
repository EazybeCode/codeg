# Neev / Neevz — maintainer handover

Neev is a shared workspace for AI agents, collaborative documents, files,
shared integrations, and organization knowledge, built on a fork of Codeg.
The hosted app is [app.neev.so](https://app.neev.so).

## Claude or Codex: start here

Read **[the handover guide](docs/NEEV-HANDOVER.md)** before changing or deploying
the app. It covers the architecture, setup, tests, deployment, and remaining work.

| Branch | Purpose |
| --- | --- |
| [`teams/client-layer-oct12`](https://github.com/EazybeCode/codeg/tree/teams/client-layer-oct12) | Neev application and current production release line. Start application work here. |
| `main` | GitHub default branch, upstream baseline, and this handover. It is **not** the source of the currently deployed Neev app. |

The deployment checked on **2026-10-26** runs commit
[`160a45bc`](https://github.com/EazybeCode/codeg/commit/160a45bca3a09e7b644d776f40651904ec14dc93)
from `teams/client-layer-oct12`. Newer local work is being consolidated onto
that release branch. Publishing to GitHub and deploying are separate steps;
verify both before saying a change is live.

For a fresh application checkout:

```bash
git clone --branch teams/client-layer-oct12 https://github.com/EazybeCode/codeg.git neev
cd neev
```

Read `AGENTS.md`, `CLAUDE.md`, and any referenced organization instructions.
The hosted app uses `neev-server` and `Dockerfile.native*`; the generic Codeg
server is a different runtime.

## Handover contents

- [Branch and release status](docs/NEEV-HANDOVER.md#branch-and-release-status)
- [Source map](docs/NEEV-HANDOVER.md#source-map)
- [Development and verification](docs/NEEV-HANDOVER.md#development-and-verification)
- [Publishing and deployment](docs/NEEV-HANDOVER.md#publishing-and-deployment)
- [Remaining work](docs/NEEV-HANDOVER.md#remaining-work)
- [Prompt for the next agent](docs/NEEV-HANDOVER.md#prompt-for-the-next-agent)

## Upstream project

The original Codeg overview and installation guide are preserved in
[README.upstream.md](README.upstream.md). Upstream's feature list is not a list
of verified hosted Neev capabilities. The [Apache-2.0 license](LICENSE) is preserved.
