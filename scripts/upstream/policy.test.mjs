import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  NONCORE_ALLOWLIST, allowlistDigest, assertPatternShape, checkBlob, classifyChange, classifyChanges,
  invalidPathReason, matchesAllowlist, protectedReason, scanAddedLines, validateManifest,
} from './policy.mjs';

const add = (path) => ({ status: 'A', path, oldMode: '000000', newMode: '100644' });
const mod = (path) => ({ status: 'M', path, oldMode: '100644', newMode: '100644' });

test('allowlist admits only exact presentation paths', () => {
  assert.equal(classifyChange(add('src/components/ui/drawer-occlusion.test.tsx')), null);
  for (const path of ['src/components/ui/button.tsx',
    'docs/readme/README.ja.md', 'docs/chat-channels/telegram-topic-mode.md', 'docs/url-scheme.md',
    'docs/images/screen.png']) {
    assert.equal(classifyChange(mod(path)), null, path);
  }
});

test('protected core, config, dependency and security paths are never eligible', () => {
  const cases = [
    'src/components/neev/native-chat.tsx', 'src/app/globals.css', 'src/app/layout.tsx', 'src/lib/api.ts',
    'src-tauri/src/neev/agents/runs.rs', 'src-tauri/src/db/migration/mod.rs', 'package.json',
    'pnpm-lock.yaml', 'Cargo.lock', 'Dockerfile', '.github/workflows/release.yml', 'scripts/upstream/policy.mjs',
    'deploy/coolify/app.yml', 'docs/operations/backups.md', 'AGENTS.md', 'CLAUDE.md', 'README.md',
    'docs/CLIENT-PRIVACY.md', 'src/components/ui/auth-dialog.tsx', 'src/components/ui/session-list.tsx',
    'src/components/ui/button.ts', 'src/components/ui/types.d.ts', 'src/components/ui/icon.svg',
    'src/components/ui/theme.css', 'docs/images/logo.svg', 'src/i18n/messages/en.json',
  ];
  for (const path of cases) assert.notEqual(classifyChange(mod(path)), null, path);
  assert.equal(protectedReason('src/components/neev/x.tsx'), 'protected_path');
  assert.equal(protectedReason('SRC/APP/layout.tsx'), 'protected_path', 'case aliases are folded');
});

test('glob wildcards never traverse directories, dotfiles or unusual characters', () => {
  for (const path of [
    'src/components/ui/nested/button.tsx', 'src/components/ui/../lib/api.tsx', 'src/components/ui/./button.tsx',
    'src/components/ui//button.tsx', '/src/components/ui/button.tsx', 'src/components/ui/button.tsx/',
    'src\\components\\ui\\button.tsx', 'src/components/ui/.hidden.tsx', 'src/components/UI/button.tsx',
    'src/components/ui/but ton.tsx', 'src/components/ui/button‮.tsx', 'src/components/ui/b\nx.tsx',
    'docs/readme/README.md', 'docs/readme/sub/README.ja.md', 'src/components/ui/.tsx',
  ]) {
    assert.equal(matchesAllowlist(path), false, JSON.stringify(path));
    assert.notEqual(classifyChange(mod(path)), null, JSON.stringify(path));
  }
  assert.equal(invalidPathReason('a/../b'), 'path_traversal');
});

test('symlinks, submodules, executables, type changes and test edits are rejected', () => {
  const p = 'src/components/ui/button.tsx';
  assert.equal(classifyChange({ status: 'A', path: p, oldMode: '000000', newMode: '120000' }), 'unsupported_file_mode');
  assert.equal(classifyChange({ status: 'A', path: p, oldMode: '000000', newMode: '160000' }), 'unsupported_file_mode');
  assert.equal(classifyChange({ status: 'M', path: p, oldMode: '100644', newMode: '100755' }), 'unsupported_file_mode');
  assert.equal(classifyChange({ status: 'T', path: p, oldMode: '100644', newMode: '120000' }), 'unsupported_change_type');
  assert.equal(classifyChange({ status: 'R', path: p, oldMode: '100644', newMode: '100644' }), 'unsupported_change_type');
  assert.equal(classifyChange(mod('src/components/ui/button.test.tsx')), 'test_modification');
  assert.equal(classifyChange({ status: 'D', path: 'src/components/ui/button.test.tsx', oldMode: '100644', newMode: '000000' }),
    'test_modification');
  assert.equal(classifyChange(add('src/components/ui/button.test.tsx')), null);
});

test('a commit is eligible only if every change is eligible', () => {
  assert.equal(classifyChanges([mod('src/components/ui/button.tsx'), mod('docs/url-scheme.md')]).eligible, true);
  const mixed = classifyChanges([mod('src/components/ui/button.tsx'), mod('src/lib/utils.ts')]);
  assert.equal(mixed.eligible, false);
  assert.deepEqual(mixed.rejections.map((r) => r.path), ['src/lib/utils.ts']);
  assert.equal(classifyChanges([]).eligible, false, 'empty change sets are not eligible');
});

test('added code is scanned for network, storage, bridge and import escapes', () => {
  const p = 'src/components/ui/button.tsx';
  assert.deepEqual(scanAddedLines(p, ['import { cn } from "@/lib/utils"', '  className={cn("px-2", className)}']), []);
  const bad = {
    'fetch("/api/secret")': 'network_call', 'import { api } from "@/lib/api"': 'import_not_allowlisted',
    'const m = await import("./x")': 'dynamic_import', 'window["fe" + "tch"]': 'obfuscation',
    'localStorage.setItem("k", v)': 'storage_access', 'dangerouslySetInnerHTML={{ __html: x }}': 'raw_html',
    'const u = "https://evil.example"': 'external_url', 'invoke("run_agent")': 'runtime_bridge',
    'export * from "../../lib/api"': 'import_not_allowlisted', 'import "./polyfill"': null,
  };
  for (const [line, reason] of Object.entries(bad)) {
    const reasons = scanAddedLines(p, [line]);
    if (reason) assert.ok(reasons.includes(reason), `${line} -> ${reasons}`);
    else assert.deepEqual(reasons, []);
  }
  assert.deepEqual(scanAddedLines('src/components/ui/x.test.tsx', ['import { it } from "vitest"']), []);
  assert.deepEqual(scanAddedLines(p, ['import { it } from "vitest"']), ['import_not_allowlisted']);
  assert.deepEqual(scanAddedLines('docs/url-scheme.md', ['See [docs](https://example.com)']), []);
  assert.ok(scanAddedLines('docs/url-scheme.md', ['<script>alert(1)</script>']).includes('raw_html'));
});

test('blobs are size-bounded and images must carry real signatures', () => {
  const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0]);
  assert.equal(checkBlob('docs/images/a.png', png), null);
  assert.equal(checkBlob('docs/images/a.png', Buffer.from('<svg onload=alert(1)>')), 'image_signature_mismatch');
  assert.equal(checkBlob('src/components/ui/a.tsx', Buffer.from('a\0b')), 'binary_text');
  assert.equal(checkBlob('docs/url-scheme.md', Buffer.alloc(3 * 1024 * 1024, 97)), 'blob_too_large');
});

test('allowlist cannot be widened by code, manifest or config drift', () => {
  for (const wide of ['src/**', 'src/components/ui/**', 'src/components/*/button.tsx', 'src/lib/*.tsx',
    'src/components/ui/*', 'src/components/ui/*.*', 'src/components/ui/*.ts', 'src/components/neev/*.tsx',
    'docs/operations/*.md', 'docs/images/*.svg', '.github/*.md', 'src/components/ui/../lib/*.tsx']) {
    assert.throws(() => assertPatternShape(wide), undefined, wide);
  }
  const manifest = JSON.parse(readFileSync(new URL('../../.github/workos-upstream.json', import.meta.url), 'utf8'));
  assert.equal(validateManifest(manifest), manifest, 'checked-in manifest matches the trusted policy');
  const widened = structuredClone(manifest);
  widened.noncorePolicy.allowlist.push('src/lib/*.tsx');
  assert.throws(() => validateManifest(widened), /refusing to widen/);
  const redigested = structuredClone(widened);
  redigested.noncorePolicy.allowlistSha256 = allowlistDigest(redigested.noncorePolicy.allowlist);
  assert.throws(() => validateManifest(redigested), /refusing to widen/);
  assert.throws(() => validateManifest({ ...manifest, mode: 'discovery-only' }));
  assert.equal(Object.isFrozen(NONCORE_ALLOWLIST), true);
});
