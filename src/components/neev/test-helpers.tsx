import { act, fireEvent } from "@testing-library/react"
import { NextIntlClientProvider } from "next-intl"
import { vi } from "vitest"
import messages from "@/i18n/messages/en.json"
import { NativeAuthProvider } from "./native-auth-provider"
import type { NativeSession } from "@/lib/neev-client"

export const nativeCapabilities = {
  version: 1,
  identityMode: "native",
  runtimeEnabled: false,
  providers: { google: true, email: true, github: false },
  catalog: {
    nodeKinds: ["project", "folder"],
    repositoryDefaults: false,
    chats: true,
    documents: true,
  },
} as const
export const orgSession: Extract<NativeSession, { scope: "organization" }> = {
  user: { id: 1, displayName: "Sagar", avatarUrl: null },
  organization: { id: 3, name: "Neev demo", avatarUrl: null },
  role: "admin",
  scope: "organization",
  csrf: "synthetic-csrf",
}
export const identitySession: NativeSession = {
  ...orgSession,
  organization: null,
  role: null,
  scope: "identity",
}
/**
 * `GET /api/neev/agent/capabilities` exactly as the deployed runner answers it
 * (Claude Code 2.1.267, codex-cli 0.155.1), field for field.
 *
 * Copied from the backend contract rather than simplified: the controls refuse
 * a payload that does not match the documented shape, so a fixture that took
 * liberties would pass tests the real response fails. It carries the two cases
 * that matter — a model whose `efforts` narrow the provider's, and a provider
 * where only one model supports speed.
 */
export const agentCatalog = {
  providers: [
    {
      provider: "claude",
      label: "Claude Code",
      models: [
        {
          id: "default",
          label: "Default",
          default: true,
          supportsSpeed: false,
        },
        { id: "haiku", label: "Haiku", default: false, supportsSpeed: false },
        {
          id: "sonnet",
          label: "Sonnet",
          default: false,
          supportsSpeed: false,
          efforts: ["low", "medium", "high"],
        },
        {
          id: "opus",
          label: "Opus",
          default: false,
          supportsSpeed: true,
          efforts: null,
        },
      ],
      efforts: ["low", "medium", "high", "xhigh", "max"],
      defaultEffort: null,
      supportsSpeed: true,
      speedUnavailableReason:
        "Fast mode runs on Opus. Choose Opus to enable it.",
    },
    {
      provider: "codex",
      label: "Codex",
      models: [
        { id: "default", label: "Default", default: true, supportsSpeed: true },
      ],
      efforts: ["minimal", "low", "medium", "high", "xhigh"],
      defaultEffort: null,
      supportsSpeed: true,
      speedUnavailableReason: "Fast mode is not available for this model.",
    },
  ],
} as const

export type Handler = (init: RequestInit) => Response | Promise<Response>
export function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  })
}
export function mockNativeSession(
  session: NativeSession | null = orgSession,
  routes: Record<string, Handler> = {}
) {
  const unknown: string[] = []
  const fetcher = vi.fn(async (path: string, init: RequestInit = {}) => {
    const key = `${init.method ?? "GET"} ${path}`
    if (routes[key]) return routes[key](init)
    if (key === "GET /api/workos/capabilities") return json(nativeCapabilities)
    if (key === "GET /api/neev/session")
      return json(session ?? { code: "unauthorized" }, session ? 200 : 401)
    // The Settings page always mounts the join-code panel; default to "no code
    // yet" so unrelated Settings tests need not mock it (tests can still override).
    // Every folder page reads its setting for new items (schema 21). Default
    // to what an untouched folder answers: nothing set anywhere, so private.
    const sharing = /^GET \/api\/neev\/nodes\/(\d+)\/sharing$/.exec(key)
    if (sharing)
      return json({
        nodeId: Number(sharing[1]),
        revision: "1",
        defaultVisibility: "inherit",
        effectiveVisibility: "private",
        inheritedFrom: null,
        canManage: true,
      })
    // A folder's access (v2): default to an open folder the reader can edit,
    // so tests about other things need not mock it.
    const nodeAccess = /^GET \/api\/neev\/nodes\/(\d+)\/access$/.exec(key)
    if (nodeAccess)
      return json({
        nodeId: Number(nodeAccess[1]),
        revision: "1",
        canManage: true,
        canRestrict: true,
        restricted: false,
        level: "edit",
        direct: [],
        inherited: [],
      })
    if (key === "GET /api/neev/shared/resources") return json({ resources: [] })
    // The workspace sidebar lists Agent Chats shared with the reader; default
    // to none so unrelated workspace tests need not mock it.
    if (key === "GET /api/neev/agent/shared-sessions")
      return json({ sessions: [] })
    if (key === "GET /api/neev/organization/join-code")
      return json({ code: null, disabled: false })
    // Settings always mounts the per-user agent connection panel; default to
    // "no runtime bound" so unrelated Settings tests need not mock it.
    if (key === "GET /api/neev/agent/connections")
      return json({
        runtimeEnabled: false,
        connections: [
          {
            provider: "claude",
            state: "disabled",
            accountLabel: null,
            checkedAt: null,
            errorCode: null,
          },
        ],
      })
    // Every agent composer reads the catalogue to render its controls. Default
    // to the real deployed shape so unrelated tests need not mock it; a test
    // about the controls themselves overrides it with the case it is about.
    if (key === "GET /api/neev/agent/capabilities") return json(agentCatalog)
    // Auto routing is off unless a test configures a router.
    if (key === "GET /api/neev/agent/routing")
      return json({
        state: "off",
        router: null,
        maxTaskChars: 6000,
        maxPreviousChars: 2000,
        supervision: "off",
        usage: [],
      })
    // The workspace always mounts the mentions indicator; default to an empty
    // inbox so unrelated tests need not mock it. A test about being named
    // overrides it with the notices it is about.
    if (key === "GET /api/neev/mentions") return json({ items: [], unread: 0 })
    // The trash view lists trashed projects and folders (schema 23); default to
    // none so unrelated trash tests need not mock it.
    if (key === "GET /api/neev/trash/nodes") return json({ items: [] })
    unknown.push(key)
    throw new Error(`Unexpected fetch: ${key}`)
  })
  vi.stubGlobal("fetch", fetcher)
  return { fetcher, unknown }
}
export function NativeTestRoot({ children }: { children: React.ReactNode }) {
  return (
    <NextIntlClientProvider locale="en" messages={messages}>
      <NativeAuthProvider>{children}</NativeAuthProvider>
    </NextIntlClientProvider>
  )
}

/**
 * Type into the inline composer, where the caret would be.
 *
 * `userEvent.type` drives a contenteditable by dispatching keys and letting the
 * browser do the editing, which jsdom does not — the characters arrive out of
 * order. ProseMirror reads its own DOM back, so the honest way to type here is
 * to put the text where it belongs and say the DOM changed. `where` is which
 * side of what is already there: "start" is typing BEFORE an attachment,
 * "end" is typing after it.
 */
export async function typeInComposer(
  box: HTMLElement,
  text: string,
  where: "start" | "end" = "end"
) {
  const paragraph = writableParagraph(box)
  await act(async () => {
    const node = document.createTextNode(text)
    if (where === "start") paragraph.insertBefore(node, paragraph.firstChild)
    else paragraph.appendChild(node)
    // Leave the caret after what was just typed, the way typing does — the next
    // file dropped in lands after these words rather than before them.
    const range = document.createRange()
    range.setStart(node, text.length)
    range.collapse(true)
    const selection = box.ownerDocument.getSelection()
    selection?.removeAllRanges()
    selection?.addRange(range)
    fireEvent.input(paragraph, { inputType: "insertText", data: text })
  })
}

/** The words in an inline composer, the way a `<textarea>`'s value used to be. */
export function composerText(box: HTMLElement): string {
  return box.textContent ?? ""
}

/**
 * Whether the composer will take typing.
 *
 * A contenteditable is not a form control, so it is not `disabled` — it stops
 * being editable. Same fact, said the way the DOM says it.
 */
export function composerEditable(box: HTMLElement): boolean {
  return box.getAttribute("contenteditable") === "true"
}

/**
 * Replace everything in an inline composer.
 *
 * The blunt instrument, for tests about a draft's lifetime rather than about
 * the composer: it does not respect attachment nodes, because the drafts these
 * tests write have none.
 */
export async function setComposerText(box: HTMLElement, text: string) {
  const paragraph = writableParagraph(box)
  await act(async () => {
    paragraph.textContent = text
    fireEvent.input(paragraph, { inputType: "insertText", data: text })
  })
}

/**
 * Where text goes in an editor.
 *
 * A cleared editor can be left with no paragraph at all; putting one back is
 * what a browser would do on the next keystroke, and the editor re-parses it.
 */
function writableParagraph(box: HTMLElement): HTMLElement {
  const existing = box.querySelector("p")
  if (existing) return existing
  const paragraph = box.ownerDocument.createElement("p")
  box.appendChild(paragraph)
  return paragraph
}
