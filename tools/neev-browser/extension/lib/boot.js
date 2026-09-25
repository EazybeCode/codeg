/**
 * Startup visibility for the side panel.
 *
 * The panel is a module script. Any throw during its evaluation — a failed
 * import, a `chrome` namespace that is not there, a service worker that will
 * not start — aborts the rest of the file silently. The static HTML still
 * paints, so the panel looks right and then does nothing at all: no listener
 * below the throw was ever registered. Chrome reports that only to the panel's
 * own devtools, which is exactly the surface someone staring at a dead panel
 * cannot reach.
 *
 * So the panel has one entry point, `boot.js`, and it loads `sidepanel.js`
 * itself with a dynamic import. Success and failure are then the two
 * settlements of one promise, with no timing assumption anywhere: the panel
 * either evaluated or it did not.
 *
 * The earlier version of this file used a zero-delay timer queued from a
 * second `<script>` tag and treated "the timer fired first" as proof the panel
 * had died. Document order is evaluation order, but the two tags each fetch
 * their own module graph, and that timer could run while `sidepanel.js` was
 * still being fetched — a permanent error banner over a panel that was about
 * to work perfectly. Hence the import.
 */

let banner = null

/** Whatever an error event carries, reduced to one readable line. */
export function describe(value) {
  if (!value) return "unknown error"
  if (typeof value === "string") return value
  if (value.message) return String(value.message)
  if (value.error) return describe(value.error)
  if (value.reason) return describe(value.reason)
  return String(value)
}

export function installStartupReporter({ win, doc }) {
  banner = doc.getElementById("boot")

  win.addEventListener("error", (event) => {
    report("Panel error: " + describe(event && (event.error || event)))
  })
  win.addEventListener("unhandledrejection", (event) => {
    report("Panel error: " + describe(event && event.reason))
  })

  return { report }
}

/** Show a line in the boot banner. The first one shown is the one kept. */
export function report(text) {
  if (!banner || !banner.hidden) return
  banner.textContent = text
  banner.hidden = false
}

/**
 * Install the reporter, then load the panel. `load` is injected so the tests
 * can hold the import pending, resolve it, or reject it on demand.
 */
export async function bootPanel({ win, doc, load }) {
  installStartupReporter({ win, doc })
  try {
    await load()
  } catch (error) {
    report(
      "The panel script did not start: " +
        describe(error) +
        ". Nothing below will respond — reload the extension, then reopen " +
        "the panel."
    )
  }
}
