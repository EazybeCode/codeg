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

### Compatible residual browser and tooling remediation

The remaining compatible browser/rendering and lint-chain patches were applied
without changing the protected Next, BlockNote, Tiptap, Vitest or Vite versions.
Direct parents are pinned at the first reviewed compatible release when their
children were exact dependencies: `ai` 6.0.214 is the first 6.0.x release whose
direct and gateway paths both use `@ai-sdk/provider-utils` 4.0.33, and
`next-intl` 4.9.2 keeps its documented Next 16 / React 19 peer support. Its
declared compatible child ranges resolve `icu-minify` and `use-intl` 4.14.4.

The other paths use package-major or exact-parent floors. Each accepted
resolution is admitted by its parent's declared range except Monaco's
DOMPurify child: `monaco-editor` 0.55.1 exact-pins 3.2.7, and the workspace
intentionally overrides that exact pin with the same-major patched 3.4.13
security floor.

| Package | Task 2 resolution | Accepted resolution | Parent chain and official evidence |
| --- | --- | --- | --- |
| `dompurify` | 3.2.7 / 3.3.3 | 3.4.13 | The workspace intentionally replaces `monaco-editor`'s exact 3.2.7 pin; `@streamdown/mermaid > mermaid` admits 3.4.13 through `^3.3.3`. The governing [detached-subtree XSS advisory][dompurify-final] patches at 3.4.13 and subsumes the earlier Task 2 DOMPurify findings. |
| `mermaid` | 11.13.0 | 11.16.1 | `@streamdown/mermaid` declares `^11.12.2`; 11.16.1 clears the [configuration prototype-pollution][mermaid-prototype] and [sibling CSS injection][mermaid-css] floors plus the earlier Mermaid findings. |
| `uuid` | 11.1.0 | 11.1.1 | `mermaid` declares `^11.1.0` for the 11.x line; [buffer-bounds advisory][uuid-bounds]. |
| `@babel/core` | 7.29.0 | 7.29.6 | Vite React, Next/styled-jsx and ESLint React hooks all admit Babel 7.x. The original audit named 7.29.1, but that version is not published; current official [advisory metadata][babel-read] names 7.29.6 as the first patched 7.x release. |
| `@humanfs/node` / `ajv` | 0.16.7 / 6.12.6 | 0.16.8 / 6.14.0 | Development-only ESLint 9.39.2 ranges admit both patches; [humanfs symlink-copy advisory][humanfs-copy] and [Ajv ReDoS advisory][ajv-redos]. |
| `baseline-browser-mapping` | 2.10.38 on Next; 2.11.22 on Browserslist | 2.11.0 | An exact `next@16.3.4` child floor stays inside Next's `^2.9.19` range; [invalid-input DoS advisory][baseline-dos]. |
| `@ai-sdk/provider-utils` | 4.0.14 | 4.0.33 | `ai` 6.0.214 and `@ai-sdk/gateway` 3.0.139 both declare the patched version exactly; [resource-consumption advisory][ai-provider-dos]. |
| `next-intl` / `icu-minify` | 4.8.3 / 4.8.3 | 4.9.2 / 4.14.4 | Direct compatible parent update; [next-intl precompile prototype pollution][next-intl-dos], [next-intl open redirect][next-intl-cache], and [icu-minify prototype lookup][icu-minify-dos]. |

The real Streamdown Mermaid renderer is now exercised with strict security
configuration: legitimate `<b>` label markup survives, while a `javascript:`
click target does not appear in the generated SVG. This is a behavioral
security-boundary regression, not a claim that the test reproduces the
specific CSS/prototype-pollution advisories. Existing diagram component tests
and native rich-document parser, adapter, editor and document tests remain
unchanged and pass with the upgraded graph.

## Audit result

`pnpm audit --json` still exits 1 because moderate advisories remain:

| Audit | Low | Moderate | High | Critical | Total advisories | Total dependencies |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Original baseline | 14 | 77 | 52 | 1 | 144 | 1,423 |
| After Task 1 | 14 | 74 | 44 | 1 | 133 | 1,421 |
| After Task 2 | 8 | 30 | 0 | 0 | 38 | 1,244 |
| Task 2 change | -6 | -44 | -44 | -1 | -95 | -177 |
| After Task 3 | 0 | 2 | 0 | 0 | 2 | 1,232 |
| Task 3 change | -8 | -28 | 0 | 0 | -36 | -12 |

There are no residual low, high or critical findings. The two remaining
moderate entries are the same [Vitest redirect-mock advisory][vitest-mocker]
reported on Vitest 3.2.7 and `@vitest/mocker` 3.2.7. Their exact paths are the
direct `vitest` development dependency and the matching `@vitest/ui` and
`@vitest/coverage-v8` development paths. They are test-tooling only and are not
part of the production static application graph; that classification limits
runtime reachability but does not suppress or invalidate the registry finding.
The patched floor is Vitest / mocker 4.1.11. That cross-major migration remains
deferred because Task 2 observed an assertion-level compatibility change and
this slice explicitly retains the accepted 3.2.7 toolchain.

## Verification evidence

All commands used `/opt/homebrew/opt/node@22/bin` and ran serially:

- `pnpm install --ignore-scripts`: exit 0; supply-chain policy passed and no
  lifecycle scripts ran.
- Stylesheet equivalence: `cmp` exit 0; both copies are 95 lines / 1,669 bytes
  with the SHA-256 above.
- Native notice packaging check: the Dockerfile contains the explicit install
  and attribution, a staged install is byte-identical, and its mode is 0644.
- Task 3 focused diagram/native-rich verification: exit 0; 7 files and 125/125
  tests passed, including the real Mermaid safe-markup/executable-link boundary.
- `pnpm test`: exit 0; 469 test files and 6,422 tests passed in 133.70s. Existing
  expected stderr/warnings remained (React `act`, jsdom canvas, invalid nested
  HTML, and deliberate mocked failure logs); none was a test failure.
- `pnpm exec tsc --noEmit`: exit 0, no diagnostics.
- `pnpm build`: exit 0; Next 16.3.4 compiled in 16.5s, typechecked, and generated
  all 34 static routes.
- `pnpm audit --json`: exit 1 with exactly 2 moderate development-only findings;
  zero low, critical and high.
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
[dompurify-final]: https://github.com/advisories/GHSA-55q2-fjhq-7xh7
[mermaid-prototype]: https://github.com/advisories/GHSA-c4c3-pg64-4m4v
[mermaid-css]: https://github.com/advisories/GHSA-6x64-9x62-f2gx
[uuid-bounds]: https://github.com/advisories/GHSA-w5hq-g745-h8pq
[babel-read]: https://github.com/advisories/GHSA-4x5r-pxfx-6jf8
[humanfs-copy]: https://github.com/advisories/GHSA-p498-v437-472g
[ajv-redos]: https://github.com/advisories/GHSA-2g4f-4pwh-qvx6
[baseline-dos]: https://github.com/advisories/GHSA-w5vr-8v7q-w6rv
[ai-provider-dos]: https://github.com/advisories/GHSA-866g-f22w-33x8
[next-intl-dos]: https://github.com/advisories/GHSA-4c35-wcg5-mm9h
[next-intl-cache]: https://github.com/advisories/GHSA-8f24-v5vv-gm5j
[icu-minify-dos]: https://github.com/advisories/GHSA-r27j-894h-3w3p
[vitest-mocker]: https://github.com/advisories/GHSA-82fw-gwwq-j7x9
