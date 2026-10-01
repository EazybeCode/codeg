import assert from "node:assert/strict"
import test from "node:test"
import fs from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import { constants as fsConstants } from "node:fs"
import { backup, restoreBlobs } from "./neev-content-backup.mjs"

const key = "10000000-0000-4000-8000-000000000001"
const imageKey = "20000000-0000-4000-8000-000000000002"
const attachmentKey = "30000000-0000-4000-8000-000000000003"
const lateKey = "40000000-0000-4000-8000-000000000004"
const hash = (s) => createHash("sha256").update(s).digest("hex")
// Larger than the obsolete 1 MiB whole-backup cap; attachments have none.
const attachmentBytes = Buffer.alloc(3 * 1024 * 1024 + 7, 0x61)
const imageBytes = Buffer.from("\x89PNG fake image")

const TABLE = {
  md: ["neev_document_revisions", "blob_key"],
  img: ["neev_image_blobs", "key"],
  att: ["neev_attachments", "key"],
  // Schema 22 knowledge rows: separate BlobStore root, `.md` blobs.
  ksnap: ["neev_knowledge_snapshots", "blob_key"],
  krev: ["neev_knowledge_revisions", "blob_key"],
}
// A pg_restore stand-in that serves a listing and COPY text shaped exactly
// like the real tool's output (the real tools are exercised further below).
function archive(schema, rows, { omitCopy = [], listOnly = [] } = {}) {
  const kinds = Object.keys(rows)
  const list = [
    ";",
    "; Archive created at 2026-09-30",
    ...kinds.map(
      (kind, i) =>
        `${3300 + i}; 0 ${16400 + i} TABLE DATA ${schema} ${TABLE[kind][0]} neev`
    ),
    ...listOnly,
  ].join("\n")
  const data = kinds
    .filter((kind) => !omitCopy.includes(kind))
    .map((kind) => {
      const [table, keyColumn] = TABLE[kind]
      return [
        `COPY ${schema}.${table} (org_id, ${keyColumn}, file_name, sha256, byte_length) FROM stdin;`,
        ...rows[kind].map((r) =>
          [r.org, r.key, "name\\twith tab", r.sha, r.size].join("\t")
        ),
        "\\.",
        "",
      ].join("\n")
    })
    .join("\n")
  return {
    list: `${list}\n`,
    data: `--\n-- PostgreSQL database dump\n--\n\n${data}`,
  }
}
const row = (org, k, bytes) => ({
  org,
  key: k,
  sha: hash(bytes),
  size: bytes.length,
})

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
  const restore = path.join(root, "pg_restore")
  const spec = path.join(root, "archive.json")
  await fs.writeFile(
    restore,
    `#!${process.execPath}\nimport fs from 'node:fs';\nconst args=process.argv.slice(2);\nfs.appendFileSync(${JSON.stringify(path.join(root, "restore-record"))},JSON.stringify({args,env:process.env})+'\\n');\nconst spec=JSON.parse(fs.readFileSync(${JSON.stringify(spec)},'utf8'));\nprocess.stdout.write(args[0]==='--list'?spec.list:spec.data);\n`,
    { mode: 0o700 }
  )
  const setArchive = (schema, rows, options) =>
    fs.writeFile(spec, JSON.stringify(archive(schema, rows, options)))
  await setArchive("public", { md: [row(7, key, "body")] })
  return {
    root,
    source,
    output: path.join(root, "backup"),
    executable,
    restore,
    setArchive,
    record: path.join(root, "record"),
    restoreRecord: path.join(root, "restore-record"),
    options: {
      source,
      output: path.join(root, "backup"),
      pgDump: executable,
      pgRestore: restore,
      databaseUrl: "postgresql:///private",
    },
  }
}
async function addBlob(f, org, name, bytes) {
  await fs.mkdir(path.join(f.source, String(org)), {
    mode: 0o700,
    recursive: true,
  })
  await fs.writeFile(path.join(f.source, String(org), name), bytes, {
    mode: 0o600,
  })
}
async function mixedFixture(t) {
  const f = await fixture(t)
  await addBlob(f, 7, `${imageKey}.img`, imageBytes)
  await addBlob(f, 12, `${attachmentKey}.att`, attachmentBytes)
  await addBlob(f, 12, `${lateKey}.att`, "uploaded after the dump")
  await f.setArchive("public", {
    md: [row(7, key, "body")],
    img: [row(7, imageKey, imageBytes)],
    att: [row(12, attachmentKey, attachmentBytes)],
  })
  return f
}
const complete = (output) => path.join(output, "complete.json")

test("dump precedes finalized copy and complete manifest without exposing credentials", async (t) => {
  const f = await fixture(t)
  const events = []
  // The archive lists the public schema: another schema's snapshot is refused.
  const mismatched = path.join(f.root, "mismatched")
  await assert.rejects(
    backup({ ...f.options, output: mismatched, schema: "neev_test_123" })
  )
  await assert.rejects(fs.stat(complete(mismatched)))
  await f.setArchive("neev_test_123", { md: [row(7, key, "body")] })
  const output = path.join(f.root, "matching")
  const accepted = await backup({
    ...f.options,
    output,
    schema: "neev_test_123",
    databaseUrl: "postgresql://user:password@localhost/private",
    onOperation: (operation) => events.push(operation),
  })
  assert.deepEqual(events, [
    "pg_dump",
    "snapshot-references",
    "copy-finalized",
    "manifest",
  ])
  assert.equal(accepted.stdout.includes("postgresql://"), false)
  assert.equal(accepted.exitCode, 0)
  const recorded = JSON.parse(await fs.readFile(f.record))
  assert.deepEqual(recorded.args, [
    "--format=custom",
    "--no-owner",
    "--no-privileges",
    "--strict-names",
    "--schema",
    "neev_test_123",
    "--file",
    path.join(output, "metadata.dump"),
  ])
  assert.equal(recorded.env.PGDATABASE, "private")
  assert.equal(recorded.env.PGUSER, "user")
  assert.equal(recorded.env.PGPASSWORD, "password")
  assert.equal(recorded.args.join(" ").includes("password"), false)
  assert.equal(recorded.env.PROVIDER_SECRET, undefined)
  const manifest = JSON.parse(await fs.readFile(complete(output)))
  assert.equal(manifest.format, "neev-content-copy-v3")
  assert.equal(manifest.knowledge, "not configured")
  assert.equal(manifest.dump.sha256, hash("dump"))
  assert.deepEqual(manifest.blobs, [
    { org_id: 7, key, kind: "md", sha256: hash("body"), byte_length: 4 },
  ])
  assert.deepEqual(await fs.readdir(path.join(output, "blobs", "7")), [
    `${key}.md`,
  ])
  assert.equal((await fs.stat(output)).mode & 0o777, 0o700)
  assert.equal(
    (await fs.stat(path.join(output, "metadata.dump"))).mode & 0o777,
    0o600
  )
  await assert.rejects(
    backup({ ...f.options, output, databaseUrl: "postgresql:///private" })
  )
})

test("pg_restore reads only the archive file and never receives credentials", async (t) => {
  const f = await fixture(t)
  await backup({
    ...f.options,
    databaseUrl: "postgresql://user:password@localhost/private",
  })
  const calls = (await fs.readFile(f.restoreRecord, "utf8"))
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line))
  const dumpPath = path.join(f.output, "metadata.dump")
  assert.deepEqual(
    calls.map((call) => call.args),
    [
      ["--list", dumpPath],
      [
        "--data-only",
        "--schema",
        "public",
        "--table",
        "neev_document_revisions",
        "--file=-",
        dumpPath,
      ],
    ]
  )
  for (const { env, args } of calls) {
    for (const name of ["PGPASSWORD", "PGUSER", "PGHOST", "PGDATABASE"])
      assert.equal(env[name], undefined)
    assert.equal(env.PGPASSFILE, "/dev/null")
    assert.equal(args.join(" ").includes("password"), false)
  }
})

test("documents, images and multi-MiB attachments are copied against exact snapshot rows", async (t) => {
  const f = await mixedFixture(t)
  const result = JSON.parse((await backup(f.options)).stdout)
  assert.deepEqual(result.copied_blobs, 3)
  assert.equal(
    result.copied_bytes,
    4 + imageBytes.length + attachmentBytes.length
  )
  assert.equal(
    result.unreferenced_skipped,
    1,
    "a final with no snapshot row is not copied"
  )
  const manifest = JSON.parse(await fs.readFile(complete(f.output)))
  assert.deepEqual(manifest.tables, [
    "neev_attachments",
    "neev_document_revisions",
    "neev_image_blobs",
  ])
  assert.deepEqual(
    manifest.blobs.map((b) => [b.org_id, b.key, b.kind, b.byte_length]),
    [
      [7, key, "md", 4],
      [7, imageKey, "img", imageBytes.length],
      [12, attachmentKey, "att", attachmentBytes.length],
    ]
  )
  const copied = await fs.readFile(
    path.join(f.output, "blobs", "12", `${attachmentKey}.att`)
  )
  assert.equal(hash(copied), hash(attachmentBytes))
  await assert.rejects(
    fs.stat(path.join(f.output, "blobs", "12", `${lateKey}.att`))
  )
})

test("any disagreement with snapshot metadata fails closed without a complete marker", async (t) => {
  const f = await mixedFixture(t)
  const good = {
    md: [row(7, key, "body")],
    img: [row(7, imageKey, imageBytes)],
    att: [row(12, attachmentKey, attachmentBytes)],
  }
  const variants = {
    "hash-mismatch": [
      "hash-mismatch",
      { ...good, att: [{ ...good.att[0], sha: hash("other") }] },
    ],
    "size-mismatch": [
      "size-mismatch",
      { ...good, img: [{ ...good.img[0], size: imageBytes.length + 1 }] },
    ],
    "missing-blob": [
      "missing-blob",
      {
        ...good,
        att: [
          ...good.att,
          row(12, "50000000-0000-4000-8000-000000000005", "gone"),
        ],
      },
    ],
    "document over its runtime bound": [
      "snapshot-size",
      { ...good, md: [{ ...good.md[0], size: 1024 * 1024 + 1 }] },
    ],
    "image over its runtime bound": [
      "snapshot-size",
      { ...good, img: [{ ...good.img[0], size: 5 * 1024 * 1024 + 1 }] },
    ],
    "empty attachment": [
      "snapshot-size",
      { ...good, att: [{ ...good.att[0], size: 0 }] },
    ],
    "non-uuid key": [
      "snapshot-row",
      { ...good, md: [{ ...good.md[0], key: "status-blob" }] },
    ],
    "uppercase digest": [
      "snapshot-row",
      { ...good, md: [{ ...good.md[0], sha: hash("body").toUpperCase() }] },
    ],
    "conflicting duplicate rows": [
      "snapshot-conflict",
      { ...good, md: [...good.md, { ...good.md[0], sha: hash("x") }] },
    ],
    "attachments present on disk but absent from snapshot": [
      "snapshot-tables",
      { md: good.md, img: good.img },
    ],
  }
  let n = 0
  for (const [name, [code, rows]] of Object.entries(variants)) {
    await f.setArchive("public", rows)
    const output = path.join(f.root, `refused-${n++}`)
    await assert.rejects(
      backup({ ...f.options, output }),
      new RegExp(`\\(${code}\\)`),
      name
    )
    await assert.rejects(fs.stat(complete(output)), undefined, name)
  }
  for (const [name, [code, options]] of Object.entries({
    "listed table without COPY data": ["snapshot-copy", { omitCopy: ["att"] }],
    "unparseable TABLE DATA entry": [
      "snapshot-list",
      { listOnly: ["garbage TABLE DATA public neev_attachments"] },
    ],
  })) {
    await f.setArchive("public", good, options)
    const output = path.join(f.root, `refused-${n++}`)
    await assert.rejects(
      backup({ ...f.options, output }),
      new RegExp(`\\(${code}\\)`),
      name
    )
    await assert.rejects(fs.stat(complete(output)), undefined, name)
  }
  await f.setArchive("public", good)
  await fs.appendFile(
    path.join(f.source, "12", `${attachmentKey}.att`),
    "grown"
  )
  await assert.rejects(
    backup({ ...f.options, output: path.join(f.root, "grown") }),
    /\(size-mismatch\)/
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
    await assert.rejects(backup({ ...f.options, schema }))
  }
  await assert.rejects(backup({ ...f.options, output: "relative" }))
  await assert.rejects(fs.stat(f.output))
})

test("failed dump stays incomplete and symlink final files cannot be copied", async (t) => {
  const f = await fixture(t)
  await assert.rejects(backup({ ...f.options, pgDump: "/usr/bin/false" }))
  await assert.rejects(fs.stat(complete(f.output)))
  await fs.symlink(
    path.join(f.source, "7", `${key}.md`),
    path.join(f.source, "7", "20000000-0000-4000-8000-000000000001.md")
  )
  const output = path.join(f.root, "second")
  await assert.rejects(backup({ ...f.options, output }))
  await assert.rejects(fs.stat(complete(output)))
})

test("libpq gets exact socket connection and explicit TLS; conflicting options fail closed", async (t) => {
  const f = await fixture(t)
  await backup({
    ...f.options,
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
        ...f.options,
        output: path.join(f.root, "refused"),
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
  // A failure names only its reason code.
  await fs.appendFile(path.join(f.source, "7", `${key}.md`), "!")
  const failed = spawnSync(
    process.execPath,
    [
      path.resolve("scripts/neev-content-backup.mjs"),
      "--source",
      f.source,
      "--output",
      path.join(f.root, "failed"),
    ],
    {
      encoding: "utf8",
      env: {
        PATH: `${f.root}:${process.env.PATH}`,
        NEEV_BACKUP_DATABASE_URL: "postgresql://u:secret@h/private",
      },
    }
  )
  assert.equal(failed.status, 1)
  assert.match(failed.stderr, /\(size-mismatch\)/)
  assert.equal(failed.stderr.includes("secret"), false)
  assert.equal(failed.stderr.includes(f.source), false)
})

test("interrupted copy has no complete marker; fresh retries have deterministic inventories", async (t) => {
  const f = await fixture(t)
  await assert.rejects(
    backup({
      ...f.options,
      onOperation: (phase) => {
        if (phase === "manifest") throw new Error("interrupted")
      },
    })
  )
  assert.equal(
    await fs.readFile(path.join(f.output, "blobs", "7", `${key}.md`), "utf8"),
    "body"
  )
  await assert.rejects(fs.stat(complete(f.output)))
  assert.equal(
    (await fs.stat(path.join(f.output, "INCOMPLETE"))).isFile(),
    true
  )
  await assert.rejects(backup(f.options))
  const second = path.join(f.root, "second")
  const third = path.join(f.root, "third")
  await backup({ ...f.options, output: second })
  await backup({ ...f.options, output: third })
  assert.equal(
    await fs.readFile(complete(second), "utf8"),
    await fs.readFile(complete(third), "utf8")
  )
  assert.equal(
    await fs.readFile(path.join(f.source, "7", "pending.tmp"), "utf8"),
    "unfinished"
  )
})

test("restore rebuilds a fresh verified root and refuses tampering", async (t) => {
  const f = await mixedFixture(t)
  await backup(f.options)
  const target = path.join(f.root, "restored")
  const result = JSON.parse(
    (await restoreBlobs({ backupDirectory: f.output, target })).stdout
  )
  assert.equal(result.restored_blobs, 3)
  assert.deepEqual((await fs.readdir(target)).sort(), ["12", "7"])
  assert.equal((await fs.stat(target)).mode & 0o777, 0o700)
  assert.equal((await fs.stat(path.join(target, "12"))).mode & 0o777, 0o700)
  const restored = path.join(target, "12", `${attachmentKey}.att`)
  assert.equal((await fs.stat(restored)).mode & 0o777, 0o600)
  assert.equal((await fs.stat(restored)).nlink, 1)
  assert.equal(hash(await fs.readFile(restored)), hash(attachmentBytes))
  // Never restores over an existing root.
  await assert.rejects(restoreBlobs({ backupDirectory: f.output, target }))
  // Tampered blob: the target keeps its incomplete marker.
  const blob = path.join(f.output, "blobs", "7", `${imageKey}.img`)
  const original = await fs.readFile(blob)
  await fs.writeFile(blob, Buffer.from(original).fill(0x30))
  const tampered = path.join(f.root, "tampered")
  await assert.rejects(
    restoreBlobs({ backupDirectory: f.output, target: tampered })
  )
  assert.equal(
    (await fs.stat(path.join(tampered, ".restore-incomplete"))).isFile(),
    true
  )
  await fs.writeFile(blob, original)
  // Tampered dump is refused before anything is created.
  await fs.appendFile(path.join(f.output, "metadata.dump"), "x")
  const dumpTampered = path.join(f.root, "dump-tampered")
  await assert.rejects(
    restoreBlobs({ backupDirectory: f.output, target: dumpTampered })
  )
  await assert.rejects(fs.stat(dumpTampered))
})

test("restore accepts historical v1 document-only archives", async (t) => {
  const f = await fixture(t)
  await backup(f.options)
  const manifestPath = complete(f.output)
  const manifest = JSON.parse(await fs.readFile(manifestPath))
  const v1 = {
    format: "neev-content-copy-v1",
    source_schema: manifest.source_schema,
    dump: manifest.dump,
    blobs: manifest.blobs.map((blob) => {
      const legacy = { ...blob }
      delete legacy.kind
      return legacy
    }),
  }
  await fs.chmod(manifestPath, 0o600)
  await fs.unlink(manifestPath)
  await fs.writeFile(manifestPath, JSON.stringify(v1), { mode: 0o600 })
  const target = path.join(f.root, "restored-v1")
  await restoreBlobs({ backupDirectory: f.output, target })
  assert.equal(
    await fs.readFile(path.join(target, "7", `${key}.md`), "utf8"),
    "body"
  )
})

// Real PostgreSQL: proves the listing/COPY parsing against actual tool output.
const pgBin = ["initdb", "pg_ctl", "pg_dump", "pg_restore", "psql"].every(
  (tool) => spawnSync("which", [tool]).status === 0
)
test(
  "real pg_dump/pg_restore round trip into a restorable root",
  {
    skip: !pgBin && "PostgreSQL client/server tools not installed",
    timeout: 120000,
  },
  async (t) => {
    const f = await mixedFixture(t)
    const cluster = path.join(f.root, "cluster")
    const socket = await fs.mkdtemp(path.join("/tmp", "neevpg-"))
    t.after(() => fs.rm(socket, { recursive: true, force: true }))
    const run = (cmd, args, extra = {}) => {
      const out = spawnSync(cmd, args, { encoding: "utf8", ...extra })
      assert.equal(out.status, 0, `${cmd} failed: ${out.stderr}`)
      return out.stdout
    }
    run("initdb", [
      "-D",
      cluster,
      "-A",
      "trust",
      "-U",
      "neev",
      "--no-sync",
      "--no-locale",
      "-E",
      "UTF8",
    ])
    const port = String(20000 + Math.floor(Math.random() * 20000))
    run("pg_ctl", [
      "-D",
      cluster,
      "-w",
      "-l",
      path.join(f.root, "pg.log"),
      "-o",
      `-k ${socket} -p ${port} -c listen_addresses=''`,
      "start",
    ])
    t.after(() =>
      spawnSync("pg_ctl", ["-D", cluster, "-m", "immediate", "stop"])
    )
    const sql = [
      "CREATE SCHEMA neev_restore_0123456789abcdef0123456789abcdef;",
      "SET search_path TO neev_restore_0123456789abcdef0123456789abcdef;",
      "CREATE TABLE neev_document_revisions (org_id INTEGER, document_id TEXT, revision INTEGER, blob_key TEXT, sha256 TEXT, byte_length BIGINT);",
      "CREATE TABLE neev_image_blobs (org_id INTEGER, key TEXT, document_id TEXT, content_type TEXT, byte_length BIGINT, sha256 TEXT, created_by INTEGER, created_at BIGINT);",
      "CREATE TABLE neev_attachments (org_id INTEGER, key TEXT, chat_id TEXT, session_id TEXT, file_name TEXT, content_type TEXT, byte_length BIGINT, sha256 TEXT, created_by INTEGER, created_at BIGINT, archived BOOLEAN);",
      "CREATE TABLE neev_notes (body TEXT);",
      `INSERT INTO neev_document_revisions VALUES (7,'doc',1,'${key}','${hash("body")}',4);`,
      `INSERT INTO neev_image_blobs VALUES (7,'${imageKey}','doc','image/png',${imageBytes.length},'${hash(imageBytes)}',1,1);`,
      `INSERT INTO neev_attachments VALUES (12,'${attachmentKey}',NULL,'s','tab\there\nand newline','text/plain',${attachmentBytes.length},'${hash(attachmentBytes)}',1,1,TRUE);`,
      "INSERT INTO neev_notes VALUES ('TABLE DATA public neev_attachments x');",
    ].join("\n")
    run("psql", [
      "-h",
      socket,
      "-p",
      port,
      "-U",
      "neev",
      "-d",
      "postgres",
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      sql,
    ])
    const schema = "neev_restore_0123456789abcdef0123456789abcdef"
    const result = JSON.parse(
      (
        await backup({
          source: f.source,
          output: f.output,
          schema,
          databaseUrl: `postgresql:///postgres?host=${socket}&port=${port}&user=neev`,
        })
      ).stdout
    )
    assert.equal(result.copied_blobs, 3)
    assert.equal(result.unreferenced_skipped, 1)
    const target = path.join(f.root, "restored")
    await restoreBlobs({ backupDirectory: f.output, target })
    assert.equal(
      hash(await fs.readFile(path.join(target, "12", `${attachmentKey}.att`))),
      hash(attachmentBytes)
    )
    // Changing a row to disagree with the bytes is caught from the real dump.
    run("psql", [
      "-h",
      socket,
      "-p",
      port,
      "-U",
      "neev",
      "-d",
      "postgres",
      "-c",
      `UPDATE ${schema}.neev_attachments SET byte_length=byte_length-1`,
    ])
    const refused = path.join(f.root, "refused")
    await assert.rejects(
      backup({
        source: f.source,
        output: refused,
        schema,
        databaseUrl: `postgresql:///postgres?host=${socket}&port=${port}&user=neev`,
      })
    )
    await assert.rejects(fs.stat(complete(refused)))
  }
)

// Evicted originals: the backup streams them from the real root-side broker
// (fed by a fake ssh serving a local "remote" directory) straight into the
// output, verified by row, without refilling the live cache.
const python = spawnSync("python3", ["--version"]).status === 0
test(
  "evicted blobs are streamed from the originals broker into the backup only",
  { skip: !python && "python3 unavailable", timeout: 60000 },
  async (t) => {
    const f = await mixedFixture(t)
    // Real path: the broker refuses symlinked config paths (macOS /tmp is one).
    const base = await fs.realpath(await fs.mkdtemp(path.join("/tmp", "nvbk-")))
    t.after(() => fs.rm(base, { recursive: true, force: true }))
    const remote = path.join(base, "remote")
    const relative = path.join("12", `${attachmentKey}.att`)
    await fs.mkdir(path.join(remote, "neev-originals", "12"), {
      recursive: true,
    })
    await fs.copyFile(
      path.join(f.source, relative),
      path.join(remote, "neev-originals", relative)
    )
    const state = path.join(base, "state")
    await fs.mkdir(state, { mode: 0o700 })
    await fs.writeFile(
      path.join(state, "verified.json"),
      JSON.stringify({
        version: 1,
        verified: {
          [`12/${attachmentKey}.att`]: {
            sha256: hash(attachmentBytes),
            size: attachmentBytes.length,
            verified_at: "t",
          },
        },
      })
    )
    const secret = async (name, body) => {
      const location = path.join(base, name)
      await fs.writeFile(location, body, { mode: 0o600 })
      return location
    }
    const bin = path.join(base, "bin")
    await fs.mkdir(bin)
    await fs.writeFile(
      path.join(bin, "ssh"),
      '#!/bin/sh\nfor last; do :; done\nexec cat "$NEEV_TEST_REMOTE/$last"\n',
      { mode: 0o700 }
    )
    const socket = path.join(base, "b.sock")
    const config = await secret(
      "config.json",
      JSON.stringify({
        host: "u123456.your-storagebox.de",
        user: "u123456",
        allow_main_account: true,
        identity_file: await secret("id", "private"),
        known_hosts_file: await secret("known_hosts", ""),
        remote_root: "neev-originals",
        local_root: f.source,
        state_dir: state,
        broker: {
          socket,
          client_uid: process.getuid(),
          socket_gid: process.getgid(),
          fetch_timeout_seconds: 30,
        },
      })
    )
    const { spawn } = await import("node:child_process")
    const broker = spawn(
      "python3",
      [
        path.resolve("scripts/operations/neev_originals_broker.py"),
        "--config",
        config,
        "serve",
      ],
      {
        env: {
          ...process.env,
          PATH: `${bin}:${process.env.PATH}`,
          NEEV_TEST_REMOTE: remote,
        },
        stdio: "ignore",
      }
    )
    t.after(() => broker.kill())
    for (let i = 0; i < 100; i++) {
      if (await fs.stat(socket).catch(() => null)) break
      await new Promise((resolve) => setTimeout(resolve, 100))
    }
    // Evict the attachment locally, as the evictor would.
    await fs.unlink(path.join(f.source, relative))
    const withoutBroker = path.join(f.root, "no-broker")
    await assert.rejects(
      backup({ ...f.options, output: withoutBroker }),
      /\(missing-blob\)/
    )
    const events = []
    const result = JSON.parse(
      (
        await backup({
          ...f.options,
          brokerSocket: socket,
          onOperation: (operation) => events.push(operation),
        })
      ).stdout
    )
    assert.equal(result.copied_blobs, 3)
    assert.equal(events.filter((event) => event === "stream").length, 1)
    assert.equal(
      hash(await fs.readFile(path.join(f.output, "blobs", relative))),
      hash(attachmentBytes)
    )
    const copied = await fs.lstat(path.join(f.output, "blobs", relative))
    assert.equal(copied.mode & 0o777, 0o600)
    // The live root stays evicted: a backup never undoes eviction.
    await assert.rejects(fs.lstat(path.join(f.source, relative)))
    // A remote copy that disagrees with the row is rejected, not copied.
    await fs.writeFile(
      path.join(remote, "neev-originals", relative),
      Buffer.alloc(attachmentBytes.length, 0x62)
    )
    const tampered = path.join(f.root, "tampered")
    await assert.rejects(
      backup({ ...f.options, output: tampered, brokerSocket: socket }),
      /\(missing-blob\)/
    )
    await assert.rejects(fs.stat(complete(tampered)))
    // An evicted blob the broker cannot verify still fails the backup closed.
    await fs.unlink(path.join(f.source, "7", `${imageKey}.img`))
    await assert.rejects(
      backup({
        ...f.options,
        output: path.join(f.root, "unverified"),
        brokerSocket: socket,
      }),
      /\(missing-blob\)/
    )
    await assert.rejects(fs.stat(complete(path.join(f.root, "unverified"))))
  }
)

test("a finalized blob mid-publish (two links, sibling temp) does not abort a backup", async (t) => {
  const f = await mixedFixture(t)
  const late = path.join(f.source, "12", `${lateKey}.att`)
  await fs.link(late, path.join(f.source, "12", `${lateKey}.att.tmp`))
  const result = JSON.parse((await backup(f.options)).stdout)
  assert.equal(result.unreferenced_skipped, 1)
  // A second link with no publish in progress is still refused.
  await fs.unlink(path.join(f.source, "12", `${lateKey}.att.tmp`))
  await fs.link(
    late,
    path.join(f.source, "12", "60000000-0000-4000-8000-000000000006.att")
  )
  await assert.rejects(
    backup({ ...f.options, output: path.join(f.root, "linked") })
  )
})

test(
  "a backup does not refresh blob access times (O_NOATIME)",
  { skip: fsConstants.O_NOATIME === undefined && "O_NOATIME is Linux-only" },
  async (t) => {
    const f = await mixedFixture(t)
    const blob = path.join(f.source, "12", `${attachmentKey}.att`)
    const old = new Date(Date.now() - 30 * 86400 * 1000)
    await fs.utimes(blob, old, old)
    await backup(f.options)
    assert.ok((await fs.stat(blob)).atimeMs < Date.now() - 29 * 86400 * 1000)
  }
)

// ---------------------------------------------------------------------------
// v3: org knowledge blobs (schema 22). The dump is the index: every knowledge
// row in it must have its bytes in the same backup, and the two restore
// together. Knowledge is never evicted, so there is no broker fallback.
// ---------------------------------------------------------------------------
const snapKey = "a0000000-0000-4000-8000-00000000000a"
const revKey = "b0000000-0000-4000-8000-00000000000b"
const strayKey = "c0000000-0000-4000-8000-00000000000c"
const snapBody = "## Source: chat\nsnapshot text\n"
const revBody = "# Page\n\nBody [^k-0123456789ab]\n"

async function knowledgeFixture(t, { rows, files } = {}) {
  const f = await mixedFixture(t)
  const knowledge = path.join(f.root, "knowledge")
  await fs.mkdir(knowledge, { mode: 0o700 })
  const put = async (org, name, bytes) => {
    await fs.mkdir(path.join(knowledge, String(org)), {
      mode: 0o700,
      recursive: true,
    })
    await fs.writeFile(path.join(knowledge, String(org), name), bytes, {
      mode: 0o600,
    })
  }
  for (const [org, name, bytes] of files ?? [
    [3, `${snapKey}.md`, snapBody],
    [3, `${revKey}.md`, revBody],
  ])
    await put(org, name, bytes)
  const good = {
    md: [row(7, key, "body")],
    img: [row(7, imageKey, imageBytes)],
    att: [row(12, attachmentKey, attachmentBytes)],
    ksnap: [row(3, snapKey, snapBody)],
    krev: [row(3, revKey, revBody)],
  }
  await f.setArchive("public", rows ?? good)
  return {
    ...f,
    knowledge,
    put,
    good,
    options: { ...f.options, knowledgeSource: knowledge },
  }
}

test("v3 copies knowledge snapshot and revision blobs beside the dump that indexes them", async (t) => {
  const f = await knowledgeFixture(t)
  const result = JSON.parse((await backup(f.options)).stdout)
  assert.equal(result.copied_blobs, 3, "document-root blobs unchanged")
  assert.equal(result.knowledge_copied_blobs, 2)
  assert.equal(result.knowledge_unreferenced_skipped, 0)
  for (const [name, body] of [
    [snapKey, snapBody],
    [revKey, revBody],
  ]) {
    const copy = path.join(f.output, "blobs-knowledge", "3", `${name}.md`)
    assert.equal(await fs.readFile(copy, "utf8"), body)
    assert.equal((await fs.stat(copy)).mode & 0o777, 0o600)
  }
  assert.equal(
    (await fs.stat(path.join(f.output, "blobs-knowledge", "3"))).mode & 0o777,
    0o700
  )
  // Knowledge never lands in the document-root copy, and vice versa.
  await assert.rejects(fs.stat(path.join(f.output, "blobs", "3")))
  const encoded = await fs.readFile(complete(f.output), "utf8")
  const manifest = JSON.parse(encoded)
  assert.equal(manifest.format, "neev-content-copy-v3")
  assert.deepEqual(manifest.tables, [
    "neev_attachments",
    "neev_document_revisions",
    "neev_image_blobs",
  ])
  assert.deepEqual(manifest.knowledge, {
    tables: ["neev_knowledge_revisions", "neev_knowledge_snapshots"],
    blobs: [
      {
        org_id: 3,
        key: snapKey,
        sha256: hash(snapBody),
        byte_length: snapBody.length,
      },
      {
        org_id: 3,
        key: revKey,
        sha256: hash(revBody),
        byte_length: revBody.length,
      },
    ],
  })
  // The manifest names no host path, credential or content.
  for (const leak of [f.root, f.knowledge, "postgresql", "snapshot text"])
    assert.equal(encoded.includes(leak), false, leak)
})

test("a knowledge blob that disagrees with its row fails the whole backup", async (t) => {
  const variants = {
    "hash-mismatch": (g) => ({
      ...g,
      ksnap: [{ ...g.ksnap[0], sha: hash("other") }],
    }),
    "size-mismatch": (g) => ({
      ...g,
      krev: [{ ...g.krev[0], size: revBody.length + 1 }],
    }),
    "missing-blob": (g) => ({
      ...g,
      krev: [...g.krev, row(3, strayKey, "never written")],
    }),
    "snapshot-conflict": (g) => ({
      ...g,
      krev: [...g.krev, { ...g.krev[0], sha: hash("x") }],
    }),
  }
  let n = 0
  for (const [code, mutate] of Object.entries(variants)) {
    const f = await knowledgeFixture(t)
    await f.setArchive("public", mutate(f.good))
    const output = path.join(f.root, `refused-${n++}`)
    await assert.rejects(
      backup({ ...f.options, output }),
      new RegExp(`\\(${code}\\)`),
      code
    )
    await assert.rejects(fs.stat(complete(output)), undefined, code)
  }
})

test("knowledge rows are bounded exactly like their schema-22 CHECKs", async (t) => {
  // Authoritative Task 1 DDL: snapshots 0..65536 bytes (an empty capture is
  // valid), revisions 1..65536. Both boundaries are exercised with real bytes.
  const atCap = "s".repeat(65536)
  const overCap = "s".repeat(65537)
  const f = await knowledgeFixture(t, {
    files: [
      [3, `${snapKey}.md`, atCap],
      [3, `${revKey}.md`, atCap],
      [3, `${strayKey}.md`, ""],
    ],
  })
  await f.setArchive("public", {
    ...f.good,
    ksnap: [row(3, snapKey, atCap), row(3, strayKey, "")],
    krev: [row(3, revKey, atCap)],
  })
  const ok = JSON.parse((await backup(f.options)).stdout)
  assert.equal(
    ok.knowledge_copied_blobs,
    3,
    "65536-byte snapshot/revision and an empty snapshot"
  )
  const bad = {
    "empty revision": { krev: [{ ...row(3, revKey, atCap), size: 0 }] },
    "revision over 64 KiB": { krev: [row(3, revKey, overCap)] },
    "snapshot over 64 KiB": { ksnap: [row(3, snapKey, overCap)] },
  }
  let n = 0
  for (const [name, change] of Object.entries(bad)) {
    await f.setArchive("public", { ...f.good, ...change })
    const output = path.join(f.root, `bounded-${n++}`)
    await assert.rejects(
      backup({ ...f.options, output }),
      /\(snapshot-size\)/,
      name
    )
    await assert.rejects(fs.stat(complete(output)), undefined, name)
  }
})

test("a v3 manifest knowledge entry over 64 KiB is refused at restore", async (t) => {
  const f = await knowledgeFixture(t)
  await backup(f.options)
  const manifestPath = complete(f.output)
  const manifest = JSON.parse(await fs.readFile(manifestPath))
  for (const [n, byte_length] of [65537, 1048576].entries()) {
    const forged = structuredClone(manifest)
    forged.knowledge.blobs[0].byte_length = byte_length
    await fs.unlink(manifestPath)
    await fs.writeFile(manifestPath, JSON.stringify(forged), { mode: 0o600 })
    const target = path.join(f.root, `over-${n}`)
    await assert.rejects(
      restoreBlobs({
        backupDirectory: f.output,
        target,
        knowledgeTarget: path.join(f.root, `over-k-${n}`),
      }),
      /\(manifest\)/,
      String(byte_length)
    )
    await assert.rejects(fs.stat(target))
  }
})

test("tenant isolation: a knowledge blob is only accepted under its own org", async (t) => {
  // Same key and bytes, but filed under org 4 while the row says org 3.
  const f = await knowledgeFixture(t, {
    files: [
      [4, `${snapKey}.md`, snapBody],
      [3, `${revKey}.md`, revBody],
    ],
  })
  // Even with a broker configured, knowledge is never fetched remotely.
  await assert.rejects(
    backup({ ...f.options, brokerSocket: "/nonexistent/broker.sock" }),
    /\(missing-blob\)/
  )
  await assert.rejects(fs.stat(complete(f.output)))
  // A non-numeric org directory or a foreign file name is refused outright.
  for (const [dir, name] of [
    ["03", `${snapKey}.md`],
    ["3", `${snapKey}.att`],
    ["3", "notes.txt"],
  ]) {
    const g = await knowledgeFixture(t)
    await fs.mkdir(path.join(g.knowledge, dir), {
      mode: 0o700,
      recursive: true,
    })
    await fs.writeFile(path.join(g.knowledge, dir, name), "x", { mode: 0o600 })
    await assert.rejects(backup(g.options), /\(entry\)/, `${dir}/${name}`)
  }
})

test("unreferenced knowledge files are counted, verified for shape, and not copied", async (t) => {
  const f = await knowledgeFixture(t)
  await f.put(3, `${strayKey}.md`, "orphan from a failed commit")
  await f.put(3, `${strayKey}.md.tmp`, "temporary")
  const result = JSON.parse((await backup(f.options)).stdout)
  assert.equal(result.knowledge_unreferenced_skipped, 1)
  await assert.rejects(
    fs.stat(path.join(f.output, "blobs-knowledge", "3", `${strayKey}.md`))
  )
})

test("knowledge rows without a configured knowledge source fail closed", async (t) => {
  // A dump that indexes knowledge blobs is only a backup if the bytes are in it.
  const f = await knowledgeFixture(t)
  const output = path.join(f.root, "no-source")
  await assert.rejects(
    backup({ ...f.options, knowledgeSource: undefined, output }),
    /\(knowledge-not-configured\)/
  )
  await assert.rejects(fs.stat(complete(output)))
  // Knowledge tables with zero rows and no source: recorded, not invented.
  await f.setArchive("public", { ...f.good, ksnap: [], krev: [] })
  const empty = path.join(f.root, "empty-tables")
  await backup({ ...f.options, knowledgeSource: undefined, output: empty })
  assert.equal(
    JSON.parse(await fs.readFile(complete(empty))).knowledge,
    "not configured"
  )
})

test("knowledge files on disk with a pre-22 dump are not silently skipped", async (t) => {
  const f = await knowledgeFixture(t)
  await f.setArchive("public", {
    md: f.good.md,
    img: f.good.img,
    att: f.good.att,
  })
  await assert.rejects(backup(f.options), /\(snapshot-tables\)/)
  // An empty knowledge root with a pre-22 dump is a valid, empty knowledge copy.
  const g = await knowledgeFixture(t, { files: [] })
  await g.setArchive("public", {
    md: g.good.md,
    img: g.good.img,
    att: g.good.att,
  })
  const manifest = JSON.parse(
    await (await backup(g.options), fs.readFile(complete(g.output)))
  )
  assert.deepEqual(manifest.knowledge, { tables: [], blobs: [] })
})

test("the knowledge root must be a private directory apart from the document root and output", async (t) => {
  const f = await knowledgeFixture(t)
  let n = 0
  const attempt = (knowledgeSource) =>
    backup({
      ...f.options,
      knowledgeSource,
      output: path.join(f.root, `path-${n++}`),
    })
  await assert.rejects(attempt(f.source), /\(path\)/)
  await assert.rejects(attempt(path.join(f.source, "7")), /\(path\)/)
  await assert.rejects(attempt("relative/knowledge"))
  await fs.chmod(f.knowledge, 0o755)
  await assert.rejects(attempt(f.knowledge), /\(directory-contract\)/)
  await fs.chmod(f.knowledge, 0o700)
  await assert.rejects(
    backup({ ...f.options, output: path.join(f.knowledge, "inside") }),
    /\(path\)/
  )
})

test("v3 restore rebuilds both roots together and refuses a partial restore", async (t) => {
  const f = await knowledgeFixture(t)
  await backup(f.options)
  const target = path.join(f.root, "restored-documents")
  const knowledgeTarget = path.join(f.root, "restored-knowledge")
  // Knowledge rows exist in the dump: restoring only one root is refused
  // before anything is created.
  await assert.rejects(
    restoreBlobs({ backupDirectory: f.output, target }),
    /\(knowledge-target\)/
  )
  await assert.rejects(fs.stat(target))
  for (const bad of [target, path.join(target, "k"), path.join(f.output, "k")])
    await assert.rejects(
      restoreBlobs({ backupDirectory: f.output, target, knowledgeTarget: bad }),
      /\(path\)/
    )
  const result = JSON.parse(
    (await restoreBlobs({ backupDirectory: f.output, target, knowledgeTarget }))
      .stdout
  )
  assert.equal(result.restored_blobs, 3)
  assert.equal(result.restored_knowledge_blobs, 2)
  assert.deepEqual(await fs.readdir(knowledgeTarget), ["3"])
  assert.deepEqual((await fs.readdir(path.join(knowledgeTarget, "3"))).sort(), [
    `${snapKey}.md`,
    `${revKey}.md`,
  ])
  assert.equal(
    await fs.readFile(path.join(knowledgeTarget, "3", `${revKey}.md`), "utf8"),
    revBody
  )
  assert.equal((await fs.stat(knowledgeTarget)).mode & 0o777, 0o700)
  assert.deepEqual((await fs.readdir(target)).sort(), ["12", "7"])
  // A tampered knowledge blob leaves BOTH roots marked incomplete.
  const blob = path.join(f.output, "blobs-knowledge", "3", `${revKey}.md`)
  await fs.writeFile(blob, revBody.replace("Page", "Pwnd"))
  const t2 = path.join(f.root, "t2")
  const k2 = path.join(f.root, "k2")
  await assert.rejects(
    restoreBlobs({ backupDirectory: f.output, target: t2, knowledgeTarget: k2 })
  )
  for (const root of [t2, k2])
    assert.equal(
      (await fs.stat(path.join(root, ".restore-incomplete"))).isFile(),
      true,
      root
    )
})

test("v2 archives still restore, and need no knowledge target", async (t) => {
  const f = await mixedFixture(t)
  await backup(f.options)
  const manifestPath = complete(f.output)
  const v3 = JSON.parse(await fs.readFile(manifestPath))
  const v2 = { ...v3, format: "neev-content-copy-v2" }
  delete v2.knowledge
  await fs.unlink(manifestPath)
  await fs.writeFile(manifestPath, JSON.stringify(v2), { mode: 0o600 })
  const target = path.join(f.root, "restored-v2")
  const result = JSON.parse(
    (await restoreBlobs({ backupDirectory: f.output, target })).stdout
  )
  assert.equal(result.restored_blobs, 3)
  assert.equal(result.restored_knowledge_blobs, 0)
  // A v3 manifest that claims knowledge without a valid shape is refused.
  const broken = { ...v3, knowledge: { tables: [], blobs: [{ org_id: 3 }] } }
  await fs.unlink(manifestPath)
  await fs.writeFile(manifestPath, JSON.stringify(broken), { mode: 0o600 })
  await assert.rejects(
    restoreBlobs({
      backupDirectory: f.output,
      target: path.join(f.root, "broken"),
      knowledgeTarget: path.join(f.root, "broken-k"),
    }),
    /\(manifest\)/
  )
})

test("CLI: --knowledge-source and restore-blobs --knowledge-target", async (t) => {
  const f = await knowledgeFixture(t)
  const cli = (args, env = {}) =>
    spawnSync(
      process.execPath,
      [path.resolve("scripts/neev-content-backup.mjs"), ...args],
      {
        encoding: "utf8",
        env: {
          PATH: `${f.root}:${process.env.PATH}`,
          NEEV_BACKUP_DATABASE_URL: "postgresql:///private?host=/test&user=t",
          ...env,
        },
      }
    )
  const made = cli([
    "--source",
    f.source,
    "--knowledge-source",
    f.knowledge,
    "--output",
    f.output,
  ])
  assert.equal(made.status, 0, made.stderr)
  assert.equal(JSON.parse(made.stdout).knowledge_copied_blobs, 2)
  const restored = cli([
    "restore-blobs",
    "--backup",
    f.output,
    "--target",
    path.join(f.root, "r"),
    "--knowledge-target",
    path.join(f.root, "rk"),
  ])
  assert.equal(restored.status, 0, restored.stderr)
  assert.equal(JSON.parse(restored.stdout).restored_knowledge_blobs, 2)
  const refused = cli([
    "--source",
    f.source,
    "--output",
    path.join(f.root, "refused"),
  ])
  assert.equal(refused.status, 1)
  assert.match(refused.stderr, /\(knowledge-not-configured\)/)
  assert.equal(refused.stderr.includes(f.root), false)
})

test(
  "real pg_dump/pg_restore: schema-22 knowledge rows and blobs round trip together",
  {
    skip: !pgBin && "PostgreSQL client/server tools not installed",
    timeout: 120000,
  },
  async (t) => {
    const f = await knowledgeFixture(t)
    const cluster = path.join(f.root, "cluster")
    const socket = await fs.mkdtemp(path.join("/tmp", "neevpg-"))
    t.after(() => fs.rm(socket, { recursive: true, force: true }))
    const run = (cmd, args) => {
      const out = spawnSync(cmd, args, { encoding: "utf8" })
      assert.equal(out.status, 0, `${cmd} failed: ${out.stderr}`)
      return out.stdout
    }
    run("initdb", [
      "-D",
      cluster,
      "-A",
      "trust",
      "-U",
      "neev",
      "--no-sync",
      "--no-locale",
      "-E",
      "UTF8",
    ])
    const port = String(20000 + Math.floor(Math.random() * 20000))
    run("pg_ctl", [
      "-D",
      cluster,
      "-w",
      "-l",
      path.join(f.root, "pg.log"),
      "-o",
      `-k ${socket} -p ${port} -c listen_addresses=''`,
      "start",
    ])
    t.after(() =>
      spawnSync("pg_ctl", ["-D", cluster, "-m", "immediate", "stop"])
    )
    const schema = "neev_restore_22"
    const psql = (sql) =>
      run("psql", [
        "-h",
        socket,
        "-p",
        port,
        "-U",
        "neev",
        "-d",
        "postgres",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        sql,
      ])
    // Column lists follow the schema-22 DDL (Task 1), so COPY headers are real.
    psql(
      [
        `CREATE SCHEMA ${schema}; SET search_path TO ${schema};`,
        "CREATE TABLE neev_document_revisions (org_id INTEGER, document_id TEXT, revision INTEGER, blob_key TEXT, sha256 TEXT, byte_length BIGINT);",
        "CREATE TABLE neev_image_blobs (org_id INTEGER, key TEXT, document_id TEXT, content_type TEXT, byte_length BIGINT, sha256 TEXT, created_by INTEGER, created_at BIGINT);",
        "CREATE TABLE neev_attachments (org_id INTEGER, key TEXT, chat_id TEXT, session_id TEXT, file_name TEXT, content_type TEXT, byte_length BIGINT, sha256 TEXT, created_by INTEGER, created_at BIGINT, archived BOOLEAN);",
        "CREATE TABLE neev_knowledge_snapshots (org_id integer NOT NULL, id text NOT NULL, source_id text NOT NULL, source_kind text NOT NULL, engine boolean NOT NULL, segment integer NOT NULL, from_sequence bigint, to_sequence bigint, marker text NOT NULL, blob_key text NOT NULL, sha256 text NOT NULL, byte_length bigint NOT NULL, state text NOT NULL, captured_at bigint NOT NULL);",
        "CREATE TABLE neev_knowledge_revisions (org_id integer NOT NULL, page_id text NOT NULL, revision integer NOT NULL, blob_key text NOT NULL, sha256 text NOT NULL, byte_length bigint NOT NULL, parent_revision integer, batch_id text NOT NULL, created_at bigint NOT NULL);",
        `INSERT INTO neev_document_revisions VALUES (7,'doc',1,'${key}','${hash("body")}',4);`,
        `INSERT INTO neev_image_blobs VALUES (7,'${imageKey}','doc','image/png',${imageBytes.length},'${hash(imageBytes)}',1,1);`,
        `INSERT INTO neev_attachments VALUES (12,'${attachmentKey}',NULL,'s','f','text/plain',${attachmentBytes.length},'${hash(attachmentBytes)}',1,1,FALSE);`,
        `INSERT INTO neev_knowledge_snapshots VALUES (3,'s1','chat-1','chat',false,0,1,67,'m','${snapKey}','${hash(snapBody)}',${snapBody.length},'current',1);`,
        `INSERT INTO neev_knowledge_revisions VALUES (3,'p1',1,'${revKey}','${hash(revBody)}',${revBody.length},NULL,'b1',1);`,
      ].join("\n")
    )
    const databaseUrl = `postgresql:///postgres?host=${socket}&port=${port}&user=neev`
    // The real tools, not the fixture's stand-ins.
    const real = {
      pgDump: "pg_dump",
      pgRestore: "pg_restore",
      schema,
      databaseUrl,
    }
    const result = JSON.parse((await backup({ ...f.options, ...real })).stdout)
    assert.equal(result.knowledge_copied_blobs, 2)
    const manifest = JSON.parse(await fs.readFile(complete(f.output)))
    assert.deepEqual(manifest.knowledge.tables, [
      "neev_knowledge_revisions",
      "neev_knowledge_snapshots",
    ])
    const target = path.join(f.root, "rd")
    const knowledgeTarget = path.join(f.root, "rk")
    await restoreBlobs({ backupDirectory: f.output, target, knowledgeTarget })
    assert.equal(
      await fs.readFile(
        path.join(knowledgeTarget, "3", `${snapKey}.md`),
        "utf8"
      ),
      snapBody
    )
    // The real dump is the index: a row edited to disagree is caught.
    psql(`UPDATE ${schema}.neev_knowledge_revisions SET sha256='${hash("x")}'`)
    await assert.rejects(
      backup({ ...f.options, ...real, output: path.join(f.root, "x") }),
      /\(hash-mismatch\)/
    )
  }
)
