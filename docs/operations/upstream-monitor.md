# codeg update monitoring

The **WorkOS upstream monitor** Action checks public `xintaofei/codeg` every six hours (00:17, 06:17, 12:17, 18:17 UTC), and supports **Run workflow**. GitHub scheduling is best effort. It must live on Workos's current default branch, `workos/team-workspace`; changing that branch requires reviewing the workflow guard.

Each run's summary and `upstream-discovery-<run>-<attempt>` artifact contain the exact downstream, observed upstream and incorporated SHAs, commit count and changed paths. Artifacts expire after 90 days. An unchanged upstream is `current`. Pending commits require human review. Rewritten history, missing objects, changed expected branches and tool/network errors fail closed. Check the failed step; no checkpoint or application change occurs.

The incorporated checkpoint in `.github/workos-upstream.json` is deliberately separate from the latest observed SHA in each report. Update it only after reviewed upstream commits are actually incorporated into Workos. Selective cherry-picks require separate incorporation accounting before advancing a contiguous checkpoint.

## Security and current limits

This first stage only monitors. It never checks out or runs upstream code, installs candidate dependencies, creates proposals, merges, deploys, or uses an AI adaptation provider. It has read-only repository permission and no production/customer/provider secrets. All nonempty ranges require human review; a future copy-only lane needs content validation and full regression gates, not extension-based guessing.

Automatic proposal preparation, adaptation, scoped PR publication and protected release are **not implemented by this monitor**. Production remains pinned in Coolify with auto-deploy disabled. Review the full design in `docs/superpowers/specs/2026-09-08-upstream-update-automation-design.md` before adding these stages.

## Local verification

```bash
node --test scripts/upstream/discover.test.mjs
node scripts/upstream/discover.mjs <full-incorporated-sha> <full-observed-sha> <full-downstream-sha>
```

The CLI writes `upstream-report/report.json` and `upstream-report/summary.md` in the current directory. Run in a disposable checkout for actual monitoring. It never changes Git refs, tracked files or the checkpoint. Blocked history returns exit 2; malformed input/tool failure returns exit 1.
