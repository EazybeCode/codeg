import assert from "node:assert/strict"
import test from "node:test"
import fs from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { backup } from "./neev-content-backup.mjs"

const key = "10000000-0000-4000-8000-000000000001"
const hash = (s) => createHash("sha256").update(s).digest("hex")
async function fixture(t) {
  const root = await fs.mkdtemp(
    path.join(await fs.realpath(os.tmpdir()), "neev-backup-test-")
  )
  t.after(() => fs.rm(root, { recursive: true, force: true }))
  const source = path.join(root, "source")
  await fs.mkdir(source, { mode: 0o700 })
  await fs.mkdir(path.join(source, "7"), { mode: 0o700 })
  await fs.writeFile(path.join(source, "7", `${key}.md`), "body", {
    mode: 0o600,
  })
  await fs.writeFile(path.join(source, "7", "pending.tmp"), "unfinished")
  // A real controlled pg_dump child records the boundary contract without network.
  const executable = path.join(root, "pg_dump")
  await fs.writeFile(
    executable,
    `#!${process.execPath}\nimport fs from 'node:fs';\nconst args=process.argv.slice(2);\nconst output=args[args.indexOf('--file')+1];\nfs.writeFileSync(${JSON.stringify(path.join(root, "record"))},JSON.stringify({args,env:process.env}));\nif(fs.existsSync(output.replace('/metadata.dump','/blobs'))) process.exit(3);\nfs.writeFileSync(output, 'dump');\n`,
    { mode: 0o700 }
  )
  return {
    root,
    source,
    output: path.join(root, "backup"),
    executable,
    record: path.join(root, "record"),
  }
}
test("dump precedes finalized copy and complete manifest without exposing credentials", async (t) => {
  const f = await fixture(t)
  const events = []
  const result = await backup({
    source: f.source,
    output: f.output,
    schema: "neev_test_123",
    databaseUrl: "postgresql://user:password@localhost/private",
    pgDump: f.executable,
    onOperation: (operation) => events.push(operation),
  })
  assert.deepEqual(events, ["pg_dump", "copy-finalized", "manifest"])
  assert.equal(result.stdout.includes("postgresql://"), false)
  assert.equal(result.exitCode, 0)
  const recorded = JSON.parse(await fs.readFile(f.record))
  assert.deepEqual(recorded.args, [
    "--format=custom",
    "--no-owner",
    "--no-privileges",
    "--strict-names",
    "--schema",
    "neev_test_123",
    "--file",
    path.join(f.output, "metadata.dump"),
  ])
  assert.equal(recorded.env.PGDATABASE, "private")
  assert.equal(recorded.env.PGUSER, "user")
  assert.equal(recorded.env.PGPASSWORD, "password")
  assert.equal(recorded.args.join(" ").includes("password"), false)
  assert.equal(recorded.env.PROVIDER_SECRET, undefined)
  const manifest = JSON.parse(
    await fs.readFile(path.join(f.output, "complete.json"))
  )
  assert.equal(manifest.dump.sha256, hash("dump"))
  assert.deepEqual(manifest.blobs, [
    { org_id: 7, key, sha256: hash("body"), byte_length: 4 },
  ])
  assert.deepEqual(await fs.readdir(path.join(f.output, "blobs", "7")), [
    `${key}.md`,
  ])
  assert.equal((await fs.stat(f.output)).mode & 0o777, 0o700)
  assert.equal(
    (await fs.stat(path.join(f.output, "metadata.dump"))).mode & 0o777,
    0o600
  )
  await assert.rejects(
    backup({
      source: f.source,
      output: f.output,
      databaseUrl: "postgresql:///private",
    })
  )
})
test("unsafe schema selectors and relative destinations are refused before creation", async (t) => {
  const f = await fixture(t)
  for (const schema of [
    "*",
    "public*",
    "--schema=public",
    "public,other",
    "public.other",
    "",
    "UPPER",
  ]) {
    await assert.rejects(
      backup({
        source: f.source,
        output: f.output,
        schema,
        databaseUrl: "postgresql:///private",
      })
    )
  }
  await assert.rejects(
    backup({
      source: f.source,
      output: "relative",
      databaseUrl: "postgresql:///private",
    })
  )
  await assert.rejects(fs.stat(f.output))
})
test("failed dump stays incomplete and symlink final files cannot be copied", async (t) => {
  const f = await fixture(t)
  await assert.rejects(
    backup({
      source: f.source,
      output: f.output,
      databaseUrl: "postgresql:///private",
      pgDump: "/usr/bin/false",
    })
  )
  await assert.rejects(fs.stat(path.join(f.output, "complete.json")))
  await fs.symlink(
    path.join(f.source, "7", `${key}.md`),
    path.join(f.source, "7", "20000000-0000-4000-8000-000000000001.md")
  )
  const output = path.join(f.root, "second")
  await assert.rejects(
    backup({
      source: f.source,
      output,
      databaseUrl: "postgresql:///private",
      pgDump: f.executable,
    })
  )
  await assert.rejects(fs.stat(path.join(output, "complete.json")))
})

test("libpq gets exact socket connection and explicit TLS; conflicting options fail closed", async (t) => {
  const f = await fixture(t)
  await backup({
    source: f.source,
    output: f.output,
    pgDump: f.executable,
    databaseUrl:
      "postgresql:///neev_identity_test?host=/tmp/neev-pg.aTlLNW&port=55439&user=air1&sslmode=verify-full&sslrootcert=/private/ca.pem&password=literal+value",
  })
  const { env } = JSON.parse(await fs.readFile(f.record))
  assert.equal(env.PGDATABASE, "neev_identity_test")
  assert.equal(env.PGHOST, "/tmp/neev-pg.aTlLNW")
  assert.equal(env.PGPORT, "55439")
  assert.equal(env.PGUSER, "air1")
  assert.equal(env.PGSSLMODE, "verify-full")
  assert.equal(env.PGSSLROOTCERT, "/private/ca.pem")
  assert.equal(env.PGPASSWORD, "literal+value")
  for (const url of [
    "postgresql:///db?host=a&host=b",
    "postgresql://a/db?host=b",
    "postgresql:///db?service=live",
    "postgresql:///db?options=-csearch_path=other",
    "postgresql:///db?sslmode=invalid",
    "postgresql:///db#fragment",
    "postgresql:///first/../private",
  ]) {
    await assert.rejects(
      backup({
        source: f.source,
        output: path.join(f.root, "refused"),
        pgDump: f.executable,
        databaseUrl: url,
      })
    )
    await assert.rejects(fs.stat(path.join(f.root, "refused")))
  }
})

test("CLI output is bounded and child does not inherit PG or provider secrets", async (t) => {
  const f = await fixture(t)
  const result = spawnSync(
    process.execPath,
    [
      path.resolve("scripts/neev-content-backup.mjs"),
      "--source",
      f.source,
      "--output",
      f.output,
    ],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        PATH: `${f.root}:${process.env.PATH}`,
        NEEV_BACKUP_DATABASE_URL: "postgresql:///private?host=/test&user=test",
        PGHOST: "production",
        PGPASSWORD: "inherited-password",
        PGSERVICE: "live",
        PGPASSFILE: "/secret",
        PROVIDER_SECRET: "private-provider",
      },
    }
  )
  assert.equal(result.status, 0)
  assert.equal(result.stderr, "")
  assert.equal(JSON.parse(result.stdout).copied_blobs, 1)
  assert.equal(result.stdout.includes("postgresql://"), false)
  const { env } = JSON.parse(await fs.readFile(f.record))
  assert.equal(env.PGHOST, "/test")
  for (const name of [
    "PGPASSWORD",
    "PGSERVICE",
    "PROVIDER_SECRET",
    "NEEV_BACKUP_DATABASE_URL",
  ])
    assert.equal(env[name], undefined)
  assert.equal(env.PGPASSFILE, "/dev/null")
})

test("interrupted copy has no complete marker; fresh retries have deterministic inventories", async (t) => {
  const f = await fixture(t)
  const options = {
    source: f.source,
    output: f.output,
    pgDump: f.executable,
    databaseUrl: "postgresql:///private",
  }
  await assert.rejects(
    backup({
      ...options,
      onOperation: (phase) => {
        if (phase === "manifest") throw new Error("interrupted")
      },
    })
  )
  assert.equal(
    await fs.readFile(path.join(f.output, "blobs", "7", `${key}.md`), "utf8"),
    "body"
  )
  await assert.rejects(fs.stat(path.join(f.output, "complete.json")))
  assert.equal(
    (await fs.stat(path.join(f.output, "INCOMPLETE"))).isFile(),
    true
  )
  await assert.rejects(backup(options))
  const second = path.join(f.root, "second")
  const third = path.join(f.root, "third")
  await backup({ ...options, output: second })
  await backup({ ...options, output: third })
  assert.equal(
    await fs.readFile(path.join(second, "complete.json"), "utf8"),
    await fs.readFile(path.join(third, "complete.json"), "utf8")
  )
  assert.equal(
    await fs.readFile(path.join(f.source, "7", "pending.tmp"), "utf8"),
    "unfinished"
  )
})
