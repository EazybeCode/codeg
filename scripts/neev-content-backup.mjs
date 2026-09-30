import fs from "node:fs/promises"
import { constants } from "node:fs"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { createHash } from "node:crypto"
import { spawn } from "node:child_process"
import net from "node:net"

const uuid = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
const finalized = new RegExp(`^(${uuid})\\.(md|img|att)$`)
const exactKey = new RegExp(`^${uuid}$`)
const MiB = 1024 * 1024
// A backup read is not use: without O_NOATIME (Linux) every backup would reset
// each blob's idle clock and the Storage Box evictor would never select any.
const NOATIME = constants.O_NOATIME ?? 0
// One blob kind per BlobStore namespace. `max` mirrors the runtime read bound
// the restore verifier enforces for that kind; attachments have no per-file
// rule, so the exact snapshot row is their only bound.
const KINDS = {
  md: { table: "neev_document_revisions", key: "blob_key", min: 0, max: MiB },
  img: { table: "neev_image_blobs", key: "key", min: 1, max: 5 * MiB },
  att: { table: "neev_attachments", key: "key", min: 1, max: null },
}
const TABLES = Object.fromEntries(
  Object.entries(KINDS).map(([kind, spec]) => [spec.table, kind])
)
const REFERENCE_OUTPUT_LIMIT = 512 * MiB
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex")
// Reason codes carry no paths, URLs or content; they tell an operator which
// safety check stopped the run.
const fail = (reason = "invalid") => {
  throw new Error(
    `native content backup failed (${reason}); output is incomplete`
  )
}

async function directory(location, restrictive = true) {
  if (
    !path.isAbsolute(location) ||
    path.resolve(location) !== location ||
    (await fs.realpath(location)) !== location
  )
    fail("path")
  const stat = await fs.lstat(location)
  if (
    !stat.isDirectory() ||
    (restrictive &&
      (stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700))
  )
    fail("directory-contract")
}
async function syncDirectory(location) {
  const handle = await fs.open(
    location,
    constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW
  )
  try {
    await handle.sync()
  } finally {
    await handle.close()
  }
}
async function writeFresh(location, bytes) {
  const handle = await fs.open(
    location,
    constants.O_WRONLY |
      constants.O_CREAT |
      constants.O_EXCL |
      constants.O_NOFOLLOW,
    0o600
  )
  try {
    await handle.writeFile(bytes)
    await handle.sync()
  } finally {
    await handle.close()
  }
}
function checkBlobStat(stat) {
  if (
    !stat.isFile() ||
    stat.uid !== process.getuid() ||
    stat.nlink !== 1 ||
    (stat.mode & 0o777) !== 0o600
  )
    fail("file-contract")
}
async function digestFile(location) {
  const handle = await fs.open(
    location,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK | NOATIME
  )
  try {
    const stat = await handle.stat()
    checkBlobStat(stat)
    const hash = createHash("sha256")
    let size = 0
    for await (const chunk of handle.createReadStream({ autoClose: false })) {
      hash.update(chunk)
      size += chunk.length
    }
    if (size !== stat.size) fail("changed")
    return { sha256: hash.digest("hex"), byte_length: size }
  } finally {
    await handle.close()
  }
}

// Stream one blob into a fresh 0600 file. Never reads past the expected length,
// so a grown, truncated or rewritten source cannot pass; the written copy is
// then re-read from disk and compared again.
async function copyVerified(sourcePath, targetPath, expected) {
  const source = await fs.open(
    sourcePath,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK | NOATIME
  )
  try {
    const before = await source.stat()
    checkBlobStat(before)
    if (before.size !== expected.byte_length) fail("size-mismatch")
    const target = await fs.open(
      targetPath,
      constants.O_WRONLY |
        constants.O_CREAT |
        constants.O_EXCL |
        constants.O_NOFOLLOW,
      0o600
    )
    try {
      const hash = createHash("sha256")
      const buffer = Buffer.alloc(MiB)
      let total = 0
      for (;;) {
        const { bytesRead } = await source.read(buffer, 0, buffer.length, null)
        if (!bytesRead) break
        total += bytesRead
        if (total > expected.byte_length) fail("size-mismatch")
        const chunk = buffer.subarray(0, bytesRead)
        hash.update(chunk)
        let written = 0
        while (written < chunk.length) {
          const result = await target.write(chunk, written)
          written += result.bytesWritten
        }
      }
      const after = await source.stat()
      if (
        total !== expected.byte_length ||
        after.size !== before.size ||
        after.mtimeMs !== before.mtimeMs ||
        hash.digest("hex") !== expected.sha256
      )
        fail("hash-mismatch")
      await target.sync()
    } finally {
      await target.close()
    }
  } finally {
    await source.close()
  }
  const copied = await digestFile(targetPath)
  if (
    copied.sha256 !== expected.sha256 ||
    copied.byte_length !== expected.byte_length
  )
    fail("copy-mismatch")
}

// libpq does not expand a URI supplied through PGDATABASE. Convert only explicit
// supported fields; never inherit service, password, or host overrides.
function connectionEnvironment(raw) {
  if (/[\u0000- \u007f]/.test(raw)) fail()
  const url = new URL(raw)
  if (!["postgres:", "postgresql:"].includes(url.protocol) || url.hash) fail()
  const values = new Map()
  const set = (name, value) => {
    if (!value || value.includes("\0") || values.has(name)) fail()
    values.set(name, value)
  }
  const rawPath = /^postgres(?:ql)?:\/\/[^/?#]*\/([^?#]*)/.exec(raw)?.[1]
  if (!rawPath || rawPath.includes("/")) fail()
  const database = decodeURIComponent(rawPath)
  if (
    !database ||
    database.includes("/") ||
    database !== decodeURIComponent(url.pathname.slice(1))
  )
    fail()
  set("dbname", database)
  if (url.hostname)
    set("host", decodeURIComponent(url.hostname.replace(/^\[|\]$/g, "")))
  if (url.port) set("port", url.port)
  if (url.username) set("user", decodeURIComponent(url.username))
  if (url.password) set("password", decodeURIComponent(url.password))
  const names = {
    host: "PGHOST",
    port: "PGPORT",
    user: "PGUSER",
    password: "PGPASSWORD",
    dbname: "PGDATABASE",
    sslmode: "PGSSLMODE",
    sslrootcert: "PGSSLROOTCERT",
    sslcert: "PGSSLCERT",
    sslkey: "PGSSLKEY",
    sslcrl: "PGSSLCRL",
    sslcrldir: "PGSSLCRLDIR",
    channel_binding: "PGCHANNELBINDING",
    connect_timeout: "PGCONNECT_TIMEOUT",
  }
  // libpq URI values are percent-decoded, not form-decoded: '+' stays literal.
  for (const entry of url.search.slice(1).split("&").filter(Boolean)) {
    const equals = entry.indexOf("=")
    if (equals < 1) fail()
    const name = decodeURIComponent(entry.slice(0, equals))
    const value = decodeURIComponent(entry.slice(equals + 1))
    if (!Object.hasOwn(names, name)) fail()
    set(name, value)
  }
  if (
    values.has("port") &&
    (!/^[0-9]{1,5}$/.test(values.get("port")) ||
      +values.get("port") < 1 ||
      +values.get("port") > 65535)
  )
    fail()
  if (
    values.has("sslmode") &&
    ![
      "disable",
      "allow",
      "prefer",
      "require",
      "verify-ca",
      "verify-full",
    ].includes(values.get("sslmode"))
  )
    fail()
  if (
    values.has("channel_binding") &&
    !["disable", "prefer", "require"].includes(values.get("channel_binding"))
  )
    fail()
  if (
    values.has("connect_timeout") &&
    !/^[1-9][0-9]{0,3}$/.test(values.get("connect_timeout"))
  )
    fail()
  return {
    PATH: process.env.PATH,
    PGPASSFILE: "/dev/null",
    PGCONNECT_TIMEOUT: "15",
    ...Object.fromEntries(
      [...values].map(([name, value]) => [names[name], value])
    ),
  }
}

// pg_restore reads the local archive file only; it gets no connection settings.
function readArchive(pgRestore, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(pgRestore, args, {
      env: { PATH: process.env.PATH, PGPASSFILE: "/dev/null" },
      stdio: ["ignore", "pipe", "ignore"],
    })
    const chunks = []
    let size = 0
    child.stdout.on("data", (chunk) => {
      size += chunk.length
      if (size > REFERENCE_OUTPUT_LIMIT) child.kill("SIGKILL")
      else chunks.push(chunk)
    })
    child.once("error", () => reject(new Error("metadata read failed")))
    child.once("close", (code) =>
      code === 0 && size <= REFERENCE_OUTPUT_LIMIT
        ? resolve(Buffer.concat(chunks).toString("utf8"))
        : reject(new Error("metadata read failed"))
    )
  })
}

// Blob references exactly as recorded in the dumped snapshot, so the copy is
// judged against the same rows the restore verifier will read, not a newer
// live database.
export async function snapshotReferences(dumpPath, schema, pgRestore) {
  const listing = await readArchive(pgRestore, ["--list", dumpPath])
  const tables = new Set()
  for (const line of listing.split("\n")) {
    if (!line.includes(" TABLE DATA ") || line.startsWith(";")) continue
    // An unparseable data entry could hide a blob table: refuse, never skip.
    const entry = /^\d+; \d+ \d+ TABLE DATA (\S+) (\S+)(?: \S*)?$/.exec(line)
    if (!entry) fail("snapshot-list")
    if (entry[1] === schema && Object.hasOwn(TABLES, entry[2]))
      tables.add(entry[2])
  }
  // Every content schema has revisions; images/attachments follow its version.
  if (!tables.has(KINDS.md.table)) fail("snapshot-tables")
  const ordered = [...tables].sort()
  const data = await readArchive(pgRestore, [
    "--data-only",
    "--schema",
    schema,
    ...ordered.flatMap((table) => ["--table", table]),
    "--file=-",
    dumpPath,
  ])
  const references = new Map()
  const seen = new Set()
  const lines = data.split("\n")
  for (let index = 0; index < lines.length; index++) {
    const header =
      /^COPY (?:([a-z_][a-z0-9_]*)\.)?([a-z_][a-z0-9_]*) \(([^)]*)\) FROM stdin;$/.exec(
        lines[index]
      )
    if (!header) continue
    const table = header[2]
    if (
      (header[1] !== undefined && header[1] !== schema) ||
      !tables.has(table) ||
      seen.has(table)
    )
      fail("snapshot-copy")
    seen.add(table)
    const kind = TABLES[table]
    const spec = KINDS[kind]
    const columns = header[3].split(", ").map((c) => c.replace(/^"|"$/g, ""))
    const position = (name) => {
      const at = columns.indexOf(name)
      if (at < 0 || columns.indexOf(name, at + 1) >= 0) fail("snapshot-columns")
      return at
    }
    const org = position("org_id")
    const key = position(spec.key)
    const digest = position("sha256")
    const length = position("byte_length")
    for (index++; ; index++) {
      if (index >= lines.length) fail("snapshot-copy")
      if (lines[index] === "\\.") break
      const fields = lines[index].split("\t")
      if (fields.length !== columns.length) fail("snapshot-row")
      const reference = {
        org_id: fields[org],
        key: fields[key],
        kind,
        sha256: fields[digest],
        byte_length: fields[length],
      }
      if (
        !/^[1-9][0-9]{0,9}$/.test(reference.org_id) ||
        Number(reference.org_id) > 2147483647 ||
        !exactKey.test(reference.key) ||
        !/^[0-9a-f]{64}$/.test(reference.sha256) ||
        !/^(0|[1-9][0-9]{0,15})$/.test(reference.byte_length)
      )
        fail("snapshot-row")
      reference.org_id = Number(reference.org_id)
      reference.byte_length = Number(reference.byte_length)
      if (
        !Number.isSafeInteger(reference.byte_length) ||
        reference.byte_length < spec.min ||
        (spec.max !== null && reference.byte_length > spec.max)
      )
        fail("snapshot-size")
      const id = `${reference.org_id}/${reference.key}.${kind}`
      const previous = references.get(id)
      if (
        previous &&
        (previous.sha256 !== reference.sha256 ||
          previous.byte_length !== reference.byte_length)
      )
        fail("snapshot-conflict")
      references.set(id, reference)
    }
  }
  if (seen.size !== tables.size) fail("snapshot-copy")
  return { tables: ordered, references }
}

// Fetch one evicted original from the root-side broker straight into the
// backup output. Nothing is republished into the live document root, so a
// backup never refills the cache the evictor freed. Only identity and the
// snapshot row's expectations are sent; the bytes are length-bounded, hashed
// and compared with the row here, and any refusal or short read fails closed.
function streamOriginal(socketPath, reference, targetPath) {
  const refuse = () => fail("missing-blob")
  return new Promise((resolve, reject) => {
    let settled = false
    const done = (error) => {
      if (settled) return
      settled = true
      client.destroy()
      if (error) reject(error)
      else resolve()
    }
    let target
    let header = Buffer.alloc(0)
    let received = 0
    let chain = Promise.resolve()
    const hash = createHash("sha256")
    const client = net.createConnection({ path: socketPath })
    client.setTimeout(660000, () => done(new Error("stream timeout")))
    client.on("connect", () =>
      client.write(
        `${JSON.stringify({
          v: 1,
          op: "stream",
          org_id: reference.org_id,
          key: reference.key,
          kind: reference.kind,
          sha256: reference.sha256,
          byte_length: reference.byte_length,
        })}\n`
      )
    )
    const accept = async (chunk) => {
      if (!target) {
        header = Buffer.concat([header, chunk])
        const end = header.indexOf(10)
        if (end < 0) {
          if (header.length > 512) refuse()
          return
        }
        const parsed = JSON.parse(header.subarray(0, end).toString("utf8"))
        if (
          parsed.ok !== true ||
          parsed.result !== "stream" ||
          parsed.byte_length !== reference.byte_length
        )
          refuse()
        target = await fs.open(
          targetPath,
          constants.O_WRONLY |
            constants.O_CREAT |
            constants.O_EXCL |
            constants.O_NOFOLLOW,
          0o600
        )
        chunk = header.subarray(end + 1)
      }
      received += chunk.length
      if (received > reference.byte_length) refuse()
      hash.update(chunk)
      let written = 0
      while (written < chunk.length)
        written += (await target.write(chunk, written)).bytesWritten
    }
    client.on("data", (chunk) => {
      chain = chain.then(() => accept(chunk)).catch(done)
    })
    client.on("error", () => done(new Error("broker unavailable")))
    client.on("close", () => {
      chain
        .then(async () => {
          if (!target) refuse()
          if (
            received !== reference.byte_length ||
            hash.digest("hex") !== reference.sha256
          )
            refuse()
          await target.sync()
        })
        .then(
          () => done(),
          (error) => done(error)
        )
        .finally(() => target?.close())
    })
  }).catch(() => refuse())
}

const byBlob = (a, b) =>
  a.org_id - b.org_id ||
  (a.key < b.key ? -1 : a.key > b.key ? 1 : 0) ||
  (a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0)

// The executable boundary is injectable for an offline process-level test.
export async function backup({
  source,
  output,
  databaseUrl,
  schema = "public",
  pgDump = "pg_dump",
  pgRestore = "pg_restore",
  brokerSocket,
  onOperation = () => {},
}) {
  if (
    process.platform === "win32" ||
    typeof databaseUrl !== "string" ||
    !/^postgres(?:ql)?:\/\//.test(databaseUrl) ||
    !/^[a-z_][a-z0-9_]{0,62}$/.test(schema)
  )
    fail()
  const env = connectionEnvironment(databaseUrl)
  if (!path.isAbsolute(output) || path.resolve(output) !== output) fail()
  await directory(source)
  await directory(path.dirname(output), false)
  if (output === source || output.startsWith(`${source}/`)) fail()
  await fs.mkdir(output, { mode: 0o700 }) // Exclusive, never resume/replace a path.
  await writeFresh(
    path.join(output, "INCOMPLETE"),
    "No complete.json means this backup must not be accepted.\n"
  )
  await syncDirectory(output)
  await syncDirectory(path.dirname(output))
  const dumpPath = path.join(output, "metadata.dump")
  onOperation("pg_dump")
  await new Promise((resolve, reject) => {
    const child = spawn(
      pgDump,
      [
        "--format=custom",
        "--no-owner",
        "--no-privileges",
        "--strict-names",
        "--schema",
        schema,
        "--file",
        dumpPath,
      ],
      {
        env,
        stdio: "ignore", // pg_dump errors can contain the supplied URL.
      }
    )
    child.once("error", () => reject(new Error("metadata dump failed")))
    child.once("exit", (code) =>
      code === 0 ? resolve() : reject(new Error("metadata dump failed"))
    )
  })
  // Dump is private under the owned0700 output even before this mode normalization.
  const dumpHandle = await fs.open(
    dumpPath,
    constants.O_RDWR | constants.O_NOFOLLOW | constants.O_NONBLOCK
  )
  try {
    const stat = await dumpHandle.stat()
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid())
      fail()
    await dumpHandle.chmod(0o600)
    await dumpHandle.sync()
  } finally {
    await dumpHandle.close()
  }
  const dump = await digestFile(dumpPath)
  onOperation("snapshot-references")
  const { tables, references } = await snapshotReferences(
    dumpPath,
    schema,
    pgRestore
  )
  onOperation("copy-finalized")
  const destination = path.join(output, "blobs")
  await fs.mkdir(destination, { mode: 0o700 })
  const blobs = []
  let unreferenced = 0
  // Finals without a snapshot row (late uploads, swept orphans) are not part of
  // this recovery point and are not copied; they must still be well-formed.
  for (const org of (await fs.readdir(source)).sort(
    (a, b) => Number(a) - Number(b)
  )) {
    if (!/^[1-9][0-9]*$/.test(org) || Number(org) > 2147483647) fail("entry")
    const orgSource = path.join(source, org)
    await directory(orgSource)
    const orgDestination = path.join(destination, org)
    let created = false
    const names = (await fs.readdir(orgSource)).sort()
    const temporaries = names.filter((entry) => entry.endsWith(".tmp"))
    for (const name of names) {
      if (name.endsWith(".tmp")) continue
      const match = finalized.exec(name)
      if (!match) fail("entry")
      // Bytes of a kind whose table the snapshot lacks would be silently
      // skipped as unreferenced; that means the snapshot is not understood.
      if (!tables.includes(KINDS[match[2]].table)) fail("snapshot-tables")
      const sourcePath = path.join(orgSource, name)
      const reference = references.get(`${org}/${name}`)
      if (!reference) {
        const stat = await fs.lstat(sourcePath)
        // The app publishes by link-then-unlink; mid-publish a new, not yet
        // referenced final briefly has two links. Anything else is refused.
        const key = name.split(".")[0]
        const publishing =
          stat.nlink === 2 &&
          temporaries.some(
            (entry) =>
              entry === `${key}.tmp` ||
              entry === `${name}.tmp` ||
              entry.startsWith(`${name}.fetch-`)
          )
        if (!publishing) checkBlobStat(stat)
        unreferenced++
        continue
      }
      if (!created) {
        await fs.mkdir(orgDestination, { mode: 0o700 })
        created = true
      }
      await copyVerified(sourcePath, path.join(orgDestination, name), reference)
      references.delete(`${org}/${name}`)
      blobs.push({
        org_id: reference.org_id,
        key: reference.key,
        kind: reference.kind,
        sha256: reference.sha256,
        byte_length: reference.byte_length,
      })
    }
    await directory(orgSource)
    if (created) await syncDirectory(orgDestination)
  }
  // Referenced bytes absent locally were either evicted to the Storage Box
  // (the broker restores them, verified against the same row) or are really
  // gone. An orphan swept after the dump lands here too; a fresh retry excludes
  // its deleted row.
  if (references.size && !brokerSocket) fail("missing-blob")
  for (const reference of [...references.values()].sort(byBlob)) {
    const org = String(reference.org_id)
    const name = `${reference.key}.${reference.kind}`
    onOperation("stream")
    const orgDestination = path.join(destination, org)
    await fs.mkdir(orgDestination, { mode: 0o700 }).catch((error) => {
      if (error.code !== "EEXIST") throw error
    })
    await directory(orgDestination)
    const targetPath = path.join(orgDestination, name)
    await streamOriginal(brokerSocket, reference, targetPath)
    // Re-read the written copy from disk, exactly as a local copy is checked.
    const copied = await digestFile(targetPath)
    if (
      copied.sha256 !== reference.sha256 ||
      copied.byte_length !== reference.byte_length
    )
      fail("copy-mismatch")
    await syncDirectory(orgDestination)
    blobs.push({
      org_id: reference.org_id,
      key: reference.key,
      kind: reference.kind,
      sha256: reference.sha256,
      byte_length: reference.byte_length,
    })
  }
  await directory(source)
  await syncDirectory(destination)
  // Persist metadata.dump and blobs/ directory entries before completion exists.
  await syncDirectory(output)
  onOperation("manifest")
  blobs.sort(byBlob)
  const manifest = {
    format: "neev-content-copy-v2",
    source_schema: schema,
    dump,
    tables,
    blobs,
  }
  const encoded = `${JSON.stringify(manifest)}\n`
  const pending = path.join(output, "complete.json.tmp")
  await writeFresh(pending, encoded)
  // Hard-link publishes a fully synced marker atomically without replacing anything.
  await fs.link(pending, path.join(output, "complete.json"))
  await fs.unlink(pending)
  await syncDirectory(output)
  return {
    exitCode: 0,
    stdout: `${JSON.stringify({
      copied_blobs: blobs.length,
      copied_bytes: blobs.reduce((sum, blob) => sum + blob.byte_length, 0),
      unreferenced_skipped: unreferenced,
      manifest_sha256: sha256(encoded),
    })}\n`,
  }
}

const RESTORE_MARKER = ".restore-incomplete"

// Rebuild a fresh document root from a completed backup: every manifest blob
// is re-hashed while it is copied. The marker is a non-numeric root entry, so
// the backup and Storage Box tools refuse an interrupted restore until it is
// discarded; it is removed only after every file and directory is synced.
export async function restoreBlobs({ backupDirectory, target }) {
  if (process.platform === "win32") fail()
  await directory(backupDirectory)
  if (!path.isAbsolute(target) || path.resolve(target) !== target) fail("path")
  await directory(path.dirname(target), false)
  if (target === backupDirectory || target.startsWith(`${backupDirectory}/`))
    fail("path")
  const encoded = await fs.readFile(
    path.join(backupDirectory, "complete.json"),
    {
      encoding: "utf8",
      flag: constants.O_RDONLY | constants.O_NOFOLLOW,
    }
  )
  const manifest = JSON.parse(encoded)
  // v1 archives predate image/attachment blobs and hold only documents.
  const kindOf = {
    "neev-content-copy-v1": () => "md",
    "neev-content-copy-v2": (b) => b.kind,
  }[manifest?.format]
  if (!kindOf || !Array.isArray(manifest.blobs)) fail("manifest")
  const dump = await digestFile(path.join(backupDirectory, "metadata.dump"))
  if (
    dump.sha256 !== manifest.dump?.sha256 ||
    dump.byte_length !== manifest.dump?.byte_length
  )
    fail("dump-mismatch")
  const entries = manifest.blobs.map((blob) => {
    const entry = { ...blob, kind: kindOf(blob) }
    if (
      !Number.isSafeInteger(entry.org_id) ||
      entry.org_id < 1 ||
      entry.org_id > 2147483647 ||
      !exactKey.test(entry.key) ||
      !Object.hasOwn(KINDS, entry.kind) ||
      !/^[0-9a-f]{64}$/.test(entry.sha256) ||
      !Number.isSafeInteger(entry.byte_length) ||
      entry.byte_length < KINDS[entry.kind].min
    )
      fail("manifest")
    return entry
  })
  await fs.mkdir(target, { mode: 0o700 }) // Exclusive: never restore over a root.
  await writeFresh(
    path.join(target, RESTORE_MARKER),
    "Restore incomplete: discard this directory and restore again.\n"
  )
  await syncDirectory(target)
  await syncDirectory(path.dirname(target))
  const orgs = new Set()
  let bytes = 0
  for (const entry of entries.sort(byBlob)) {
    const org = String(entry.org_id)
    const orgTarget = path.join(target, org)
    if (!orgs.has(org)) {
      await directory(path.join(backupDirectory, "blobs", org))
      await fs.mkdir(orgTarget, { mode: 0o700 })
      orgs.add(org)
    }
    const name = `${entry.key}.${entry.kind}`
    await copyVerified(
      path.join(backupDirectory, "blobs", org, name),
      path.join(orgTarget, name),
      entry
    )
    bytes += entry.byte_length
  }
  for (const org of orgs) await syncDirectory(path.join(target, org))
  await syncDirectory(target)
  await fs.unlink(path.join(target, RESTORE_MARKER))
  await syncDirectory(target)
  return {
    exitCode: 0,
    stdout: `${JSON.stringify({ restored_blobs: entries.length, restored_bytes: bytes })}\n`,
  }
}

function parseFlags(args, allowed) {
  const opts = {}
  for (let i = 0; i < args.length; i += 2) {
    if (!allowed.includes(args[i]) || !args[i + 1] || opts[args[i]]) fail()
    opts[args[i]] = args[i + 1]
  }
  return opts
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const args = process.argv.slice(2)
  const restoring = args[0] === "restore-blobs"
  try {
    let result
    if (restoring) {
      const opts = parseFlags(args.slice(1), ["--backup", "--target"])
      result = await restoreBlobs({
        backupDirectory: opts["--backup"],
        target: opts["--target"],
      })
    } else {
      const opts = parseFlags(args, ["--source", "--output", "--schema"])
      result = await backup({
        source: opts["--source"],
        output: opts["--output"],
        schema: opts["--schema"],
        databaseUrl: process.env.NEEV_BACKUP_DATABASE_URL,
        brokerSocket: process.env.NEEV_ORIGINALS_BROKER_SOCKET || undefined,
      })
    }
    process.stdout.write(result.stdout)
  } catch (error) {
    const reason = /\(([a-z-]+)\)/.exec(String(error?.message))?.[1]
    process.stderr.write(
      restoring
        ? `Native content restore failed${reason ? ` (${reason})` : ""}. Discard the target directory.\n`
        : `Native content backup failed${reason ? ` (${reason})` : ""}. Any newly created output is incomplete unless complete.json exists.\n`
    )
    process.exitCode = 1
  }
}
