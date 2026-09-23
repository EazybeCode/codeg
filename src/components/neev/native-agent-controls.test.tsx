import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeAgentChats } from "./native-agent-chats"
import { NativeBoundary } from "./native-boundary"
import {
  AgentControls,
  reconcileAgentSettings,
  viewAgentControls,
} from "./native-agent-controls"
import {
  agentCatalog,
  json,
  mockNativeSession,
  NativeTestRoot,
} from "./test-helpers"
import { parseAgentCapabilities } from "@/lib/neev-agent-sessions-client"

vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
}))

// Parsed rather than hand-built: the controls must be exercised against the
// same object the client produces from the real response, not a convenient
// stand-in that skips its validation.
const capabilities = parseAgentCapabilities(agentCatalog)

const SESSION_ID = "11111111-1111-4111-8111-111111111111"

const session = {
  id: SESSION_ID,
  orgId: 3,
  ownerId: 1,
  title: "Refactor the parser",
  provider: "claude",
  model: "default",
  effort: null as string | null,
  fast: false,
  boundChatId: null,
  archived: false,
  revision: "1",
  createdAt: 1_700_000_000,
  updatedAt: 1_700_000_000,
}

const connected = {
  runtimeEnabled: true,
  connections: [
    {
      provider: "claude",
      state: "connected",
      accountLabel: "sagar@example.test",
      checkedAt: 1_700_000_000,
      errorCode: null,
    },
  ],
}

const TURNS = `GET /api/neev/agent/sessions/${SESSION_ID}/turns`
const SETTINGS = `PATCH /api/neev/agent/sessions/${SESSION_ID}/settings`
const SESSION = `GET /api/neev/agent/sessions/${SESSION_ID}`

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe("what the catalogue allows", () => {
  const settings = (model: string, effort: string | null, fast: boolean) => ({
    model,
    effort,
    fast,
  })

  it("offers the provider's levels, narrowed by a model that says so", () => {
    // `default` carries no list of its own, so the provider's applies.
    expect(
      viewAgentControls(
        capabilities,
        "claude",
        settings("default", null, false)
      ).efforts
    ).toEqual([null, "low", "medium", "high", "xhigh", "max"])
    // Sonnet does carry one, and it is the authority for Sonnet.
    expect(
      viewAgentControls(capabilities, "claude", settings("sonnet", null, false))
        .efforts
    ).toEqual([null, "low", "medium", "high"])
    // Levels differ per provider: Codex has `minimal` and no `max`.
    expect(
      viewAgentControls(capabilities, "codex", settings("default", null, false))
        .efforts
    ).toEqual([null, "minimal", "low", "medium", "high", "xhigh"])
  })

  it("leads with the provider default, because that is a choice", () => {
    const view = viewAgentControls(
      capabilities,
      "claude",
      settings("default", null, false)
    )
    // `null` is "pass no flag", not an absence, so it is first and it is real.
    expect(view.efforts[0]).toBeNull()
  })

  it("allows fast only for the selected model, and says which kind of no", () => {
    expect(
      viewAgentControls(capabilities, "claude", settings("opus", null, false))
        .speed
    ).toBe("none")
    // Claude has the control; Haiku does not. Fixable by choosing a model.
    expect(
      viewAgentControls(capabilities, "claude", settings("haiku", null, false))
        .speed
    ).toBe("model")
    // A model this catalogue does not list cannot be claimed to support speed.
    expect(
      viewAgentControls(capabilities, "claude", settings("gpt-9", null, false))
        .speed
    ).toBe("model")
  })

  it("turns fast off for a model that cannot run it, and never turns it on", () => {
    // Someone on Opus + Fast who moves to Haiku asked for two things. The
    // model they chose is kept and fast is dropped — the reverse would spend
    // money they did not agree to.
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("haiku", null, true)
      )
    ).toEqual({ model: "haiku", effort: null, fast: false })
    // And the other direction is never taken: moving to a model that COULD do
    // it does not switch it on.
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("opus", null, false)
      ).fast
    ).toBe(false)
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("opus", null, true)
      ).fast
    ).toBe(true)
  })

  it("drops a level the chosen model does not advertise", () => {
    // `max` is Claude's, but Sonnet narrows to low/medium/high.
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("sonnet", "max", false)
      )
    ).toEqual({ model: "sonnet", effort: null, fast: false })
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("sonnet", "high", false)
      ).effort
    ).toBe("high")
    // A Codex-only level cannot survive a move to Claude.
    expect(
      reconcileAgentSettings(
        capabilities,
        "claude",
        settings("default", "minimal", false)
      ).effort
    ).toBeNull()
  })

  it("corrects nothing when it has not been told anything", () => {
    // With no catalogue the server is the only validator. Inventing a
    // restriction here would refuse a combination it would have accepted.
    const asked = settings("opus", "max", true)
    expect(reconcileAgentSettings(null, "claude", asked)).toEqual(asked)
  })
})

function openSaved(routes: Record<string, () => Response>) {
  mockNativeSession(undefined, {
    "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
    "GET /api/neev/agent/connections": () => json(connected),
    [TURNS]: () => json({ items: [], nextAfter: null }),
    ...routes,
  })
  return render(
    <NativeTestRoot>
      <NativeBoundary>
        <NativeAgentChats sessionId={SESSION_ID} />
      </NativeBoundary>
    </NativeTestRoot>
  )
}

/** Open the one composer menu behind the model trigger. */
async function openMenu(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Model" }))
}

/**
 * Choose a model, wherever it lives.
 *
 * The menu leads with a shortlist; everything else is one submenu away. A test
 * about what gets STORED should not have to know which of the two a model is
 * in today, so this looks in both.
 */
async function pickModel(
  user: ReturnType<typeof userEvent.setup>,
  label: string
) {
  await openMenu(user)
  const short = await screen.findByRole("group", { name: "Claude Code" })
  const pinned = within(short).queryByRole("menuitemradio", { name: label })
  if (pinned) {
    await user.click(pinned)
    return
  }
  await user.click(screen.getByRole("menuitem", { name: "More models" }))
  const all = await screen.findByRole("menu", { name: "More models" })
  // `fireEvent`, not `userEvent`, and only here: Radix keeps a submenu open
  // using the pointer's position against the trigger's geometry, and in jsdom
  // every rect is zero — so userEvent's move-then-click closes the submenu
  // before the click lands. The keyboard path is covered separately.
  fireEvent.click(within(all).getByRole("menuitemradio", { name: label }))
}

/** The Fast row, which now lives inside the menu rather than beside it. */
async function fastRow(user: ReturnType<typeof userEvent.setup>) {
  await openMenu(user)
  return screen.getByRole("menuitemcheckbox", { name: "Fast" })
}

describe("more than one of these on a page", () => {
  it("gives each row its own description, so a chip never points at another chat's", () => {
    // Several composers are mounted at once — one per open tab. A hardcoded id
    // would make every Fast chip resolve to the FIRST row's description, so a
    // reader would be told about a model they are not looking at.
    render(
      <NativeTestRoot>
        <AgentControls
          capabilities={capabilities}
          provider="claude"
          settings={{ model: "haiku", effort: null, fast: false }}
          onChange={() => {}}
        />
        <AgentControls
          capabilities={capabilities}
          provider="codex"
          settings={{ model: "default", effort: null, fast: false }}
          onChange={() => {}}
        />
      </NativeTestRoot>
    )
    // The description lives beside the trigger, not inside the menu, so it
    // resolves whether or not the menu is open — and there is one per mounted
    // row.
    const described = screen
      .getAllByRole("button", { name: "Model" })
      .map((trigger) =>
        trigger
          .closest("div")!
          .querySelector<HTMLElement>("span.sr-only[id]")
          ?.getAttribute("id")
      )
    expect(described[0]).toBeTruthy()
    expect(described[0]).not.toBe(described[1])
    // Every id resolves, and to exactly one element.
    for (const id of described)
      expect(document.querySelectorAll(`[id="${id}"]`)).toHaveLength(1)
    // And each really describes its OWN row: Haiku cannot do fast mode, the
    // Codex default can.
    expect(document.getElementById(described[0]!)).toHaveTextContent(
      /not available for Haiku/
    )
    expect(document.getElementById(described[1]!)).toHaveTextContent(
      "Fast mode costs more per run."
    )
  })
})

describe("the controls on a saved agent chat", () => {
  it("shows what the session actually runs as, from the server's own row", async () => {
    // Restored on reload because it is read from the SessionDto, not from a
    // local selection that a refresh would have forgotten.
    mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () =>
        json({
          sessions: [
            { ...session, model: "opus", effort: "xhigh", fast: true },
          ],
        }),
      "GET /api/neev/agent/connections": () => json(connected),
      [TURNS]: () => json({ items: [], nextAfter: null }),
    })
    render(
      <NativeTestRoot>
        <NativeBoundary>
          <NativeAgentChats sessionId={SESSION_ID} />
        </NativeBoundary>
      </NativeTestRoot>
    )
    // The model's server-given label, the level's translated name, and the
    // speed state — all three, with nothing invented.
    // All three on the one trigger: the model's server-given label, the level's
    // translated name, and a mark that Fast is on — with nothing invented.
    const trigger = await screen.findByRole("button", { name: "Model" })
    expect(trigger).toHaveTextContent("Opus")
    expect(trigger).toHaveTextContent("Very high")
    const user = userEvent.setup()
    await user.click(trigger)
    expect(
      screen.getByRole("menuitemcheckbox", { name: "Fast" })
    ).toHaveAttribute("aria-checked", "true")
  })

  it("stores model, level and speed together in one revision", async () => {
    const patches: Record<string, unknown>[] = []
    const user = userEvent.setup()
    openSaved({
      [SETTINGS]: () => json({ ...session, model: "opus", revision: "2" }),
    })
    // Record bodies through the same fetch mock the helper installed.
    const fetcher = globalThis.fetch as unknown as ReturnType<typeof vi.fn>
    const original = fetcher.getMockImplementation()!
    fetcher.mockImplementation(async (path: string, init: RequestInit = {}) => {
      if (`${init.method ?? "GET"} ${path}` === SETTINGS)
        patches.push(JSON.parse(init.body as string))
      return original(path, init)
    })

    await pickModel(user, "Opus")
    await waitFor(() => expect(patches).toHaveLength(1))
    // The whole desired state, against the revision it was read at — never a
    // partial patch the server would have to guess the rest of.
    expect(patches[0]).toEqual({
      model: "opus",
      effort: null,
      fast: false,
      expectedRevision: "1",
    })
  })

  it("does not pass off an unread catalogue as a one-model provider", async () => {
    // A provider whose discovery failed lists only `default` — exactly what a
    // provider that genuinely has one model lists. Without saying so the menu
    // would claim Default is all there is, which the server never said.
    const user = userEvent.setup()
    mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
      "GET /api/neev/agent/connections": () => json(connected),
      [TURNS]: () => json({ items: [], nextAfter: null }),
      "GET /api/neev/agent/capabilities": () =>
        json({
          providers: [
            {
              ...agentCatalog.providers[0],
              models: [agentCatalog.providers[0].models[0]],
              discoveryUnavailableReason:
                "The runner could not be asked for its models.",
            },
          ],
        }),
    })
    render(
      <NativeTestRoot>
        <NativeBoundary>
          <NativeAgentChats sessionId={SESSION_ID} />
        </NativeBoundary>
      </NativeTestRoot>
    )
    await user.click(await screen.findByRole("button", { name: "Model" }))
    expect(
      await screen.findByText(
        "Claude Code could not list its models, so this may not be all of them."
      )
    ).toBeVisible()
    // The server's own English prose is never rendered; the reader gets a
    // translated sentence instead.
    expect(screen.queryByText(/runner could not be asked/)).toBeNull()
    // And what IS listed stays usable: a catalogue that could not be read must
    // not block a run that would have worked.
    expect(
      within(screen.getByRole("group", { name: "Claude Code" })).getByRole(
        "menuitemradio",
        { name: "Default" }
      )
    ).toBeEnabled()
  })

  it("says nothing about discovery when the catalogue was read", async () => {
    const user = userEvent.setup()
    openSaved({})
    await user.click(await screen.findByRole("button", { name: "Model" }))
    await screen.findByRole("group", { name: "Claude Code" })
    // Absent field = discovery succeeded. A note here would invent a doubt the
    // server never expressed.
    expect(screen.queryByText(/may not be all of them/)).toBeNull()
  })

  it("will not offer a different provider, because that is a different chat", async () => {
    const user = userEvent.setup()
    openSaved({})
    await openMenu(user)
    await user.click(screen.getByRole("menuitem", { name: "More models" }))
    // The session's own provider is there; the other is not, and the menu says
    // why rather than showing a disabled row with no explanation.
    const all = await screen.findByRole("menu", { name: "More models" })
    expect(
      within(all).getByRole("group", { name: "Claude Code" })
    ).toBeVisible()
    expect(within(all).queryByRole("group", { name: "Codex" })).toBeNull()
    expect(
      within(all).getByText(/Another provider is a new agent chat/)
    ).toBeVisible()
  })

  it("keeps fast off for a model that cannot run it, and says so", async () => {
    const user = userEvent.setup()
    openSaved({})
    await screen.findByRole("button", { name: "Model" })
    // Claude has the control, the default model does not: the row stays
    // visible and disabled with the reason, rather than vanishing.
    const fast = await fastRow(user)
    expect(fast).toHaveAttribute("aria-disabled", "true")
    // The reason rides on the row — as its description and its tooltip — not
    // as a standing paragraph under every composer that has one of these.
    const reason = (row: HTMLElement) =>
      document.getElementById(row.getAttribute("aria-describedby") ?? "")
        ?.textContent ?? ""
    expect(reason(fast)).toMatch(/Fast mode is not available for Default/)
    expect(fast).toHaveAttribute(
      "title",
      expect.stringMatching(/not available for Default/) as unknown as string
    )
    await user.keyboard("{Escape}")
    // Choosing Opus makes it possible — but does not switch it on.
    await pickModel(user, "Opus")
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
        "Opus"
      )
    )
    const enabled = await fastRow(user)
    expect(enabled).not.toHaveAttribute("aria-disabled", "true")
    // Standard is where it stays until someone chooses otherwise.
    expect(enabled).toHaveAttribute("aria-checked", "false")
    // And the cost of using it is stated before it is used — reachable, and
    // reachable without a permanent line of copy under the composer.
    expect(
      document.getElementById(enabled.getAttribute("aria-describedby") ?? "")
    ).toHaveTextContent("Fast mode costs more per run.")
    expect(enabled).toHaveAttribute("title", "Fast mode costs more per run.")
  })

  it("puts model, reasoning and speed behind one trigger, with nothing under it", async () => {
    const user = userEvent.setup()
    openSaved({})
    const trigger = await screen.findByRole("button", { name: "Model" })
    const row = trigger.parentElement!
    // One control on the row, and no explanatory paragraph beneath it: every
    // one of those now sits on the thing it explains.
    expect(
      [...row.children].filter((child) => child.tagName === "P")
    ).toHaveLength(0)
    expect(row.querySelectorAll("button")).toHaveLength(1)
    // And all three decisions are one click away, together.
    await user.click(trigger)
    expect(
      screen.getByRole("group", { name: "Claude Code" })
    ).toBeInTheDocument()
    expect(
      screen.getByRole("menuitem", { name: /Reasoning/ })
    ).toBeInTheDocument()
    expect(
      screen.getByRole("menuitemcheckbox", { name: "Fast" })
    ).toBeInTheDocument()
    expect(
      screen.getByRole("menuitem", { name: "Edit shortlist" })
    ).toBeInTheDocument()
  })

  it("opens from the keyboard, and reaches both submenus without a pointer", async () => {
    // One menu now holds three decisions, so each has to be reachable without
    // a pointer. Typeahead picks the row; ArrowRight opens what is behind it.
    const user = userEvent.setup()
    openSaved({})
    const trigger = await screen.findByRole("button", { name: "Model" })
    trigger.focus()
    await user.keyboard("{Enter}")
    await screen.findByRole("group", { name: "Claude Code" })

    await user.keyboard("Reason")
    await user.keyboard("{ArrowRight}")
    const levels = await screen.findByRole("menu", { name: "Reasoning" })
    expect(within(levels).getAllByRole("menuitemradio").length).toBeGreaterThan(
      1
    )

    await user.keyboard("{ArrowLeft}")
    await user.keyboard("Edit")
    await user.keyboard("{ArrowRight}")
    expect(
      await screen.findByRole("menu", { name: "Edit shortlist" })
    ).toBeInTheDocument()
  })

  it("toggles fast from the keyboard and leaves the menu open", async () => {
    // Speed is read against the model right above it, so a toggle must not
    // close the menu and send someone back to reopen it to compare.
    const user = userEvent.setup()
    openSaved({
      [SETTINGS]: () => json({ ...session, model: "opus", revision: "2" }),
    })
    await screen.findByRole("button", { name: "Model" })
    await pickModel(user, "Opus")
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
        "Opus"
      )
    )
    const fast = await fastRow(user)
    expect(fast).toHaveAttribute("aria-checked", "false")
    fireEvent.click(fast)
    expect(
      await screen.findByRole("menuitemcheckbox", { name: "Fast" })
    ).toBeInTheDocument()
  })

  it("pins a model on the first click, rather than removing it", async () => {
    // With nothing saved, the editor's ticks and the list a toggle starts from
    // were computed separately: three unticked rows, and ticking one wrote a
    // list that REMOVED it. They are one value now.
    const user = userEvent.setup()
    openSaved({})
    await user.click(await screen.findByRole("button", { name: "Model" }))
    await user.click(screen.getByRole("menuitem", { name: "Edit shortlist" }))
    const editor = await screen.findByRole("menu", { name: "Edit shortlist" })
    const boxes = within(editor).getAllByRole("menuitemcheckbox")
    // What the menu is showing is what the editor ticks.
    expect(
      boxes.map((box) => [box.textContent, box.getAttribute("aria-checked")])
    ).toEqual([
      ["Default", "true"],
      ["Haiku", "true"],
      ["Sonnet", "true"],
      ["Opus", "false"],
    ])
    fireEvent.click(
      within(editor).getByRole("menuitemcheckbox", { name: "Opus" })
    )
    await waitFor(() =>
      expect(
        within(screen.getByRole("menu", { name: "Edit shortlist" })).getByRole(
          "menuitemcheckbox",
          { name: "Opus" }
        )
      ).toHaveAttribute("aria-checked", "true")
    )
    // And a fourth pin really shows up in the menu it was pinned to — the
    // shortlist size caps the DEFAULT, never a list someone chose.
    await user.keyboard("{Escape}")
    await user.click(screen.getByRole("button", { name: "Model" }))
    expect(
      within(await screen.findByRole("group", { name: "Claude Code" }))
        .getAllByRole("menuitemradio")
        .map((item) => item.textContent)
    ).toEqual(["Default", "Haiku", "Sonnet", "Opus"])
  })

  it("re-reads and retries once when someone else changed the session first", async () => {
    const revisions: string[] = []
    let refused = false
    let reread = 0
    const user = userEvent.setup()
    mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
      "GET /api/neev/agent/connections": () => json(connected),
      [TURNS]: () => json({ items: [], nextAfter: null }),
      [SESSION]: () => {
        reread += 1
        // What is actually stored now: someone renamed it, so the revision
        // this view held is stale.
        return json({ ...session, title: "Renamed elsewhere", revision: "7" })
      },
      [SETTINGS]: (init: RequestInit) => {
        revisions.push(
          (JSON.parse(init.body as string) as { expectedRevision: string })
            .expectedRevision
        )
        if (!refused) {
          refused = true
          return json({ code: "conflict" }, 409)
        }
        return json({ ...session, model: "opus", revision: "8" })
      },
    })
    render(
      <NativeTestRoot>
        <NativeBoundary>
          <NativeAgentChats sessionId={SESSION_ID} />
        </NativeBoundary>
      </NativeTestRoot>
    )
    await pickModel(user, "Opus")
    // First against the revision it held, then against what is really there —
    // and exactly once more, never a loop against a moving target.
    await waitFor(() => expect(revisions).toEqual(["1", "7"]))
    expect(reread).toBe(1)
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Model" })).toHaveTextContent(
        "Opus"
      )
    )
  })

  it("does not send the prompt when its configuration could not be stored", async () => {
    const turns: unknown[] = []
    const user = userEvent.setup()
    mockNativeSession(undefined, {
      "GET /api/neev/agent/sessions": () => json({ sessions: [session] }),
      "GET /api/neev/agent/connections": () => json(connected),
      [TURNS]: () => json({ items: [], nextAfter: null }),
      // The pairing is refused outright, and a retry would be refused too.
      [SETTINGS]: () => json({ code: "invalid_request" }, 400),
      [`POST /api/neev/agent/sessions/${SESSION_ID}/turns`]: () => {
        turns.push(1)
        return json({ code: "unavailable" }, 503)
      },
    })
    render(
      <NativeTestRoot>
        <NativeBoundary>
          <NativeAgentChats sessionId={SESSION_ID} />
        </NativeBoundary>
      </NativeTestRoot>
    )
    await pickModel(user, "Opus")
    const box = await screen.findByRole("textbox", { name: "Ask the agent" })
    await user.type(box, "Summarise the parser")
    await user.click(screen.getByRole("button", { name: "Send" }))

    // A run reads the SESSION, so a turn started here would have run under a
    // configuration nobody chose. Nothing was sent...
    await waitFor(() =>
      expect(screen.getByText("Not applied yet.")).toBeVisible()
    )
    expect(turns).toHaveLength(0)
    // ...and the words the user typed are still exactly where they were.
    expect(box).toHaveValue("Summarise the parser")
  })
})
