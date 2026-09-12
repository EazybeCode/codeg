# Neev frontend dependency security acceptance

Date: 2026-09-13

This acceptance covers a local Node 22.23.2 / pnpm 11.9.0 dependency graph and
the existing frontend behavior gates. It does not claim a clean dependency
audit, rebuild a native image, or authorize deployment.

## Reviewed changes

The workspace override policy now resolves:

- `linkify-it` from 3.0.3 to 5.0.2 for the `ansi-to-react` terminal renderer;
- `lodash-es` from 4.17.23 to 4.18.1 for the lazy
  `@streamdown/mermaid` -> Mermaid parser/render graph; and
- `postcss` to one 8.5.23 resolution for the direct build dependency,
  Tailwind/PostCSS, Next, shadcn, and Vitest/Vite paths.

`uc.micro` moved from 1.0.6 to 2.1.0 with `linkify-it`, and the obsolete
`nanoid` 3.3.11 resolution disappeared when PostCSS moved. No application API
or source behavior changed. `package.json` was intentionally unchanged.

The originally reviewed Lodash floor was 4.18.0, but npm marks that release
`Bad release`. Lodash's [4.18.1 release notes][lodash-release] explain that the
generated modular builds missed internal dependency-map updates and could throw
`ReferenceError` from `template` and `fromPairs`; 4.18.1 is the current
non-deprecated replacement. The originally reviewed PostCSS floor was 8.5.18,
but the later [incomplete-fix advisory][postcss-incomplete] requires 8.5.23.
Those two narrow patch-floor amendments were approved during review.

The standalone `shadcn` package was not removed. `src/app/globals.css` imports
its exported `shadcn/tailwind.css`; that is an actual application consumer.
This is distinct from `@blocknote/shadcn`, whose stylesheet is imported by the
native rich editor.

The protected versions remain unchanged: Next 16.3.4, BlockNote 0.54.2, and
Tiptap 3.31.3.

## Audit result

`pnpm audit --json` failed before and after because residual advisories remain:

| Audit | Low | Moderate | High | Critical | Total advisories | Total dependencies |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Before | 14 | 77 | 52 | 1 | 144 | 1,423 |
| After | 14 | 74 | 44 | 1 | 133 | 1,421 |
| Change | 0 | -3 | -8 | 0 | -11 | -2 |

The final audit contains no `linkify-it`, `lodash-es`, or `postcss` advisory.
The resolved changes address the reviewed [Linkify scan-loop][linkify-scan] and
[`mailto:` validator][linkify-mailto] advisories, Lodash
[`template` injection][lodash-template] and [`unset`/`omit` pollution][lodash-unset]
advisories, and the applicable PostCSS [XSS][postcss-xss],
[source-map read][postcss-read], [path traversal][postcss-traversal], and
[incomplete-fix][postcss-incomplete] advisories. The newer PostCSS advisory is
why the accepted final version is 8.5.23 rather than 8.5.18.

This is not a clean-audit claim. The remaining 133 entries comprise 31
development-only advisories (9 moderate, 21 high, 1 critical) and 102 entries
reported on production dependency findings (14 low, 65 moderate, 23 high).
Major affected roots include the retained `shadcn` stylesheet package (71
advisories), the ESLint/Next lint graph, Vitest/Vite test tooling,
`@streamdown/mermaid`, Monaco, Next, and `next-intl`. A missing direct import is
not treated as proof that a transitive path is unreachable.

The critical residual is [GHSA-5xrq-8626-4rwp][vitest-critical], Vitest 2.1.9
through the direct `vitest`, `@vitest/ui`, and `@vitest/coverage-v8` development
paths; the audit's patched candidate is `>=3.2.6`. Critical and high residual
IDs, affected versions, patched candidates, and representative dependency paths
are recorded in the implementation task report for scoped follow-up. They were
not force-overridden because that would be a broader, unreviewed toolchain/CLI
upgrade.

## Verification evidence

All commands used `/opt/homebrew/opt/node@22/bin` and ran serially:

- `pnpm install --ignore-scripts`: exit 0; 1,421 packages resolved; no lifecycle
  scripts run.
- `pnpm why shadcn ansi-to-react linkify-it @streamdown/mermaid mermaid lodash-es postcss`:
  exit 0; one resolved version each for the three overridden packages.
- `pnpm test`: exit 0; 468 test files and 6,419 tests passed. The suite continued
  to emit existing expected stderr/warnings (including React `act`, jsdom canvas,
  hydration, and mocked failure-path logs); none were test failures.
- `pnpm exec tsc --noEmit`: exit 0, no diagnostics.
- `pnpm build`: exit 0; Next 16.3.4 compiled, typechecked, and generated all
  34 static routes.
- `git diff --check`: exit 0.

This evidence accepts the local package graph and frontend export only. The
native image has not been rebuilt or deployed; image review and deployment are
separate release gates.

[linkify-scan]: https://github.com/advisories/GHSA-22p9-wv53-3rq4
[linkify-mailto]: https://github.com/advisories/GHSA-v245-v573-v5vm
[lodash-template]: https://github.com/lodash/lodash/security/advisories/GHSA-r5fr-rjxr-66jc
[lodash-unset]: https://github.com/lodash/lodash/security/advisories/GHSA-f23m-r3pf-42rh
[lodash-release]: https://github.com/lodash/lodash/releases/tag/4.18.1
[postcss-xss]: https://github.com/advisories/GHSA-qx2v-qp2m-jg93
[postcss-read]: https://github.com/advisories/GHSA-6g55-p6wh-862q
[postcss-traversal]: https://github.com/advisories/GHSA-r28c-9q8g-f849
[postcss-incomplete]: https://github.com/advisories/GHSA-fxqj-rqcc-2cmp
[vitest-critical]: https://github.com/advisories/GHSA-5xrq-8626-4rwp
