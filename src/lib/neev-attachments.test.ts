import { afterEach, describe, expect, it, vi } from "vitest"
import { NativeClientError } from "./neev-client"
import {
  admitFiles,
  attachmentUrl,
  canonicalAttachmentName,
  classifyFile,
  dragCarriesFiles,
  filesFromDrop,
  filesFromPaste,
  formatBytes,
  parseAttachment,
  parseAttachments,
  pasteIsFileOnly,
  sniffAttachment,
  uploadChatAttachment,
  uploadRejection,
  uploadSessionAttachment,
} from "./neev-attachments"

const PNG = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]
const CHAT = "11111111-1111-4111-8111-111111111111"
const KEY = "22222222-2222-2222-2222-222222222222"

function file(name: string, bytes: number[] | string, type = ""): File {
  const body =
    typeof bytes === "string" ? [bytes] : [new Uint8Array(bytes).buffer]
  return new File(body, name, { type })
}

/** A file of a stated size without allocating it; only `size` is read. */
function sized(name: string, size: number): File {
  const value = file(name, "x")
  Object.defineProperty(value, "size", { value: size })
  return value
}

afterEach(() => vi.unstubAllGlobals())

describe("sniffing", () => {
  it("classifies by magic bytes exactly as the server does", () => {
    expect(sniffAttachment(new Uint8Array(PNG))).toBe("image/png")
    expect(sniffAttachment(new Uint8Array([0xff, 0xd8, 0xff, 0x00]))).toBe(
      "image/jpeg"
    )
    expect(
      sniffAttachment(
        new Uint8Array([
          0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50,
        ])
      )
    ).toBe("image/webp")
    expect(
      sniffAttachment(new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31]))
    ).toBe("application/pdf")
  })

  it("takes UTF-8 text, including source code, and nothing binary", () => {
    expect(
      sniffAttachment(new TextEncoder().encode("const a = 1 // café"))
    ).toBe("text/plain")
    // A NUL is what separates a text file from a binary that happens to decode.
    expect(sniffAttachment(new Uint8Array([0x61, 0x00, 0x62]))).toBeNull()
    // Lone continuation byte: not valid UTF-8, so not text.
    expect(sniffAttachment(new Uint8Array([0xc3, 0x28]))).toBeNull()
  })

  it("ignores the operating system's guess at a type", async () => {
    // Most systems call a TypeScript file "video/mp2t". It is text.
    expect(await classifyFile(file("a.ts", "export {}", "video/mp2t"))).toBe(
      "text/plain"
    )
    // And a PNG is a PNG even when the browser declared nothing at all.
    expect(await classifyFile(file("a.png", PNG))).toBe("image/png")
  })
})

describe("names", () => {
  it("refuses what the server refuses rather than rewriting it", () => {
    expect(canonicalAttachmentName(" notes.md ")).toBe("notes.md")
    expect(canonicalAttachmentName("")).toBeNull()
    expect(canonicalAttachmentName(".env")).toBeNull()
    expect(canonicalAttachmentName("..")).toBeNull()
    expect(canonicalAttachmentName("a/b.png")).toBeNull()
    expect(canonicalAttachmentName("a\\b.png")).toBeNull()
    expect(canonicalAttachmentName("c:file")).toBeNull()
    expect(canonicalAttachmentName("bad\nname")).toBeNull()
    expect(canonicalAttachmentName(`${"n".repeat(201)}`)).toBeNull()
  })
})

describe("admission", () => {
  it("takes the first five and names the rest", async () => {
    const files = Array.from({ length: 6 }, (_, index) =>
      file(`shot-${index}.png`, PNG)
    )
    const { accepted, rejected } = await admitFiles(files)
    expect(accepted).toHaveLength(5)
    expect(rejected).toEqual([{ name: "shot-5.png", reason: "tooMany" }])
  })

  it("counts what is already staged", async () => {
    const { accepted, rejected } = await admitFiles(
      [file("one.png", PNG)],
      [{ size: 1 }, { size: 1 }, { size: 1 }, { size: 1 }, { size: 1 }]
    )
    expect(accepted).toEqual([])
    expect(rejected[0].reason).toBe("tooMany")
  })

  it("accepts a file above the former per-file limit", async () => {
    const big = sized("huge.png", 44_778_046)
    const { accepted, rejected } = await admitFiles([big])
    expect(accepted).toHaveLength(1)
    expect(rejected).toEqual([])
  })

  it("accepts a file above the former aggregate turn limit", async () => {
    const one = sized("a.png", 9 * 1024 * 1024)
    const { rejected } = await admitFiles(
      [one],
      [{ size: 9 * 1024 * 1024 }, { size: 9 * 1024 * 1024 }]
    )
    expect(rejected).toEqual([])
  })

  it("refuses an unsupported type and an empty file, by name", async () => {
    const zip = file("a.zip", [0x50, 0x4b, 0x03, 0x04, 0x00, 0x01])
    const empty = file("nothing.txt", [])
    const { accepted, rejected } = await admitFiles([
      zip,
      empty,
      file("ok.txt", "hello"),
    ])
    expect(accepted.map((item) => item.file.name)).toEqual(["ok.txt"])
    expect(rejected).toEqual([
      { name: "a.zip", reason: "unsupportedType" },
      { name: "nothing.txt", reason: "empty" },
    ])
  })

  it("refuses a name the server would refuse before uploading it", async () => {
    const { rejected } = await admitFiles([file(".env", "SECRET=1")])
    expect(rejected).toEqual([{ name: ".env", reason: "badName" }])
  })
})

describe("drags and pastes", () => {
  const transfer = (over: Partial<DataTransfer>) => over as DataTransfer

  it("only treats a drag as a drop target when it carries files", () => {
    expect(dragCarriesFiles(transfer({ types: ["Files"] }))).toBe(true)
    expect(
      dragCarriesFiles(transfer({ types: ["text/plain", "text/html"] }))
    ).toBe(false)
    expect(dragCarriesFiles(null)).toBe(false)
  })

  it("reads dropped and pasted files, ignoring text items", () => {
    const png = file("a.png", PNG)
    const items = [
      { kind: "string", getAsFile: () => null },
      { kind: "file", getAsFile: () => png },
    ]
    expect(
      filesFromDrop(
        transfer({ items: items as unknown as DataTransferItemList })
      )
    ).toEqual([png])
    expect(
      filesFromPaste(
        transfer({ items: items as unknown as DataTransferItemList })
      )
    ).toEqual([png])
  })

  it("leaves a paste that also carries text alone", () => {
    expect(pasteIsFileOnly(transfer({ types: ["Files"] }))).toBe(true)
    expect(pasteIsFileOnly(transfer({ types: ["Files", "text/plain"] }))).toBe(
      false
    )
  })
})

describe("parsing", () => {
  const row = {
    key: KEY,
    fileName: "diagram.png",
    contentType: "image/png",
    byteLength: 1024,
  }

  it("accepts the server's own row", () => {
    expect(parseAttachment(row)).toEqual(row)
    expect(parseAttachments(undefined)).toEqual([])
    expect(parseAttachments(null)).toEqual([])
    expect(parseAttachments([row])).toEqual([row])
  })

  it("reads generated outputs alongside input uploads without widening the composer limit", () => {
    const files = Array.from({ length: 105 }, (_, i) => ({
      ...row,
      key: `11111111-1111-4111-8111-${String(i).padStart(12, "0")}`,
    }))
    expect(parseAttachments(files)).toHaveLength(105)
    expect(() =>
      parseAttachments([
        ...files,
        { ...row, key: "22222222-2222-4222-8222-222222222222" },
      ])
    ).toThrow(NativeClientError)
  })

  it("refuses a row this server could not have written", () => {
    expect(() => parseAttachment({ ...row, contentType: "image/gif" })).toThrow(
      NativeClientError
    )
    expect(() => parseAttachment({ ...row, fileName: "../escape" })).toThrow(
      NativeClientError
    )
    expect(() => parseAttachment({ ...row, byteLength: 0 })).toThrow(
      NativeClientError
    )
    expect(() => parseAttachment({ ...row, extra: 1 })).toThrow(
      NativeClientError
    )
    expect(() => parseAttachments([row, row])).toThrow(NativeClientError)
  })
})

describe("uploading", () => {
  function stubFetch(
    handler: (init: RequestInit) => Response | Promise<Response>
  ) {
    const fetcher = vi.fn(async (_path: string, init: RequestInit = {}) =>
      handler(init)
    )
    vi.stubGlobal("fetch", fetcher)
    return fetcher
  }

  const stored = {
    key: KEY,
    fileName: "notes.txt",
    contentType: "text/plain",
    byteLength: 5,
  }
  const ok = () =>
    new Response(JSON.stringify(stored), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    })

  it("sends the bytes, the CSRF token and a percent-encoded name", async () => {
    const fetcher = stubFetch(ok)
    const value = file("notes ü.txt", "hello")
    expect(await uploadChatAttachment(CHAT, value, "token")).toEqual(stored)
    const [path, init = {}] = fetcher.mock.calls[0]
    expect(path).toBe(`/api/neev/chats/${CHAT}/attachments`)
    expect(init.method).toBe("POST")
    expect(init.body).toBe(value)
    const headers = init.headers as Record<string, string>
    expect(headers["x-neev-csrf"]).toBe("token")
    expect(headers["X-Neev-Filename"]).toBe("notes%20%C3%BC.txt")
    expect(decodeURIComponent(headers["X-Neev-Filename"])).toBe("notes ü.txt")
  })

  it("uploads and parses an attachment above the former size limits", async () => {
    const value = sized("large.png", 44_778_046)
    const large = { ...stored, fileName: value.name, byteLength: value.size }
    const fetcher = stubFetch(
      () =>
        new Response(JSON.stringify(large), {
          headers: { "Content-Type": "application/json" },
        })
    )
    expect(await uploadChatAttachment(CHAT, value, "token")).toEqual(large)
    expect(fetcher.mock.calls[0][1]?.body).toBe(value)
  })

  it("keeps large uploads alive past the normal request timeout", async () => {
    vi.useFakeTimers()
    try {
      let finish!: (value: Response) => void
      const fetcher = stubFetch(
        () =>
          new Promise<Response>((resolve) => {
            finish = resolve
          })
      )
      const pending = uploadChatAttachment(
        CHAT,
        sized("large.png", 44_778_046),
        "token"
      )
      await vi.advanceTimersByTimeAsync(120_000)
      expect(fetcher.mock.calls[0][1]?.signal?.aborted).toBe(false)
      finish(ok())
      await pending
    } finally {
      vi.useRealTimers()
    }
  })

  it("distinguishes storage quota from rate limits and conflicts", async () => {
    stubFetch(
      () =>
        new Response(JSON.stringify({ code: "content_quota" }), { status: 429 })
    )
    try {
      await uploadChatAttachment(CHAT, file("notes.txt", "hello"), "token")
      throw new Error("Expected storage rejection")
    } catch (error) {
      expect(uploadRejection(error)).toBe("quota")
    }
    expect(uploadRejection(new NativeClientError("limited", 429))).toBeNull()
    expect(uploadRejection(new NativeClientError("conflict", 409))).toBeNull()
  })

  it("posts a session upload to the session's own path", async () => {
    const fetcher = stubFetch(ok)
    await uploadSessionAttachment(KEY, file("notes.txt", "hello"), "token")
    expect(fetcher.mock.calls[0][0]).toBe(
      `/api/neev/agent/sessions/${KEY}/attachments`
    )
  })

  it("never puts a file on the wire without a token or a usable name", async () => {
    const fetcher = stubFetch(ok)
    await expect(
      uploadChatAttachment(CHAT, file("a.txt", "x"), "")
    ).rejects.toThrow(NativeClientError)
    await expect(
      uploadChatAttachment(CHAT, file(".env", "x"), "token")
    ).rejects.toThrow(NativeClientError)
    await expect(
      uploadChatAttachment("not-a-chat", file("a.txt", "x"), "token")
    ).rejects.toThrow(NativeClientError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it("names the refusals that are facts about this file", () => {
    expect(
      uploadRejection(new NativeClientError("unavailable", 413))
    ).toBeNull()
    // Only the explicit storage-quota code is a storage error.
    expect(
      uploadRejection(new NativeClientError("limited", 429, "content_quota"))
    ).toBe("quota")
    expect(uploadRejection(new NativeClientError("invalid_request", 400))).toBe(
      "unsupportedType"
    )
    expect(
      uploadRejection(new NativeClientError("unavailable", 503))
    ).toBeNull()
    expect(uploadRejection(new Error("offline"))).toBeNull()
  })
})

it("addresses an attachment by key and refuses anything else", () => {
  expect(attachmentUrl(KEY)).toBe(`/api/neev/attachments/${KEY}`)
  expect(() => attachmentUrl("../images/1")).toThrow(NativeClientError)
})

it("shows a size a person can read", () => {
  expect(formatBytes(512)).toBe("512 B")
  expect(formatBytes(2048)).toBe("2 KB")
  expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MB")
})
