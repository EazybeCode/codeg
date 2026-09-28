// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest"
import { readHtmlPreview } from "./neev-file-actions"
const file = {
  key: "11111111-1111-4111-8111-111111111111",
  fileName: "preview.html",
  contentType: "text/plain" as const,
  byteLength: 0,
}
function respond(html: string) {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(html)))
}
afterEach(() => vi.unstubAllGlobals())
describe("HTML preview integrity and isolation", () => {
  it("keeps the end of a large self-contained page", async () => {
    respond(
      "<!doctype html><html><head><style>body{color:red}</style></head><body><!--" +
        "x".repeat(22 * 1024 * 1024) +
        "--><h1>End of full page</h1></body></html>"
    )
    const result = await readHtmlPreview(file)
    expect(result.html).toContain("End of full page")
    expect(result.truncated).toBe(false)
  }, 30000)
  it("removes navigation and active elements despite alternative attribute syntax", async () => {
    respond(
      '<meta http-equiv=refresh content="0;url=https://example.test"><base href="https://example.test"><script>alert(1)</script><iframe src="/workspace"></iframe><form action="/api/neev/danger"><button formaction="/api/neev/danger">Go</button></form><a href="/workspace" ping="/api/neev/danger">Go</a><img src="/api/neev/image" onerror=alert(1)><h1>Keep this</h1>'
    )
    const { html } = await readHtmlPreview(file)
    const doc = new DOMParser().parseFromString(html, "text/html")
    expect(
      doc.querySelector("script,iframe,base,meta[http-equiv=refresh]")
    ).toBeNull()
    expect(
      doc.querySelector("[action],[formaction],[ping],[onerror]")
    ).toBeNull()
    expect(doc.querySelector("a")?.hasAttribute("href")).toBe(false)
    expect(doc.querySelector("img")?.hasAttribute("src")).toBe(false)
    expect(html).toContain("Keep this")
  })
  it("retains inline styling and embedded raster images", async () => {
    respond(
      '<style>h1{color:coral}</style><h1 style="padding:20px">Preview</h1><img src="data:image/png;base64,aGVsbG8=">'
    )
    const { html } = await readHtmlPreview(file)
    expect(html).toContain("h1{color:coral}")
    expect(html).toContain("padding:20px")
    expect(html).toContain("data:image/png;base64,aGVsbG8=")
  })
})

it("preserves script-bootstrapped header templates without executing their scripts", async () => {
  respond(
    '<div id="hero"><template><style>:host{display:block}</style><header>Header navigation</header><h1>Hero headline</h1><img src="/private" onerror="bad()"><script>bad()</script></template></div><script>const host=document.currentScript.previousElementSibling;host.attachShadow({mode:"open"})</script>'
  )
  const { html } = await readHtmlPreview(file)
  const doc = new DOMParser().parseFromString(html, "text/html")
  const template = doc.querySelector(
    'template[shadowrootmode="open"]'
  ) as HTMLTemplateElement
  expect(template).not.toBeNull()
  expect(template.content.querySelector("header")?.textContent).toBe(
    "Header navigation"
  )
  expect(template.content.querySelector("script,[onerror],[src]")).toBeNull()
  expect(doc.querySelector("script")).toBeNull()
  expect(html).toContain("script-src 'none'")
})

it("sanitizes nested declarative shadow content and discards unrelated inert templates", async () => {
  respond(
    '<div><template shadowrootmode="open"><section><template shadowrootmode="closed"><h1>Nested</h1><iframe src="/workspace"></iframe><img src="data:image/png;base64,aGVsbG8=" onload="bad()"></template></section></template></div><template id="unused">Hidden template</template>'
  )
  const { html } = await readHtmlPreview(file)
  expect(html).toContain("Nested")
  expect(html).toContain("data:image/png;base64,aGVsbG8=")
  expect(html).not.toContain("<iframe")
  expect(html).not.toContain("onload")
  expect(html).not.toContain("Hidden template")
})
