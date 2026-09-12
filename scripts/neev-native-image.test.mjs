import assert from "node:assert/strict"
import { randomUUID } from "node:crypto"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

const suppliedImage = process.env.NEEV_IMAGE_UNDER_TEST?.trim()
const integrationSkip = suppliedImage
  ? false
  : "actual-image integration gate skipped: set NEEV_IMAGE_UNDER_TEST to an image already present locally"

const dockerTimeoutMs = 30_000
let immutableImageId

function docker(args, { allowFailure = false } = {}) {
  const result = spawnSync("docker", args, {
    encoding: "utf8",
    timeout: dockerTimeoutMs,
    maxBuffer: 4 * 1024 * 1024,
  })

  assert.equal(
    result.error,
    undefined,
    `docker ${args[0]} could not run: ${result.error?.message ?? "unknown error"}`
  )

  if (!allowFailure) {
    assert.equal(
      result.status,
      0,
      `docker ${args[0]} failed (${result.status}): ${result.stderr || result.stdout}`
    )
  }

  return result
}

function resolveLocalImage() {
  if (immutableImageId) return immutableImageId

  const inspected = docker([
    "image",
    "inspect",
    "--format",
    "{{.Id}}",
    suppliedImage,
  ])
  immutableImageId = inspected.stdout.trim()
  assert.match(
    immutableImageId,
    /^sha256:[a-f0-9]{64}$/,
    "docker image inspect did not return an immutable image ID"
  )
  return immutableImageId
}

function uniqueContainerName(label) {
  return `neev-native-${label}-${process.pid}-${randomUUID()}`
}

function cleanupContainer(name, invokeDocker = docker) {
  const removed = invokeDocker(["container", "rm", "--force", name], {
    allowFailure: true,
  })
  if (removed.status === 0) return

  const inspected = invokeDocker(["container", "inspect", name], {
    allowFailure: true,
  })
  const inspectOutput = `${inspected.stdout}\n${inspected.stderr}`
  if (
    inspected.status === 1 &&
    /no such (?:container|object)(?::|\s)/i.test(inspectOutput)
  ) {
    return
  }

  assert.fail(
    `failed to remove test container ${name}: ${removed.stderr || removed.stdout}; ` +
      `container inspection failed or still found it: ${inspected.stderr || inspected.stdout}`
  )
}

function rootWriteProbeCommand(target = "/neev-native-write-test") {
  assert.match(target, /^\/[A-Za-z0-9_./-]+$/, "unsafe root-write probe path")
  return `if touch '${target}'; then echo 'runtime root is writable' >&2; exit 1; fi`
}

const confinementArgs = [
  "--network",
  "none",
  "--read-only",
  "--cap-drop",
  "ALL",
  "--security-opt",
  "no-new-privileges:true",
  "--pids-limit",
  "128",
  "--cpus",
  "1",
  "--memory",
  "1g",
  "--memory-swap",
  "1g",
  "--tmpfs",
  "/tmp:rw,noexec,nosuid,nodev,size=64m",
]

function dockerResult(status, stderr = "", stdout = "") {
  return {
    pid: 1,
    output: [null, stdout, stderr],
    stdout,
    stderr,
    status,
    signal: null,
    error: undefined,
  }
}

function scriptedDocker(steps) {
  const remaining = [...steps]
  const calls = []
  return {
    calls,
    invoke(args, options) {
      calls.push({ args, options })
      assert.ok(
        remaining.length > 0,
        "cleanup issued an unexpected Docker call"
      )
      return remaining.shift()
    },
    assertFinished() {
      assert.equal(
        remaining.length,
        0,
        "cleanup omitted an expected Docker call"
      )
    },
  }
}

test("cleanup accepts successful removal of its exact container", () => {
  const scripted = scriptedDocker([dockerResult(0)])

  cleanupContainer("neev-native-owned", scripted.invoke)

  scripted.assertFinished()
  assert.deepEqual(scripted.calls, [
    {
      args: ["container", "rm", "--force", "neev-native-owned"],
      options: { allowFailure: true },
    },
  ])
})

test("cleanup accepts a container confirmed genuinely absent", () => {
  const scripted = scriptedDocker([
    dockerResult(1, "Error: No such container: neev-native-absent"),
    dockerResult(1, "Error: No such object: neev-native-absent"),
  ])

  cleanupContainer("neev-native-absent", scripted.invoke)

  scripted.assertFinished()
  assert.deepEqual(
    scripted.calls.map(({ args }) => args),
    [
      ["container", "rm", "--force", "neev-native-absent"],
      ["container", "inspect", "neev-native-absent"],
    ]
  )
})

test("cleanup rejects a removal error when its container remains", () => {
  const scripted = scriptedDocker([
    dockerResult(1, "permission denied"),
    dockerResult(0, "", '[{"State":{"Status":"running"}}]'),
  ])

  assert.throws(
    () => cleanupContainer("neev-native-remains", scripted.invoke),
    /failed to remove test container neev-native-remains/
  )
  scripted.assertFinished()
})

test("cleanup rejects Docker daemon failure instead of treating it as absence", () => {
  const daemonFailure = "Cannot connect to the Docker daemon"
  const scripted = scriptedDocker([
    dockerResult(1, daemonFailure),
    dockerResult(1, daemonFailure),
  ])

  assert.throws(
    () => cleanupContainer("neev-native-daemon", scripted.invoke),
    /failed to remove test container neev-native-daemon/
  )
  scripted.assertFinished()
})

test("diagnostic root-write probe fails when its target is writable", () => {
  const probeDirectory = mkdtempSync(join(tmpdir(), "neev-native-write-probe-"))
  const probeTarget = join(probeDirectory, "writable-target")

  try {
    const result = spawnSync(
      "/bin/sh",
      [
        "-c",
        ["set -eu", rootWriteProbeCommand(probeTarget), "exit 0"].join("\n"),
      ],
      { encoding: "utf8" }
    )

    assert.equal(result.error, undefined)
    assert.equal(result.status, 1, "writable root probe did not fail closed")
    assert.match(result.stderr, /runtime root is writable/)
  } finally {
    rmSync(probeDirectory, { recursive: true, force: true })
  }
})

test(
  "actual image metadata has only the native fail-closed defaults",
  { skip: integrationSkip },
  () => {
    const imageId = resolveLocalImage()
    const inspected = docker([
      "image",
      "inspect",
      "--format",
      "{{json .Config}}",
      imageId,
    ])
    const config = JSON.parse(inspected.stdout)

    assert.equal(config.User, "65532:65532")
    assert.deepEqual(config.Cmd, ["/usr/local/bin/neev-server"])
    assert.ok(
      config.Entrypoint == null,
      "the image must not override its native CMD"
    )
    assert.ok(config.Env.includes("NEEV_RUNTIME_ENABLED=false"))
    assert.ok(config.Env.includes("NEEV_STATIC_ROOT=/app/web"))
    assert.ok(config.Env.includes("NEEV_DOCUMENT_ROOT=/data/documents"))
    assert.ok(config.Env.includes("NEEV_BIND=0.0.0.0:3080"))

    const allowedImageEnv = [
      "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
      "NEEV_RUNTIME_ENABLED=false",
      "NEEV_STATIC_ROOT=/app/web",
      "NEEV_DOCUMENT_ROOT=/data/documents",
      "NEEV_BIND=0.0.0.0:3080",
    ]
    assert.deepEqual(
      [...config.Env].sort(),
      allowedImageEnv.sort(),
      "image contains baked secret, provider, origin, database, or legacy configuration"
    )
    assert.ok(
      config.Volumes == null || Object.keys(config.Volumes).length === 0,
      "image must not declare volumes"
    )
  }
)

test(
  "default native command fails closed without database, key, or origin",
  { skip: integrationSkip },
  () => {
    const imageId = resolveLocalImage()
    const containerName = uniqueContainerName("default")

    try {
      const result = docker(
        ["run", "--name", containerName, ...confinementArgs, imageId],
        { allowFailure: true }
      )

      assert.notEqual(
        result.status,
        null,
        "default command exceeded the Docker timeout"
      )
      assert.notEqual(
        result.status,
        0,
        "default command started without required configuration"
      )
      assert.ok(
        ![125, 126, 127].includes(result.status),
        `Docker could not execute the default command (${result.status}): ${result.stderr || result.stdout}`
      )
      assert.doesNotMatch(
        `${result.stdout}\n${result.stderr}`,
        /codeg-server|--supervise|operator server/i,
        "default command started or invoked the legacy operator server"
      )
    } finally {
      cleanupContainer(containerName)
    }
  }
)

test(
  "diagnostic: runtime contents and UID are confined",
  { skip: integrationSkip },
  () => {
    const imageId = resolveLocalImage()
    const containerName = uniqueContainerName("diagnostic")
    const diagnostic = [
      "set -eu",
      '[ "$(id -u)" = "65532" ]',
      "test -r /usr/local/bin/neev-server",
      "test -r /usr/local/bin/neev-content-verify",
      "test -r /app/web/index.html",
      rootWriteProbeCommand(),
      'for tool in node npm git ssh docker rustc cargo; do if command -v "$tool" >/dev/null 2>&1; then echo "unexpected tool: $tool" >&2; exit 1; fi; done',
      "ldd /usr/local/bin/neev-server /usr/local/bin/neev-content-verify > /tmp/neev-ldd.txt",
      "! grep -q 'not found' /tmp/neev-ldd.txt",
    ].join("\n")

    try {
      const result = docker(
        [
          "run",
          "--name",
          containerName,
          ...confinementArgs,
          "--entrypoint",
          "/bin/sh",
          imageId,
          "-c",
          diagnostic,
        ],
        { allowFailure: true }
      )

      assert.equal(
        result.status,
        0,
        `diagnostic failed (${result.status}): ${result.stderr || result.stdout}`
      )
    } finally {
      cleanupContainer(containerName)
    }
  }
)
