import fs from "node:fs/promises"
import { constants } from "node:fs"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { createHash } from "node:crypto"
import { spawn } from "node:child_process"

const uuid = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
const finalized = new RegExp(`^(${uuid})\\.md$`)
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex")
const fail = () => {
  throw new Error("native content backup failed; output is incomplete")
}

async function directory(location, restrictive = true) {
  if (
    !path.isAbsolute(location) ||
    path.resolve(location) !== location ||
    (await fs.realpath(location)) !== location
  )
    fail()
  const stat = await fs.lstat(location)
  if (
    !stat.isDirectory() ||
    (restrictive &&
      (stat.uid !== process.getuid() || (stat.mode & 0o777) !== 0o700))
  )
    fail()
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
async function digestFile(location) {
  const handle = await fs.open(
    location,
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK
  )
  try {
    const stat = await handle.stat()
    if (
      !stat.isFile() ||
      stat.uid !== process.getuid() ||
      stat.nlink !== 1 ||
      (stat.mode & 0o777) !== 0o600
    )
      fail()
    const hash = createHash("sha256")
    let size = 0
    for await (const chunk of handle.createReadStream({ autoClose: false })) {
      hash.update(chunk)
      size += chunk.length
    }
    if (size !== stat.size) fail()
    return { sha256: hash.digest("hex"), byte_length: size }
  } finally {
    await handle.close()
  }
}

// libpq does not expand a URI supplied through PGDATABASE. Convert only explicit
// supported fields; never inherit service, password, or host overrides.
function connectionEnvironment(raw) {
  if (/[\u0000-\u0020\u007f]/.test(raw)) fail()
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

// The executable boundary is injectable for an offline process-level test.
export async function backup({
  source,
  output,
  databaseUrl,
  schema = "public",
  pgDump = "pg_dump",
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
  onOperation("copy-finalized")
  const destination = path.join(output, "blobs")
  await fs.mkdir(destination, { mode: 0o700 })
  const blobs = []
  // No live SQL/reference query here: concurrent late/orphan finals are harmless.
  for (const org of (await fs.readdir(source)).sort(
    (a, b) => Number(a) - Number(b)
  )) {
    if (!/^[1-9][0-9]*$/.test(org) || Number(org) > 2147483647) fail()
    const orgSource = path.join(source, org)
    await directory(orgSource)
    const orgDestination = path.join(destination, org)
    await fs.mkdir(orgDestination, { mode: 0o700 })
    for (const name of (await fs.readdir(orgSource)).sort()) {
      if (name.endsWith(".tmp")) continue
      const match = finalized.exec(name)
      if (!match) fail()
      const handle = await fs.open(
        path.join(orgSource, name),
        constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK
      )
      let bytes
      try {
        const stat = await handle.stat()
        if (
          !stat.isFile() ||
          stat.uid !== process.getuid() ||
          stat.nlink !== 1 ||
          (stat.mode & 0o777) !== 0o600 ||
          stat.size > 1048576
        )
          fail()
        // Bound even a file changed by a concurrent process after fstat.
        const buffer = Buffer.alloc(1048577)
        let size = 0
        while (size < buffer.length) {
          const { bytesRead } = await handle.read(
            buffer,
            size,
            buffer.length - size,
            null
          )
          if (!bytesRead) break
          size += bytesRead
        }
        if (size !== stat.size || size > 1048576) fail()
        bytes = buffer.subarray(0, size)
      } finally {
        await handle.close()
      }
      const target = path.join(orgDestination, name)
      await writeFresh(target, bytes)
      const copied = await digestFile(target)
      if (
        copied.sha256 !== sha256(bytes) ||
        copied.byte_length !== bytes.length
      )
        fail()
      blobs.push({ org_id: Number(org), key: match[1], ...copied })
    }
    await directory(orgSource)
    await syncDirectory(orgDestination)
  }
  await directory(source)
  await syncDirectory(destination)
  // Persist metadata.dump and blobs/ directory entries before completion exists.
  await syncDirectory(output)
  onOperation("manifest")
  const manifest = {
    format: "neev-content-copy-v1",
    source_schema: schema,
    dump,
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
    stdout: `${JSON.stringify({ copied_blobs: blobs.length, manifest_sha256: sha256(encoded) })}\n`,
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    const args = process.argv.slice(2)
    const opts = {}
    for (let i = 0; i < args.length; i += 2) {
      if (
        !["--source", "--output", "--schema"].includes(args[i]) ||
        !args[i + 1] ||
        opts[args[i]]
      )
        fail()
      opts[args[i]] = args[i + 1]
    }
    const result = await backup({
      source: opts["--source"],
      output: opts["--output"],
      schema: opts["--schema"],
      databaseUrl: process.env.NEEV_BACKUP_DATABASE_URL,
    })
    process.stdout.write(result.stdout)
  } catch {
    process.stderr.write(
      "Native content backup failed. Any newly created output is incomplete unless complete.json exists.\n"
    )
    process.exitCode = 1
  }
}
