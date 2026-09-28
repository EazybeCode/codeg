/** Static visual preview only. The iframe must also use sandbox="". */
export function staticHtmlDocument(html: string): string {
  // Template contents are inert while parsed: images/iframes are not loaded
  // into the application document before their attributes can be removed.
  const template = document.createElement("template")
  template.innerHTML = html
  const roots = [template.content]
  while (roots.length) {
    const root = roots.pop()!
    // Restore common script-mounted shadow templates declaratively. The browser
    // can render their scoped markup without running any uploaded JavaScript.
    // Template contents are separate fragments and must each be sanitized.
    for (const nested of root.querySelectorAll("template")) {
      const mode = nested.getAttribute("shadowrootmode")
      const bootstrap = nested.parentElement?.nextElementSibling
      const mountsShadow =
        bootstrap?.tagName === "SCRIPT" &&
        /document\.currentScript\.previousElementSibling/.test(
          bootstrap.textContent ?? ""
        ) &&
        /\.attachShadow\s*\(/.test(bootstrap.textContent ?? "")
      if (mode === "open" || mode === "closed" || mountsShadow) {
        nested.setAttribute("shadowrootmode", "open")
        roots.push(nested.content)
      } else nested.remove()
    }
    root
      .querySelectorAll("script,iframe,object,embed,base,meta,link,portal")
      .forEach((element) => element.remove())
    for (const element of root.querySelectorAll("*")) {
      for (const attribute of Array.from(element.attributes)) {
        const name = attribute.name.toLowerCase()
        const value = attribute.value.trim()
        if (
          name.startsWith("on") ||
          [
            "srcdoc",
            "srcset",
            "action",
            "formaction",
            "ping",
            "target",
            "download",
            "autofocus",
            "is",
            "nonce",
          ].includes(name)
        ) {
          element.removeAttribute(attribute.name)
        } else if (
          ["src", "href", "xlink:href", "poster", "background"].includes(name)
        ) {
          const embeddedRaster =
            /^data:image\/(?:png|jpe?g|gif|webp|avif);base64,/i.test(value)
          const localSvgReference =
            element.namespaceURI === "http://www.w3.org/2000/svg" &&
            /^#[\w:.-]+$/.test(value)
          if (!embeddedRaster && !localSvgReference)
            element.removeAttribute(attribute.name)
        }
      }
    }
  }
  // Applied before any untrusted content. No requests, scripts, forms, frames
  // or active plugins; inline styles and embedded raster images remain useful.
  const policy =
    "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; connect-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
  return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${policy}"></head><body>${template.innerHTML}</body></html>`
}
