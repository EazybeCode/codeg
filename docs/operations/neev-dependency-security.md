# Neev frontend dependency security acceptance

Date: 2026-09-13

This acceptance covers the local Node 22.23.2 / pnpm 11.9.0 dependency graph,
frontend behavior gates, static export, and native-image license packaging
instructions. It does not claim a clean dependency audit, rebuild a native
image, or authorize deployment.

## Reviewed changes

The standalone `shadcn` 3.8.4 CLI package is removed. Its only runtime asset
consumer now imports a vendored, byte-identical copy of `tailwind.css`: 95
lines, 1,669 bytes, SHA-256
`146941ac3ff65496fdf1cb306e697255328e4dadc7c105d66e36bb031b00f6d6`.
The upstream [MIT license][shadcn-license] is retained beside the stylesheet.
`Dockerfile.native` installs that notice into `/usr/share/doc/neev` and names
the asset in `THIRD_PARTY_NOTICES.txt`. The distinct `@blocknote/shadcn`
package and its existing notice remain present, as do internal
`project-boot/shadcn` source paths.

The test toolchain now resolves as one compatible set: Vitest, `@vitest/ui`,
and `@vitest/coverage-v8` 3.2.7; Vite 6.4.3; and
`@vitejs/plugin-react` 4.7.0. The official [Vitest 3 migration guide][vitest-3]
documents changed spy reuse, fake timers, coverage, and Vite 6 module-condition
behavior. The existing configuration does not use the removed options;
`clearMocks: true` isolates call history while preserving test doubles that
are intentionally configured once per file. The [Vite 6 migration guide][vite-6]
identifies changes to resolution conditions, JSON handling, PostCSS config,
and Sass; none of those non-default settings is used here.

Vitest 4.1.11 was evaluated because a newer moderate advisory lists it as the
patched floor. Its stricter matcher behavior no longer accepted an existing
AbortSignal assertion even though the application's timeout-composed signal
behavior was unchanged. The approved requirement is to remove critical/high
findings with Vitest at least 3.2.6, so the compatible 3.2.7 line is retained
and that moderate tooling advisory remains explicitly inventoried below. No
test assertion was weakened.

The following major-scoped overrides apply the highest patched floor required
by the current critical/high advisory set:

| Package majors | Resolved/policy floor | Official advisory evidence |
| --- | --- | --- |
| `brace-expansion` 1 / 2 / 5 | 1.1.18 / 2.1.4 / 5.0.9 | [GHSA-3jxr-9vmj-r5cp][brace-1], [GHSA-mh99-v99m-4gvg][brace-2], [GHSA-rgw5-rvv9-x895][brace-3] |
| `browserslist` 4 | 4.28.7 | [GHSA-c83g-rgw3-j3cx][browserslist-1], [GHSA-73wf-gq98-2v4g][browserslist-2] |
| `flatted` 3 | 3.4.2 | [GHSA-25h7-pfq9-p65f][flatted-1], [GHSA-rf6f-7fwh-wjgh][flatted-2] |
| `form-data` 4 | 4.0.6 | [GHSA-hmw2-7cc7-3qxx][form-data] |
| `glob` 10 | 10.5.0 | [GHSA-5j98-mcp5-4vw2][glob] |
| `js-yaml` 4 | 4.3.2 | [GHSA-52cp-r559-cp3m][js-yaml-1], [GHSA-5p4m-2wfm-xmqj][js-yaml-2], [GHSA-2883-xcg3-v3hh][js-yaml-3] |
| `minimatch` 3 / 9 / 10 | 3.1.4 / 9.0.7 / 10.2.3 | [GHSA-3ppc-4f35-3m26][minimatch-1], [GHSA-7r86-cg39-jmmj][minimatch-2], [GHSA-23c5-xmqv-rm74][minimatch-3] |
| `picomatch` 2 / 4 | 2.3.2 / 4.0.4 | [GHSA-c2c7-rcm5-vvqj][picomatch] |
| `ws` 8 | 8.21.0 | [GHSA-96hv-2xvq-fx4p][ws] |

The Task 1 floors remain `linkify-it` 5.0.2, `lodash-es` 4.18.1, and
`postcss` 8.5.23. Focused renderer coverage now exercises the actual
`ansi-to-react` fuzzy-linkification branch that consumes `linkify-it` and
confirms that application terminal output continues to leave linkification
disabled. The protected versions remain unchanged: Next 16.3.4, BlockNote
0.54.2, and Tiptap 3.31.3.

Vitest 3.2.7 clears the critical [arbitrary-file read and remote code execution
finding][vitest-critical], whose patched floor is 3.2.6. The single Vite 6.4.3
resolution clears the high [server.fs bypass finding][vite-high], whose patched
floor is 6.4.3, without a forced cross-major transitive override.

## Audit result

`pnpm audit --json` still exits 1 because moderate and low advisories remain:

| Audit | Low | Moderate | High | Critical | Total advisories | Total dependencies |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Original baseline | 14 | 77 | 52 | 1 | 144 | 1,423 |
| After Task 1 | 14 | 74 | 44 | 1 | 133 | 1,421 |
| After Task 2 | 8 | 30 | 0 | 0 | 38 | 1,244 |
| Task 2 change | -6 | -44 | -44 | -1 | -95 | -177 |

There are no residual critical or high findings. The 38 remaining advisory
entries comprise 30 moderate and 8 low entries. Four are reported as
development-only: `@humanfs/node` (1), `ajv` (1), `@vitest/mocker` (1), and
`vitest` (1). The other 34 are reported on production findings:
`baseline-browser-mapping` (1 moderate), `dompurify` (14 moderate / 4 low),
`mermaid` (8 moderate / 1 low), `next-intl` (2 moderate), `uuid`
(1 moderate), `@ai-sdk/provider-utils` (1 low), `@babel/core` (1 low), and
`icu-minify` (1 low). Audit finding flags are dependency-graph metadata, not a
complete runtime reachability proof.

No advisory is suppressed. The audit's remaining patched candidates include
Vitest and `@vitest/mocker` 4.1.11, plus compatible updates in the ESLint,
Monaco/DOMPurify, Mermaid, Next/next-intl, and AI SDK graphs. Those moderate/low
families require separately reviewed compatibility slices and are not part of
this critical/high remediation.

## Verification evidence

All commands used `/opt/homebrew/opt/node@22/bin` and ran serially:

- `pnpm install --ignore-scripts`: exit 0; supply-chain policy passed and no
  lifecycle scripts ran.
- Stylesheet equivalence: `cmp` exit 0; both copies are 95 lines / 1,669 bytes
  with the SHA-256 above.
- Native notice packaging check: the Dockerfile contains the explicit install
  and attribution, a staged install is byte-identical, and its mode is 0644.
- Focused migration/linkification verification: 150/150 representative tests
  passed after the final `clearMocks` configuration.
- `pnpm test`: exit 0; 469 test files and 6,421 tests passed in 142.24s. Existing
  expected stderr/warnings remained (React `act`, jsdom canvas, invalid nested
  HTML, and deliberate mocked failure logs); none was a test failure.
- `pnpm exec tsc --noEmit`: exit 0, no diagnostics.
- `pnpm build`: exit 0; Next 16.3.4 compiled, typechecked, and generated all 34
  static routes.
- `pnpm audit --json`: exit 1 with exactly 30 moderate and 8 low findings;
  zero critical and zero high.
- `git diff --check`: exit 0.

This evidence accepts the local package graph, frontend export, and notice
packaging instructions only. The native image has not been rebuilt or deployed;
image review and deployment remain separate release gates.

[shadcn-license]: https://github.com/shadcn-ui/ui/blob/main/LICENSE.md
[vitest-3]: https://v3.vitest.dev/guide/migration.html#vitest-3
[vite-6]: https://v6.vite.dev/guide/migration.html
[vitest-critical]: https://github.com/advisories/GHSA-5xrq-8626-4rwp
[vite-high]: https://github.com/advisories/GHSA-fx2h-pf6j-xcff
[brace-1]: https://github.com/advisories/GHSA-3jxr-9vmj-r5cp
[brace-2]: https://github.com/advisories/GHSA-mh99-v99m-4gvg
[brace-3]: https://github.com/advisories/GHSA-rgw5-rvv9-x895
[browserslist-1]: https://github.com/advisories/GHSA-c83g-rgw3-j3cx
[browserslist-2]: https://github.com/advisories/GHSA-73wf-gq98-2v4g
[flatted-1]: https://github.com/advisories/GHSA-25h7-pfq9-p65f
[flatted-2]: https://github.com/advisories/GHSA-rf6f-7fwh-wjgh
[form-data]: https://github.com/advisories/GHSA-hmw2-7cc7-3qxx
[glob]: https://github.com/advisories/GHSA-5j98-mcp5-4vw2
[js-yaml-1]: https://github.com/advisories/GHSA-52cp-r559-cp3m
[js-yaml-2]: https://github.com/advisories/GHSA-5p4m-2wfm-xmqj
[js-yaml-3]: https://github.com/advisories/GHSA-2883-xcg3-v3hh
[minimatch-1]: https://github.com/advisories/GHSA-3ppc-4f35-3m26
[minimatch-2]: https://github.com/advisories/GHSA-7r86-cg39-jmmj
[minimatch-3]: https://github.com/advisories/GHSA-23c5-xmqv-rm74
[picomatch]: https://github.com/advisories/GHSA-c2c7-rcm5-vvqj
[ws]: https://github.com/advisories/GHSA-96hv-2xvq-fx4p
