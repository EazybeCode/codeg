const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { inputFile, localRequest } = require('../operations/neev_preview.cjs');

test('preview input must be a real HTML file inside the working directory', () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'neev-preview-')));
  try {
    fs.writeFileSync(path.join(root, 'page.html'), '<h1>Hello</h1>');
    assert.equal(inputFile(root, 'page.html'), path.join(root, 'page.html'));
    fs.symlinkSync('/etc/passwd', path.join(root, 'escape.html'));
    assert.throws(() => inputFile(root, 'escape.html'));
    assert.throws(() => inputFile(root, '/etc/passwd'));
    assert.throws(() => inputFile(root, 'https://example.com'));
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('preview never enables network requests', () => {
  assert.equal(localRequest('data:image/png;base64,AA'), true);
  assert.equal(localRequest('about:blank'), true);
  assert.equal(localRequest('https://example.com'), false);
  assert.equal(localRequest('http://127.0.0.1:3100'), false);
  assert.equal(localRequest('ws://localhost'), false);
});
