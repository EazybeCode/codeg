#!/usr/bin/env node
// Offline HTML verification using the browser installed in the runtime image.
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL, fileURLToPath } = require('node:url');

function inside(root, target) {
  const rel = path.relative(root, target);
  return rel !== '..' && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel);
}

function inputFile(root, value) {
  if (!value || !/\.html?$/i.test(value)) throw new Error('Provide a local HTML file.');
  const target = fs.realpathSync(path.resolve(root, value));
  if (!inside(root, target) || !fs.statSync(target).isFile()) {
    throw new Error('HTML must be inside this run’s working directory.');
  }
  return target;
}

function localRequest(url) {
  return url.startsWith('data:') || url === 'about:blank';
}

async function main() {
  const root = fs.realpathSync(process.cwd());
  const source = inputFile(root, process.argv[2]);
  const output = path.resolve(root, process.argv[3] || '.neev-preview/page.png');
  if (!inside(root, output) || !output.endsWith('.png')) throw new Error('Screenshot must be a PNG inside this run.');
  fs.mkdirSync(path.dirname(output), { recursive: true });
  if (!inside(root, fs.realpathSync(path.dirname(output))) || fs.existsSync(output)) {
    throw new Error('Use a new screenshot path inside this run.');
  }
  process.env.PLAYWRIGHT_BROWSERS_PATH = '/opt/neev/browser-tools/browsers';
  const { chromium } = require('/opt/neev/browser-tools/node_modules/playwright');
  const browser = await chromium.launch({ args: ['--disable-dev-shm-usage', '--renderer-process-limit=1'] });
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' });
    let blocked = 0;
    await context.route('**/*', async route => {
      const url = route.request().url();
      let allowed = localRequest(url);
      if (url.startsWith('file:')) {
        try { allowed = inside(root, fs.realpathSync(fileURLToPath(url))); } catch { allowed = false; }
      }
      if (allowed) await route.continue();
      else { blocked++; await route.abort(); }
    });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', e => { if (errors.length < 20) errors.push(e.message.slice(0, 300)); });
    await page.goto(pathToFileURL(source).href, { waitUntil: 'load', timeout: 30000 });
    await page.screenshot({ path: output, fullPage: false, timeout: 15000 });
    console.log(JSON.stringify({ title: await page.title(), screenshot: path.relative(root, output), javascriptErrors: errors, blockedNetworkRequests: blocked, scope: 'Local HTML only; external resources were blocked.' }));
    if (errors.length) process.exitCode = 1;
  } finally { await browser.close(); }
}

module.exports = { inputFile, localRequest };
if (require.main === module) main().catch(error => { console.error(error.message); process.exitCode = 1; });
