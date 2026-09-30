// Trusted NON-CORE eligibility policy for unattended upstream updates.
//
// Everything here fails closed: a path, file mode, change type or added line that
// is not positively recognised as presentation-only makes the whole upstream
// commit ineligible. The allowlist is a reviewed code constant. The repository
// manifest must repeat it exactly and the server config must pin its digest, so
// neither a manifest edit nor a code edit can widen what ships unattended.
import { createHash } from 'node:crypto';

export const POLICY_VERSION = 1;

export const NONCORE_ALLOWLIST = Object.freeze([
  'src/components/ui/*.tsx',
  'docs/readme/README.*.md',
  'docs/chat-channels/*.md',
  'docs/url-scheme.md',
  'docs/images/*.png',
  'docs/images/*.jpg',
  'docs/images/*.gif',
]);

// The only directories and extensions an allowlist entry may ever name. A
// pattern outside these fails at import time, before any repository is read.
const PERMITTED_DIRECTORIES = new Set([
  'src/components/ui', 'docs', 'docs/readme', 'docs/chat-channels', 'docs/images',
]);
const PERMITTED_EXTENSIONS = new Set(['.tsx', '.md', '.png', '.jpg', '.gif']);

// Defense in depth: denied even if an allowlist entry would match.
const PROTECTED_PREFIXES = [
  'src-tauri/', 'src/lib/', 'src/app/', 'src/i18n/', 'src/hooks/', 'src/stores/',
  'src/components/neev/', 'scripts/', 'deploy/', '.github/', 'public/',
  'browser-agent/', 'docs/operations/', 'docs/superpowers/', 'docs/decisions/',
  'docs/releasing/', 'docs/features/',
];
const PROTECTED_BASENAMES = new Set([
  'package.json', 'package-lock.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml', 'yarn.lock',
  'cargo.toml', 'cargo.lock', 'docker-compose.yml', 'makefile', 'agents.md', 'claude.md',
  'gemini.md', 'skill.md', 'globals.css', 'next.config.ts', 'tsconfig.json',
  'vitest.config.ts', 'eslint.config.mjs', 'components.json', 'client-privacy.md',
  'license', 'readme.md', 'install.sh', 'install.ps1',
]);
const PROTECTED_EXTENSIONS = new Set([
  '.css', '.scss', '.js', '.mjs', '.cjs', '.ts', '.json', '.yml', '.yaml', '.toml', '.lock',
  '.sh', '.ps1', '.py', '.rs', '.sql', '.svg', '.html', '.htm', '.wasm', '.node', '.env',
  '.plist', '.service', '.conf',
]);
const PROTECTED_TOKENS = new Set([
  'neev', 'auth', 'oauth', 'login', 'logout', 'signin', 'signup', 'session', 'sessions',
  'secret', 'secrets', 'credential', 'credentials', 'token', 'tokens', 'password', 'passwords',
  'permission', 'permissions', 'security', 'sandbox', 'migration', 'migrations', 'schema',
  'schemas', 'database', 'db', 'sql', 'deploy', 'deployment', 'workflow', 'workflows',
  'install', 'script', 'scripts', 'hook', 'hooks', 'config', 'env', 'billing', 'admin',
  'tenant', 'tenancy', 'globals', 'apparmor', 'seccomp', 'key', 'keys', 'upstream',
]);
const TEST_FILE = /\.(test|spec)\.[a-z]+$/;

const SAFE_PATH = /^[A-Za-z0-9._\-/]+$/;
const SEGMENT_WILDCARD = '[A-Za-z0-9_-][A-Za-z0-9._-]*';
export const MAX_BLOB_BYTES = 2 * 1024 * 1024;

function extensionOf(name) {
  const dot = name.lastIndexOf('.');
  return dot <= 0 ? '' : name.slice(dot).toLowerCase();
}

export function assertPatternShape(pattern) {
  if (typeof pattern !== 'string' || !SAFE_PATH.test(pattern.replaceAll('*', 'x'))) {
    throw new Error(`Allowlist pattern has unsupported characters: ${JSON.stringify(pattern)}`);
  }
  const segments = pattern.split('/');
  const last = segments.pop();
  const dir = segments.join('/');
  if (segments.some((s) => s.includes('*') || s === '' || s.startsWith('.'))) {
    throw new Error(`Allowlist directories must be literal: ${pattern}`);
  }
  if (!PERMITTED_DIRECTORIES.has(dir)) throw new Error(`Allowlist directory not permitted: ${pattern}`);
  if ((last.match(/\*/g) ?? []).length > 1 || last.startsWith('.')) {
    throw new Error(`Allowlist file pattern too broad: ${pattern}`);
  }
  if (!PERMITTED_EXTENSIONS.has(extensionOf(last))) {
    throw new Error(`Allowlist extension not permitted: ${pattern}`);
  }
  return true;
}

export function allowlistDigest(allowlist = NONCORE_ALLOWLIST) {
  return createHash('sha256').update(JSON.stringify(allowlist)).digest('hex');
}

for (const pattern of NONCORE_ALLOWLIST) assertPatternShape(pattern);

function compile(pattern) {
  const escaped = pattern.split('*').map((part) => part.replace(/[.+?^${}()|[\]\\]/g, '\\$&'));
  return new RegExp(`^${escaped.join(SEGMENT_WILDCARD)}$`);
}
const COMPILED = new Map(NONCORE_ALLOWLIST.map((p) => [p, compile(p)]));

/** Returns a reason when a repository path is malformed or ambiguous, else null. */
export function invalidPathReason(path) {
  if (typeof path !== 'string' || path.length === 0 || path.length > 512) return 'invalid_path';
  if (!SAFE_PATH.test(path)) return 'invalid_path_characters';
  if (path.startsWith('/') || path.endsWith('/')) return 'invalid_path';
  for (const segment of path.split('/')) {
    if (segment === '' || segment === '.' || segment === '..') return 'path_traversal';
  }
  return null;
}

export function matchesAllowlist(path) {
  if (invalidPathReason(path)) return false;
  for (const [pattern, regex] of COMPILED) {
    // `*` never crosses `/`: patterns and paths must have the same depth.
    if (pattern.split('/').length === path.split('/').length && regex.test(path)) return true;
  }
  return false;
}

/** Returns a protected-path reason, or null. Case-folded so case aliases never pass. */
export function protectedReason(path) {
  const lower = path.toLowerCase();
  if (PROTECTED_PREFIXES.some((p) => lower.startsWith(p))) return 'protected_path';
  const segments = lower.split('/');
  const base = segments[segments.length - 1];
  if (segments.some((s) => s.startsWith('.'))) return 'protected_hidden_path';
  if (PROTECTED_BASENAMES.has(base) || base.startsWith('dockerfile')) return 'protected_file';
  if (PROTECTED_EXTENSIONS.has(extensionOf(base))) return 'protected_extension';
  for (const segment of segments) {
    for (const token of segment.split(/[-_.]+/)) {
      if (PROTECTED_TOKENS.has(token)) return 'protected_name';
    }
  }
  return null;
}

/**
 * Classify one raw change: { status, path, oldMode, newMode }.
 * Only plain files (100644) that are added, modified or deleted qualify.
 */
export function classifyChange(change) {
  const { status, path, oldMode, newMode } = change;
  const invalid = invalidPathReason(path);
  if (invalid) return invalid;
  const protectedHit = protectedReason(path);
  if (protectedHit) return protectedHit;
  if (!matchesAllowlist(path)) return 'not_allowlisted';
  const modes = { A: ['000000', '100644'], M: ['100644', '100644'], D: ['100644', '000000'] }[status];
  if (!modes) return 'unsupported_change_type';
  if (oldMode !== modes[0] || newMode !== modes[1]) return 'unsupported_file_mode';
  // Upstream may add new tests but never modify or delete existing ones.
  if (TEST_FILE.test(path) && status !== 'A') return 'test_modification';
  return null;
}

export function classifyChanges(changes) {
  const rejections = [];
  for (const change of changes) {
    const reason = classifyChange(change);
    if (reason) rejections.push({ path: change.path, reason });
  }
  return { eligible: changes.length > 0 && rejections.length === 0, rejections };
}

// Module specifiers a presentation primitive may newly import.
const IMPORT_ALLOWLIST = [
  /^react$/, /^react-dom$/, /^react\/jsx-runtime$/, /^radix-ui$/, /^@radix-ui\/react-[a-z-]+$/,
  /^@base-ui\/react(\/[a-z-]+)?$/, /^lucide-react$/, /^class-variance-authority$/, /^cmdk$/,
  /^react-day-picker$/, /^react-resizable-panels$/, /^overlayscrollbars-react$/, /^sonner$/,
  /^@\/lib\/utils$/, /^@\/components\/ui\/[a-z0-9-]+$/, /^\.\/[a-z0-9-]+$/,
];
const TEST_IMPORT_ALLOWLIST = [
  /^vitest$/, /^@testing-library\/react$/, /^@testing-library\/user-event$/,
];
const FORBIDDEN_CODE = [
  [/\bfetch\s*\(/, 'network_call'], [/XMLHttpRequest|WebSocket|EventSource|sendBeacon/, 'network_call'],
  [/https?:\/\//i, 'external_url'], [/\beval\s*\(|new\s+Function\b|\bFunction\s*\(/, 'dynamic_code'],
  [/\bimport\s*\(/, 'dynamic_import'], [/\brequire\s*\(/, 'dynamic_import'],
  [/dangerouslySetInnerHTML|innerHTML|outerHTML|insertAdjacentHTML|srcdoc|document\.write/, 'raw_html'],
  [/document\.cookie|localStorage|sessionStorage|indexedDB|caches\./, 'storage_access'],
  [/navigator\.(clipboard\.read|credentials|serviceWorker)|serviceWorker|importScripts/, 'privileged_api'],
  [/window\.open|location\.(href|assign|replace)|postMessage/, 'navigation'],
  [/process\.env|__TAURI|@tauri-apps|\binvoke\s*\(/, 'runtime_bridge'],
  [/globalThis|\bwindow\s*\[|\bself\s*\[|\bReflect\.|String\.fromCharCode|\batob\s*\(|\\u\{?[0-9a-f]/i, 'obfuscation'],
  [/javascript:|data:text\/html/i, 'script_url'],
];
const FORBIDDEN_MARKDOWN = [
  [/<\s*(script|iframe|object|embed|form|meta|link|style)\b/i, 'raw_html'],
  [/javascript:|data:text\/html|vbscript:/i, 'script_url'],
  [/\son[a-z]+\s*=/i, 'event_handler'],
];
const IMAGE_MAGIC = {
  '.png': (b) => b.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])),
  '.jpg': (b) => b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff,
  '.gif': (b) => b.subarray(0, 4).toString('latin1') === 'GIF8',
};

/** Scan lines an upstream change adds. Returns a list of reasons (empty = clean). */
export function scanAddedLines(path, lines) {
  const reasons = new Set();
  const ext = extensionOf(path);
  if (ext === '.tsx') {
    const allowed = TEST_FILE.test(path) ? [...IMPORT_ALLOWLIST, ...TEST_IMPORT_ALLOWLIST] : IMPORT_ALLOWLIST;
    for (const line of lines) {
      if (line.includes('\0')) reasons.add('binary_text');
      for (const [regex, reason] of FORBIDDEN_CODE) if (regex.test(line)) reasons.add(reason);
      const specifiers = [...line.matchAll(/\bfrom\s*['"`]([^'"`]*)['"`]|^\s*import\s*['"`]([^'"`]*)['"`]/g)]
        .map((m) => m[1] ?? m[2]);
      for (const specifier of specifiers) {
        if (!allowed.some((regex) => regex.test(specifier))) reasons.add('import_not_allowlisted');
      }
      if (/\bfrom\s*$|^\s*import\s*$/.test(line)) reasons.add('import_not_allowlisted');
    }
  } else if (ext === '.md') {
    for (const line of lines) {
      if (line.includes('\0')) reasons.add('binary_text');
      for (const [regex, reason] of FORBIDDEN_MARKDOWN) if (regex.test(line)) reasons.add(reason);
    }
  } else if (!(ext in IMAGE_MAGIC)) {
    reasons.add('unscannable_type');
  }
  return [...reasons];
}

/** Validate a blob's size and, for images, its content signature. */
export function checkBlob(path, buffer) {
  if (buffer.length > MAX_BLOB_BYTES) return 'blob_too_large';
  const ext = extensionOf(path);
  if (ext in IMAGE_MAGIC) return IMAGE_MAGIC[ext](buffer) ? null : 'image_signature_mismatch';
  if (buffer.includes(0)) return 'binary_text';
  return null;
}

/** Validate `.github/workos-upstream.json`. Throws on any deviation. */
export function validateManifest(manifest) {
  const fail = (message) => { throw new Error(`Upstream manifest rejected: ${message}`); };
  if (!manifest || typeof manifest !== 'object') fail('not an object');
  if (manifest.repository !== 'xintaofei/codeg' || manifest.branch !== 'main') fail('unexpected upstream');
  if (!/^[a-f0-9]{40}$/.test(manifest.incorporated ?? '')) fail('incorporated must be a full SHA');
  if (manifest.mode !== 'noncore-auto') fail('mode must be noncore-auto');
  const p = manifest.noncorePolicy;
  if (!p || p.version !== POLICY_VERSION) fail('unsupported policy version');
  if (JSON.stringify(p.allowlist) !== JSON.stringify(NONCORE_ALLOWLIST)) {
    fail('allowlist differs from the trusted policy; refusing to widen or alter it');
  }
  if (p.allowlistSha256 !== allowlistDigest()) fail('allowlist digest mismatch');
  return manifest;
}
