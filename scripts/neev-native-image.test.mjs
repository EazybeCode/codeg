import assert from "node:assert/strict"
import { randomUUID } from "node:crypto"
import { spawnSync } from "node:child_process"
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

function cleanupContainer(name) {
  docker(["container", "rm", "--force", name], { allowFailure: true })
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
      "! touch /neev-native-write-test",
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
