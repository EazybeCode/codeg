const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const dockerfile = fs.readFileSync(path.resolve(__dirname, '../../Dockerfile'), 'utf8');
const stages = dockerfile.split(/(?=^FROM\s)/m).filter(s => /^FROM\s/.test(s));
const stage = name => {
  const matches = stages.filter(s => new RegExp(`^FROM[^\\n]+\\sAS\\s${name}\\s*$`, 'mi').test(s.split('\n')[0]));
  assert.equal(matches.length, 1, `one ${name} stage`);
  return matches[0];
};
test('team image selects the fail-closed server and graceful shutdown', () => {
  stage('runtime');
  const body = stage('team');
  assert.match(body, /^FROM runtime AS team$/m);
  assert.match(body, /^ENV WORKOS_SERVER_MODE=team$/m);
  assert.match(body, /^ENV WORKOS_ENABLE_CATALOG_RUNTIME=false$/m);
  assert.match(body, /^STOPSIGNAL SIGINT$/m);
  assert.match(body, /^CMD \["codeg-server", "--team"\]$/m);
  assert.doesNotMatch(body, /--supervise/);
  assert.equal(body.split('\n').filter(l => /^CMD\s/.test(l)).length, 1);
});
test('default image preserves independent operator behavior', () => {
  const body = stage('operator');
  assert.equal(stages.at(-1), body);
  assert.match(body, /^FROM runtime AS operator$/m);
  assert.match(body, /^CMD \["codeg-server", "--supervise"\]$/m);
  assert.doesNotMatch(body, /WORKOS_SERVER_MODE|WORKOS_ENABLE_CATALOG_RUNTIME|STOPSIGNAL/);
  assert.doesNotMatch(stage('runtime'), /WORKOS_SERVER_MODE|WORKOS_ENABLE_CATALOG_RUNTIME|STOPSIGNAL/);
});
