import { describe, it, expect, vi, beforeEach } from "vitest"
import {
  listAgentSessionFiles,
  listResourceChatFiles,
  isSupportedType,
  formatFileSize,
} from "./neev-session-files"
import * as sessionsClient from "./neev-agent-sessions-client"
import * as contentClient from "./neev-content-client"
import type { NeevAttachmentType } from "./neev-attachments"

vi.mock("./neev-agent-sessions-client", () => ({
  listAgentTurns: vi.fn(),
}))

vi.mock("./neev-content-client", () => ({
  listNativeMessages: vi.fn(),
}))

const SESSION_ID = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
const CHAT_ID = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"

function makeTurn(
  sequence: string,
  attachments: Array<{
    key: string
    fileName: string
    contentType?: NeevAttachmentType
    byteLength?: number
  }>,
  options?: { prompt?: string; response?: string }
): sessionsClient.NeevAgentTurn {
  return {
    id: `turn-${sequence}`,
    sessionId: SESSION_ID,
    sequence,
    runId: "00000000000000000000000000000001",
    prompt: options?.prompt ?? "user prompt",
    response: options?.response ?? "",
    activity: [],
    inputs: [],
    supportsSteering: false,
    status: "completed",
    errorCode: null,
    provider: "claude",
    model: "default",
    attachments: attachments.map((a) => ({
      key: a.key,
      fileName: a.fileName,
      contentType: a.contentType ?? ("text/plain" as const),
      byteLength: a.byteLength ?? 100,
    })),
    createdAt: Date.now(),
    updatedAt: Date.now(),
    completedAt: Date.now(),
  }
}

function makeMessage(
  sequence: string,
  authorKind: "human" | "agent",
  attachments: Array<{
    key: string
    fileName: string
    contentType?: NeevAttachmentType
    byteLength?: number
  }>
): contentClient.NativeMessage {
  return {
    id: `msg-${sequence}`,
    sequence,
    body: "message body",
    authorId: 1,
    authorName: authorKind === "agent" ? "Claude" : "User",
    createdAt: Date.now(),
    authorKind,
    agentLabel: authorKind === "agent" ? "claude" : null,
    attachments: attachments.map((a) => ({
      key: a.key,
      fileName: a.fileName,
      contentType: a.contentType ?? ("text/plain" as const),
      byteLength: a.byteLength ?? 100,
    })),
  }
}

describe("listAgentSessionFiles", () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it("returns empty array for session with no turns", async () => {
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files).toEqual([])
  })

  it("includes attachments referenced in the response via URL (agent outputs)", async () => {
    // Turn where the response references attachment via server URL
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [
        makeTurn("1", [{ key: "output-key", fileName: "output.txt" }], {
          response: "Here is the file /api/neev/attachments/output-key",
        }),
      ],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files).toHaveLength(1)
    expect(files[0].key).toBe("output-key")
    expect(files[0].fileName).toBe("output.txt")
  })

  it("excludes attachments only referenced in prompt (human inputs)", async () => {
    // Turn where prompt has the attachment key but response doesn't
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [
        makeTurn("1", [{ key: "input-key", fileName: "input.txt" }], {
          prompt: "Please analyze /api/neev/attachments/input-key",
          response: "I analyzed the file and found nothing interesting.",
        }),
      ],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    // No files should be returned because the attachment is a human input
    expect(files).toEqual([])
  })

  it("handles mixed input and output attachments", async () => {
    // Turn with both human input (in prompt) and agent output (in response)
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [
        makeTurn(
          "1",
          [
            { key: "input-key", fileName: "input.txt" },
            { key: "output-key", fileName: "result.txt" },
          ],
          {
            prompt: "Process input-key file",
            response: "Done. See /api/neev/attachments/output-key",
          }
        ),
      ],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files).toHaveLength(1)
    expect(files[0].key).toBe("output-key")
  })

  it("deduplicates files by key across turns", async () => {
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [
        makeTurn("1", [{ key: "shared-key", fileName: "file.txt" }], {
          response: "/api/neev/attachments/shared-key",
        }),
        makeTurn(
          "2",
          [
            { key: "shared-key", fileName: "file.txt" }, // duplicate
            { key: "unique-key", fileName: "other.txt" },
          ],
          {
            response:
              "/api/neev/attachments/shared-key and /api/neev/attachments/unique-key",
          }
        ),
      ],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files).toHaveLength(2)
    expect(files.map((f) => f.key)).toEqual(["shared-key", "unique-key"])
    // First occurrence kept (from turn 1)
    expect(files[0].sequence).toBe("1")
  })

  it("paginates through multiple pages", async () => {
    const listAgentTurns = vi.mocked(sessionsClient.listAgentTurns)
    listAgentTurns
      .mockResolvedValueOnce({
        items: [
          makeTurn("1", [{ key: "key-1", fileName: "page1.txt" }], {
            response: "/api/neev/attachments/key-1",
          }),
        ],
        nextAfter: "1",
      })
      .mockResolvedValueOnce({
        items: [
          makeTurn("2", [{ key: "key-2", fileName: "page2.txt" }], {
            response: "/api/neev/attachments/key-2",
          }),
        ],
        nextAfter: null,
      })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files).toHaveLength(2)
    expect(listAgentTurns).toHaveBeenCalledTimes(2)
  })

  it("sorts files by turn sequence ascending", async () => {
    vi.mocked(sessionsClient.listAgentTurns).mockResolvedValue({
      items: [
        makeTurn("10", [{ key: "key-10", fileName: "ten.txt" }], {
          response: "/api/neev/attachments/key-10",
        }),
        makeTurn("2", [{ key: "key-2", fileName: "two.txt" }], {
          response: "/api/neev/attachments/key-2",
        }),
        makeTurn("1", [{ key: "key-1", fileName: "one.txt" }], {
          response: "/api/neev/attachments/key-1",
        }),
      ],
      nextAfter: null,
    })

    const files = await listAgentSessionFiles(SESSION_ID)
    expect(files.map((f) => f.sequence)).toEqual(["1", "2", "10"])
  })

  it("respects abort signal", async () => {
    const controller = new AbortController()
    controller.abort()

    const files = await listAgentSessionFiles(SESSION_ID, controller.signal)
    expect(files).toEqual([])
    expect(sessionsClient.listAgentTurns).not.toHaveBeenCalled()
  })
})

describe("listResourceChatFiles", () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it("returns empty array for chat with no messages", async () => {
    vi.mocked(contentClient.listNativeMessages).mockResolvedValue({
      items: [],
      nextAfter: null,
    })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files).toEqual([])
  })

  it("includes attachments from agent-authored messages only", async () => {
    vi.mocked(contentClient.listNativeMessages).mockResolvedValue({
      items: [
        makeMessage("1", "human", [
          { key: "human-upload", fileName: "upload.txt" },
        ]),
        makeMessage("2", "agent", [
          { key: "agent-output", fileName: "result.txt" },
        ]),
      ],
      nextAfter: null,
    })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files).toHaveLength(1)
    expect(files[0].key).toBe("agent-output")
    expect(files[0].fileName).toBe("result.txt")
  })

  it("excludes human-uploaded attachments", async () => {
    vi.mocked(contentClient.listNativeMessages).mockResolvedValue({
      items: [
        makeMessage("1", "human", [
          { key: "upload-1", fileName: "image.png", contentType: "image/png" },
          {
            key: "upload-2",
            fileName: "doc.pdf",
            contentType: "application/pdf",
          },
        ]),
      ],
      nextAfter: null,
    })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files).toEqual([])
  })

  it("deduplicates by key across messages", async () => {
    vi.mocked(contentClient.listNativeMessages).mockResolvedValue({
      items: [
        makeMessage("1", "agent", [
          { key: "shared-key", fileName: "file.txt" },
        ]),
        makeMessage("2", "agent", [
          { key: "shared-key", fileName: "file.txt" },
          { key: "unique-key", fileName: "other.txt" },
        ]),
      ],
      nextAfter: null,
    })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files).toHaveLength(2)
    expect(files.map((f) => f.key)).toEqual(["shared-key", "unique-key"])
  })

  it("paginates through multiple pages", async () => {
    const listNativeMessages = vi.mocked(contentClient.listNativeMessages)
    listNativeMessages
      .mockResolvedValueOnce({
        items: [
          makeMessage("1", "agent", [{ key: "key-1", fileName: "p1.txt" }]),
        ],
        nextAfter: "1",
      })
      .mockResolvedValueOnce({
        items: [
          makeMessage("2", "agent", [{ key: "key-2", fileName: "p2.txt" }]),
        ],
        nextAfter: null,
      })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files).toHaveLength(2)
    expect(listNativeMessages).toHaveBeenCalledTimes(2)
    expect(listNativeMessages).toHaveBeenNthCalledWith(
      1,
      CHAT_ID,
      null,
      undefined
    )
    expect(listNativeMessages).toHaveBeenNthCalledWith(
      2,
      CHAT_ID,
      "1",
      undefined
    )
  })

  it("sorts files by message sequence ascending", async () => {
    vi.mocked(contentClient.listNativeMessages).mockResolvedValue({
      items: [
        makeMessage("10", "agent", [{ key: "key-10", fileName: "ten.txt" }]),
        makeMessage("2", "agent", [{ key: "key-2", fileName: "two.txt" }]),
      ],
      nextAfter: null,
    })

    const files = await listResourceChatFiles(CHAT_ID)
    expect(files.map((f) => f.sequence)).toEqual(["2", "10"])
  })

  it("respects abort signal", async () => {
    const controller = new AbortController()
    controller.abort()

    const files = await listResourceChatFiles(CHAT_ID, controller.signal)
    expect(files).toEqual([])
    expect(contentClient.listNativeMessages).not.toHaveBeenCalled()
  })
})

describe("isSupportedType", () => {
  it("accepts supported image types", () => {
    expect(isSupportedType("image/png")).toBe(true)
    expect(isSupportedType("image/jpeg")).toBe(true)
    expect(isSupportedType("image/webp")).toBe(true)
  })

  it("accepts PDF files", () => {
    expect(isSupportedType("application/pdf")).toBe(true)
  })

  it("accepts plain text (includes HTML served as text/plain)", () => {
    expect(isSupportedType("text/plain")).toBe(true)
  })

  it("rejects unknown types", () => {
    expect(isSupportedType("application/octet-stream")).toBe(false)
    expect(isSupportedType("video/mp4")).toBe(false)
    expect(isSupportedType("text/html")).toBe(false) // raw HTML, not text/plain
  })
})

describe("formatFileSize", () => {
  it("formats bytes", () => {
    expect(formatFileSize(0)).toBe("0 B")
    expect(formatFileSize(512)).toBe("512 B")
    expect(formatFileSize(1023)).toBe("1023 B")
  })

  it("formats kilobytes", () => {
    expect(formatFileSize(1024)).toBe("1 KB")
    expect(formatFileSize(1536)).toBe("2 KB")
    expect(formatFileSize(1024 * 500)).toBe("500 KB")
  })

  it("formats megabytes", () => {
    expect(formatFileSize(1024 * 1024)).toBe("1.0 MB")
    expect(formatFileSize(1024 * 1024 * 2.5)).toBe("2.5 MB")
  })
})
