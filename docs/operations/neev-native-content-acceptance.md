# Native chats and documents acceptance

September 12, 2026. This record separates source-linked automated evidence from
the controller's actual-browser and isolated-restore evidence. Native identity is
the authority; every chat and document belongs to one existing organization node.
`runtimeEnabled:false` remains authoritative. Human chat messages and manual
documents do not prove personal-agent execution, notifications or wiki synthesis.

## Automated local boundary

The ignored real-HTTP regression uses two native participants and two loopback
listeners over one newly owned PostgreSQL schema. Both listener states clone one
fixture-owned `BlobStore`; a revision written through participant 1 is read through
participant 2. The workflow also proves private denial, private-to-organization
sharing, two-user message attribution, stale-save `409`, append-only restore with
parent revision `2` and source revision `1`, source-chat privacy revocation returning
`404`, and post-removal denial.

Every fixture HTTP client has a five-second request timeout. The content workflow
has a 120-second overall deadline; a timed-out action task is aborted and awaited
before shutdown. Shutdown gives each listener 15 seconds, then aborts and awaits
it before closing the pools, dropping only the exact owned UUID schema, and
dropping the single owned temporary document root. The regression asserts that
the root no longer exists after shutdown. No configured application directory is
used or removed.

`scripts/neev-content-preflight.mjs` is GET-only. It accepts only an explicit
literal `127.0.0.1` or `[::1]` HTTP origin with a port and an exact
`n1.session.<43 base64url characters>` native session. It sends only the preview
session cookie, refuses redirects, has five-second request and 15-second overall
deadlines, requires native/runtime-false/chat/document capabilities, and reads
only bounded first pages of native nodes/chats/documents. Its output contains only
mode flags and aggregate counts. It never reads a browser cookie store or outputs
the session, CSRF, user/org IDs, resource IDs or bodies.

The Rust regression invokes this preflight against its actual second listener.
The fixture session is passed only through a cleared child environment, retained
in memory, and neither printed nor persisted. Browser acceptance must not extract
or inject cookies to repeat this check.

The focused TDD evidence and fresh full-suite/build results are recorded in the
Task 8 execution report. Baseline compiler/frontend warnings remain disclosed;
they are not converted into successes or removed outside this scope.

## Controller-owned browser handoff

Use Node 22 and the guarded PostgreSQL URL. Start only this exact ignored driver
in a PTY; stdin EOF or a typed line stops it. Do not kill it as normal cleanup.

```sh
PATH=/Users/air1/.cargo/bin:/opt/homebrew/opt/node@22/bin:$PATH \
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
NEEV_TEST_DATABASE_URL='postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1' \
cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features \
  neev::browser_fixture::browser_acceptance_driver -- \
  --exact --ignored --nocapture --test-threads=1
```

The driver prints only the exact owned schema, canonical owned document root, two
app origins and capture-inbox origin. It never prints token, code, cookie, key or
mail-body values. The two app listeners share that schema and root. Type a line
to stop; the 45-minute limit is a fallback. Normal shutdown stops/awaits listeners,
closes pools, removes the exact schema, and removes the exact root.

The controller, not an implementation agent, records these actual Chrome checks:

- Create a project, nested folder, private chat and organization-shared chat;
  verify two human author rows and another organization's denial.
- Create/edit/save a source-bound document; preserve a conflicting draft on
  `409`; compare revisions; restore an old revision as a new appended revision.
- Change the source chat back to private and verify stale loaded chat/document
  content disappears for the member. Remove the member and verify denial again.
- With a dirty chat/document, switch browser tabs and return. Verify an unchanged
  session retains the draft, while logout or resource revocation clears it.
- Exercise save, Escape, Back, confirmation and comparison dialogs with keyboard
  focus restoration. Keep a contained dialog open during the tab/revalidation
  journey.
- In native DevTools responsive mode, record actual `innerWidth` and `scrollWidth`
  at 375 pixels for workspace, chat, document history/comparison and dialogs.
- Render malicious Markdown containing external images/links and operator/file
  paths. Record the actual network/console observations and verify no automatic
  image/network/operator-path access. A source assertion or preparation runbook is
  not browser evidence.

### Observed controller evidence

The September 12 controller run used two distinct loopback cookie hosts and the
frozen static export. It passed project/folder nesting, private then explicitly
organization-shared chat, separately attributed human authors, loaded-content
clearing after privacy restriction, explicit document save, cross-listener body
read, preserved conflicting draft, comparison, and append-only restore from
revision 1 to revision 3 while retaining revisions 1 and 2. The runtime remained
disabled and no simulated agent reply appeared.

Actual malicious Markdown produced zero `img` and `iframe` elements in the app;
a separate owned probe remained at one control request and one image request
before and after preview. Browser-extension feature-flag/message-port errors were
distinguished from the application; no application stack was observed.

That run found three defects, fixed by scoped Task 8 regressions and retested in a
fresh fixture: document `409` now explains that the document changed and the
draft was preserved; Escape from revision comparison returns focus to the exact
comparison button only while it is connected, enabled and in the same active
scope; and a foreground content `403` now revalidates session authority, removing
the entire organization header/tree and creation controls after membership
revocation. The final revocation reached the full sign-in surface in about three
seconds with no old content, draft or dialog remaining.

The Make-private action now has visible, accessible localized text explaining
that future access is blocked but copies already downloaded cannot be recalled.
The retest measured an actual `375×812` viewport with `scrollWidth` `375`; the
mobile drawer, project/settings/invitation/document/chat actions and `Meta+S`
worked, and the 343-pixel comparison dialog did not overflow.

One minor feedback issue remains: a later content poll can clear the visible
document-conflict alert while the preserved draft and **Compare with latest**
recovery remain. Content-specific text was observed immediately after a repeated
conflicting save. This is follow-up polish, not a claim that the message remains
persistently visible.

The optional source-bound-document UI journey, unchanged-authority dirty
hide/show journey and separate other-organization browser denial were not run.
Real-HTTP/native regressions cover source-chat privacy and cross-organization
denial, but those automated results are not relabeled as browser evidence.

Screenshots and summaries must contain no cookies, OAuth/magic-link values, mail
bodies or private fixture content. Distinguish application failures from browser
extensions. Synthetic participants are not real users or provider acceptance.

## Fixture-backed backup and isolated restore

While the interactive fixture remains normally owned and contains synthetic
documents, the controller may use its printed schema and document root with
`scripts/neev-content-backup.mjs`. Follow
[the native backup procedure](neev-content-backup.md) exactly: pass the guarded
connection through `NEEV_BACKUP_DATABASE_URL`, select the exact printed schema,
use a new output under an existing owned parent, verify `complete.json`, restore
only into a fresh isolated destination, copy blobs without hard links, and run the
named native verifier against the explicit database/schema/root. Do not initialize
the restore with the application and do not use `--clean`.

The browser fixture's source schema and root are removed only after the controller
finishes the backup inputs and requests normal fixture shutdown. The restored
database/root and backup output have separate exact ownership and cleanup records.
An automated verifier pass does not substitute for this controller exercise.

The controller completed this exercise against three browser-created historical
blobs. The metadata dump was 79,039 bytes with SHA-256
`e0cdf453012ef5cc982803809d7ce22a07477144682d34edbe2fae04dd056433`. The
fresh isolated database `neev_restore_9fcd806b118f4ba18718d1ac5039b3d4`
verified schema version 3, three referenced blobs and 608 referenced bytes; its
manifest SHA-256 was
`7d1f4aecf9c55451b1087dced6cd44d1cb8236fcc59676e92cb251eac139c235`. A
separate copied historical blob was quarantined and the verifier failed closed
with exit 1. The positive recovery artifact remains at
`/private/tmp/neev-content-recovery.R5JKHR`; this local exercise does not prove
hosted scheduling or independent recovery custody.

## Acceptance status and remaining gates

The automated HTTP/preflight, actual local Chrome and local isolated
backup/restore gates have controller evidence. Both interactive fixtures stopped
normally; their exact owned schemas returned count zero and their exact temporary
document roots were absent. The final amended frontend passed 6,361 tests across
465 files, and the production native server smoke returned healthy/native,
`runtimeEnabled:false`, chats/documents enabled, and no synthetic Google route.
These local results do not label the public product release-ready.

Real Google policy/client/account behavior, actual mail delivery, two consenting
real users, private HTTPS/proxy/cookie behavior, reviewed image/entrypoint,
publication through the interactive Workos `ship` gate, hosted backup scheduling,
recovery custody, aggregate disk/capacity monitoring and backup-time headroom all
remain open. Per-organization quotas do not prove aggregate hosted capacity.

Personal subscription onboarding, visible real provider/model/effort controls,
author-scoped prompt/reply behavior in shared chats, notification preferences and
delivery, organization wiki generation/indexing with privacy exclusion, and
durable isolated jobs remain later stages. Runtime stays disabled. Founder
feedback and receiving inboxes remain explicitly deferred.

Neev favicon/app-icon consistency remains part of the later website/branding gate;
the inherited public icon family is not accepted by this content milestone.
Read-only upstream discovery also remains distinct: automatic adaptation,
proposal, merge and deployment are not implemented or accepted here.

Historical architecture context: [org-memory: wiki/engineering/workos.md]. The
legacy operator/GitHub model on that page is superseded for native SaaS authority;
no GitHub repository, branch or organization is changed by this acceptance.
