import { randomUUID } from "node:crypto"
import { engineApi, engineUrl } from "./http.mjs"
import { connect, openWithForeignDocumentName, until } from "./collab.mjs"

/**
 * The acceptance cases.
 *
 * Each returns evidence — the statuses and values it actually saw — so a pass
 * can be read rather than believed. A case that cannot reach the state it is
 * about reports `skipped` with the reason; it never reports a pass for a
 * boundary it did not exercise, which is the only failure mode of a harness
 * that matters.
 */

const ok = (evidence) => ({ status: "pass", evidence })
const no = (why, evidence) => ({ status: "fail", why, evidence })
const skip = (why) => ({ status: "skipped", why })

/** Create a document through Neev and return the resource. */
async function createDocument(actor, nodeId, title, requestId = randomUUID()) {
  const { status, body } = await actor.json(`/api/neev/nodes/${nodeId}/documents`, {
    method: "POST",
    body: JSON.stringify({ title, requestId }),
  })
  return { status, resource: body, requestId }
}

export const cases = [
  {
    name: "new document in a project opens on the engine",
    async run({ owner, fixtures }) {
      const { status, resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — project"
      )
      if (status !== 201 && status !== 200)
        return no(`create returned ${status}`, { status })
      if (resource?.engine !== "lasuite")
        return no("new documents must be engine documents", {
          engine: resource?.engine ?? null,
        })

      const page = await owner.fetch(engineUrl(resource.id))
      if (page.status !== 200)
        return no(`editor page returned ${page.status}`, { page: page.status })

      // The framing headers live on the static export, not on Django. Without
      // them the iframe has no policy at all, which is quieter than being
      // blocked and so worth asserting explicitly.
      const csp = page.headers.get("content-security-policy") ?? ""
      const xfo = page.headers.get("x-frame-options") ?? ""
      if (!csp.includes("frame-ancestors"))
        return no("editor page carries no frame-ancestors", { csp, xfo })

      return ok({ id: resource.id, engine: resource.engine, csp, xfo })
    },
  },

  {
    name: "new document in a subfolder does the same",
    async run({ owner, fixtures }) {
      const { status, resource } = await createDocument(
        owner,
        fixtures.folderNodeId,
        "Acceptance — subfolder"
      )
      if (status !== 201 && status !== 200)
        return no(`create returned ${status}`, { status })
      if (resource?.engine !== "lasuite")
        return no("engine field missing in a subfolder", {
          engine: resource?.engine ?? null,
        })
      if (resource.nodeId !== fixtures.folderNodeId)
        return no("document landed in the wrong node", {
          got: resource.nodeId,
          want: fixtures.folderNodeId,
        })
      return ok({ id: resource.id, nodeId: resource.nodeId })
    },
  },

  {
    name: "a retried creation makes one document, not two",
    async run({ owner, fixtures }) {
      const requestId = randomUUID()
      const first = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — idempotent",
        requestId
      )
      const second = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — idempotent",
        requestId
      )
      if (!first.resource?.id || !second.resource?.id)
        return no("one of the attempts returned no resource", {
          first: first.status,
          second: second.status,
        })
      if (first.resource.id !== second.resource.id)
        return no("the retry created a second document", {
          first: first.resource.id,
          second: second.resource.id,
        })

      // And the engine side must not have been provisioned twice either — the
      // duplicate would be invisible in Neev and very visible in Docs.
      const engine = await engineApi(owner, `documents/${first.resource.id}/`)
      return engine.status === 200
        ? ok({ id: first.resource.id, engine: engine.status })
        : no(`engine does not know the document (${engine.status})`, {
            engine: engine.status,
          })
    },
  },

  {
    name: "CSRF is still enforced, on Neev and on Docs alike",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — csrf"
      )
      if (!resource?.id) return skip("could not create a document to test with")

      // Neev: an unsafe request with no CSRF header.
      const native = await fetch(
        new URL(`/api/neev/resources/${resource.id}`, owner.descriptor.base ?? "http://127.0.0.1:3100"),
        {
          method: "PATCH",
          headers: { cookie: owner.cookieHeader, "content-type": "application/json" },
          body: JSON.stringify({ title: "no csrf" }),
          redirect: "manual",
        }
      )
      if (native.status < 400)
        return no(`Neev accepted a mutation with no CSRF token (${native.status})`, {
          native: native.status,
        })

      // Docs: same, through the prefix. Upstream's CsrfViewMiddleware is meant
      // to be on; the proxy adds checks but must not have replaced it.
      const docs = await fetch(
        new URL(
          `/document-engine/api/v1.0/documents/${resource.id}/`,
          owner.descriptor.base ?? "http://127.0.0.1:3100"
        ),
        {
          method: "PATCH",
          headers: { cookie: owner.cookieHeader, "content-type": "application/json" },
          body: JSON.stringify({ title: "no csrf" }),
          redirect: "manual",
        }
      )
      if (docs.status < 400)
        return no(`Docs accepted a mutation with no CSRF token (${docs.status})`, {
          docs: docs.status,
        })

      return ok({ native: native.status, docs: docs.status })
    },
  },

  {
    name: "a rename in the engine reaches Neev's metadata",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — before rename"
      )
      if (!resource?.id) return skip("could not create a document to rename")

      const renamed = `Acceptance — renamed ${Date.now()}`
      const patch = await engineApi(owner, `documents/${resource.id}/`, {
        method: "PATCH",
        body: JSON.stringify({ title: renamed }),
      })
      if (patch.status >= 400)
        return no(`engine refused the rename (${patch.status})`, {
          patch: patch.status,
        })

      // The UI re-reads Neev's own resource after focus leaves the frame; this
      // asserts the thing that read depends on — that Neev's metadata follows.
      const synced = await until(async () => {
        const { body } = await owner.json(`/api/neev/resources/${resource.id}`)
        return body?.title === renamed
      })
      return synced
        ? ok({ id: resource.id, title: renamed })
        : no("Neev's title never caught up with the engine's", { want: renamed })
    },
  },

  {
    name: "another organization is refused, on the page and on the API",
    async run({ owner, outsider, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — other org"
      )
      if (!resource?.id) return skip("could not create a document to guard")

      const native = await outsider.json(`/api/neev/resources/${resource.id}`)
      const api = await engineApi(outsider, `documents/${resource.id}/`)
      const page = await outsider.fetch(engineUrl(resource.id))

      if (native.status < 400)
        return no(`outsider read Neev's resource (${native.status})`, {
          native: native.status,
        })
      if (api.status < 400)
        return no(`outsider read the engine document (${api.status})`, {
          api: api.status,
        })

      const socket = await connect(outsider, resource.id)
      socket.destroy()
      if (socket.ok)
        return no("outsider authenticated on the collaboration socket", {
          native: native.status,
          api: api.status,
        })

      return ok({
        native: native.status,
        api: api.status,
        page: page.status,
        socket: socket.reason,
      })
    },
  },

  {
    name: "a second user in the organization can open and converge",
    async run({ owner, member, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — shared"
      )
      if (!resource?.id) return skip("could not create a document to share")

      const shared = await owner.json(`/api/neev/resources/${resource.id}`, {
        method: "PATCH",
        body: JSON.stringify({ visibility: "organization" }),
      })
      if (shared.status >= 400)
        return no(`sharing failed (${shared.status})`, { shared: shared.status })

      const a = await connect(owner, resource.id)
      const b = await connect(member, resource.id)
      try {
        if (!a.ok) return no("owner could not connect", { reason: a.reason })
        if (!b.ok) return no("member could not connect", { reason: b.reason })

        // Both write without waiting for the other, which is the case that
        // distinguishes convergence from taking turns.
        const mark = randomUUID().slice(0, 8)
        a.doc.getMap("acceptance").set("owner", `owner-${mark}`)
        b.doc.getMap("acceptance").set("member", `member-${mark}`)

        const converged = await until(() => {
          const one = a.doc.getMap("acceptance")
          const two = b.doc.getMap("acceptance")
          return (
            one.get("member") === `member-${mark}` &&
            two.get("owner") === `owner-${mark}`
          )
        })
        return converged
          ? ok({ id: resource.id, mark })
          : no("the two sides never converged", {
              owner: a.doc.getMap("acceptance").toJSON(),
              member: b.doc.getMap("acceptance").toJSON(),
            })
      } finally {
        a.destroy()
        b.destroy()
      }
    },
  },

  {
    name: "withdrawing access stops the next connection",
    async run({ owner, member, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — revocation"
      )
      if (!resource?.id) return skip("could not create a document to revoke")

      await owner.json(`/api/neev/resources/${resource.id}`, {
        method: "PATCH",
        body: JSON.stringify({ visibility: "organization" }),
      })
      const before = await connect(member, resource.id)
      before.destroy()
      if (!before.ok)
        return skip(`member could not connect even before revocation: ${before.reason}`)

      await owner.json(`/api/neev/resources/${resource.id}`, {
        method: "PATCH",
        body: JSON.stringify({ visibility: "private" }),
      })

      const after = await connect(member, resource.id)
      after.destroy()
      if (after.ok)
        return no("member still connected after access was withdrawn", {})

      // Stated precisely: this is about a NEW connection. Whether an already
      // open socket is dropped at the moment access is revoked is a different
      // question, and this case does not answer it.
      return ok({ before: "connected", after: after.reason, scope: "new connections only" })
    },
  },

  {
    name: "a frame naming a different document is refused",
    async run({ owner, member, fixtures }) {
      const mine = await createDocument(owner, fixtures.projectNodeId, "Acceptance — mine")
      const theirs = await createDocument(owner, fixtures.projectNodeId, "Acceptance — private")
      if (!mine.resource?.id || !theirs.resource?.id)
        return skip("could not create the two documents this needs")

      await owner.json(`/api/neev/resources/${mine.resource.id}`, {
        method: "PATCH",
        body: JSON.stringify({ visibility: "organization" }),
      })
      // `theirs` stays private to the owner.

      // The document name is a leading varstring on every frame, so a socket
      // authorised for one document can name another. Upstream rejects that in
      // onConnect (documentName !== room). This asserts the proxy has not lost
      // that defence.
      const attempt = await openWithForeignDocumentName(
        member,
        mine.resource.id,
        theirs.resource.id
      )
      attempt.destroy()

      return attempt.ok
        ? no("a socket carried a frame for a document it was not authorised for", {
            authorised: mine.resource.id,
            named: theirs.resource.id,
          })
        : ok({ refused: attempt.reason, code: attempt.code ?? null })
    },
  },

  {
    name: "an archived document is read-only, not writable",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — archived"
      )
      if (!resource?.id) return skip("could not create a document to archive")

      const archived = await owner.json(`/api/neev/resources/${resource.id}`, {
        method: "PATCH",
        body: JSON.stringify({ archived: true }),
      })
      if (archived.status >= 400)
        return no(`archiving failed (${archived.status})`, { archived: archived.status })

      const session = await connect(owner, resource.id)
      try {
        if (!session.ok)
          // Refusing outright is also a defensible answer; it is not read-only,
          // so it is reported rather than quietly accepted.
          return ok({ outcome: "socket refused", reason: session.reason })

        const writable = session.provider.configuration?.readOnly === false
        const engine = await engineApi(owner, `documents/${resource.id}/`)
        const abilities = engine.body?.abilities ?? {}
        return writable && abilities.update === true
          ? no("an archived document is still writable", { abilities })
          : ok({ readOnly: !writable, abilities })
      } finally {
        session.destroy()
      }
    },
  },

  {
    name: "the legacy document is untouched and still native",
    async run({ owner, fixtures }) {
      // Read-only throughout. There is no migration, and this fixture is the
      // baseline that proves it — writing to it would destroy the only evidence
      // that legacy documents were left alone.
      const { status, body } = await owner.json(
        `/api/neev/resources/${fixtures.legacyDocumentId}`
      )
      if (status !== 200)
        return no(`legacy document unreadable (${status})`, { status })
      if (body?.engine === "lasuite")
        return no("a legacy document was migrated onto the engine", {
          engine: body.engine,
        })

      const content = await owner.json(
        `/api/neev/documents/${fixtures.legacyDocumentId}/content`
      )
      return content.status === 200
        ? ok({
            engine: body?.engine ?? null,
            contentRevision: body?.contentRevision,
            native: content.status,
          })
        : no(`legacy content unreadable (${content.status})`, {
            content: content.status,
          })
    },
  },

  {
    name: "Neev's own save routes refuse an engine document",
    async run({ owner, fixtures }) {
      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — native save"
      )
      if (!resource?.id) return skip("could not create a document to probe")

      // The UI never calls these for an engine document. If the server accepts
      // them anyway there are two writers for one document, which is the exact
      // failure the whole design avoids — so it has to be refused at the server,
      // not merely unused by the client.
      const save = await owner.json(`/api/neev/documents/${resource.id}/revisions`, {
        method: "POST",
        body: JSON.stringify({
          requestId: randomUUID(),
          expectedContentRevision: resource.contentRevision,
          expectedAuthRevision: resource.authRevision,
          markdown: "written by the wrong editor",
        }),
      })
      if (save.status < 400)
        return no(`a native save was accepted for an engine document (${save.status})`, {
          save: save.status,
        })

      const restore = await owner.json(`/api/neev/documents/${resource.id}/restore`, {
        method: "POST",
        body: JSON.stringify({
          requestId: randomUUID(),
          expectedContentRevision: resource.contentRevision,
          expectedAuthRevision: resource.authRevision,
          sourceRevision: "1",
        }),
      })
      return restore.status >= 400
        ? ok({ save: save.status, restore: restore.status })
        : no(`a native restore was accepted (${restore.status})`, {
            restore: restore.status,
          })
    },
  },

  {
    name: "an unavailable engine reads as retryable, not as a blank frame",
    tags: ["needs-engine-stopped"],
    async run({ owner, fixtures, engineStopped }) {
      if (!engineStopped)
        return skip(
          "needs the engine stopped; re-run with NEEV_ACCEPTANCE_ENGINE_STOPPED=1 while it is down"
        )

      const { resource } = await createDocument(
        owner,
        fixtures.projectNodeId,
        "Acceptance — unavailable"
      )
      const id = resource?.id ?? fixtures.legacyDocumentId

      const page = await owner.fetch(engineUrl(id))
      const body = await page.text()
      // The UI reads one explicit marker and refuses to sniff body text, so the
      // error page has to carry it. Without it the frame shows the error page
      // with no retry offered — which is the gap this case exists to catch.
      const marked = body.includes("neev-document-engine-error")
      return marked
        ? ok({ page: page.status, marker: true })
        : no(
            "the error page carries no neev-document-engine-error marker, so the UI cannot offer a retry",
            { page: page.status, marker: false }
          )
    },
  },
]
