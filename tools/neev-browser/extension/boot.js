/**
 * The panel's only entry point.
 *
 * `sidepanel.html` loads this and nothing else; the panel itself arrives
 * through the import below, so a panel that fails to evaluate is a rejected
 * promise rather than silence. Kept to two statements: anything that could
 * throw belongs in `lib/boot.js`, where the tests can reach it.
 */

import { bootPanel } from "./lib/boot.js"

bootPanel({
  win: window,
  doc: document,
  load: () => import("./sidepanel.js"),
})
