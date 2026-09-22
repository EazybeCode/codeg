# Comments in the embedded editor — what enabling them requires

Comments do not work in the embed today. `abilities.comment` is `true`, so the
editor offers an **Add comment** button, and every request behind it is refused
by the proxy with its own 404. The button is therefore visibly broken rather than
absent, and **La Suite's feature set should not be described as working while
that is the case**.

This is the audit for enabling it deliberately. It is not a request for a general
allowlist under `documents/<id>/`, which would open far more than comments.

## The exact surface

Eleven endpoints, all of them doc-scoped, read from
`features/docs/doc-comments/api/DocsThreadStore.tsx` in pinned v5.7.0:

| method | path under `api/v1.0/` |
|---|---|
| `GET` | `documents/<doc>/threads/` |
| `POST` | `documents/<doc>/threads/` |
| `GET` | `documents/<doc>/threads/<thread>/` |
| `DELETE` | `documents/<doc>/threads/<thread>/` |
| `POST` | `documents/<doc>/threads/<thread>/resolve/` |
| `POST` | `documents/<doc>/threads/<thread>/unresolve/` |
| `POST` | `documents/<doc>/threads/<thread>/comments/` |
| `PUT` | `documents/<doc>/threads/<thread>/comments/<comment>/` |
| `DELETE` | `documents/<doc>/threads/<thread>/comments/<comment>/` |
| `POST` | `documents/<doc>/threads/<thread>/comments/<comment>/reactions/` |
| `DELETE` | `documents/<doc>/threads/<thread>/comments/<comment>/reactions/` |

Measured on the canary, unauthenticated by the proxy's own reckoning:

```
abilities.comment on a fresh engine document : true
GET  documents/<id>/threads/   owner          : 404  {"code":"not_found"}
POST documents/<id>/threads/   owner          : 404  {"code":"not_found"}
GET  documents/<id>/threads/   outsider       : 404
```

The `{"code":"not_found"}` shape is the proxy's, not Django's — the routes are
simply not allowlisted.

## The part that needs checking before it is enabled

**The proxy can authorise the document. It cannot authorise the thread.**

Every one of these paths carries `<doc>`, which is what the proxy already
authorises for `documents/<doc>/`. But `<thread>` and `<comment>` are opaque
identifiers the proxy has no way to place. If upstream does not constrain a
thread to the document named in its URL, then a caller authorised on document A
could name a thread belonging to document B and reach it through A's path — and
the proxy would have no way to tell.

So the question to settle first, against upstream source rather than by
assumption:

1. does the thread viewset filter by the document from the URL, so a thread from
   another document is 404 rather than accessible?
2. same question for `comments/<comment>/` nested inside a thread, and for
   `reactions/` inside that;
3. does `abilities.comment` actually gate the write endpoints server-side, or
   only the UI? It is `false` on an archived document — measured — which is
   promising, but the UI reading it is not proof the API enforces it.

That is the bridge worker's to confirm. **If (1) or (2) does not hold, these
routes must not be allowlisted**, because the doc-scoped prefix would be
providing an authorisation it cannot actually perform.

## If it is enabled

- allowlist these eleven shapes specifically, with UUID-shaped path segments —
  not a wildcard under `documents/<id>/`, which would also expose versions,
  accesses, invitations, link configuration and the AI routes
- the write methods need CSRF, which the proxy already passes through
- `abilities.comment` is `false` on an archived document, so an archived
  document should refuse comment writes; worth asserting once enabled

`deploy/lasuite/acceptance` has a case for this — *comments are reachable, or the
comment UI is a dead button*. It checks the ability first, so it only fails when
commenting is **offered and unreachable**, which is the state that misleads a
user. It also checks that another organization cannot read the threads.

## Until then

The honest description is that the embedded editor supports writing, formatting,
tables and real-time collaboration, and that **comments are not available**. The
Add comment button appearing is a defect of this configuration, not evidence the
feature works.

If comments will not be enabled before release, the cleaner alternative is to
hide the affordance in embed mode rather than leave a button that does nothing.
That is a one-line gate in the embed patch series alongside the other hidden
chrome — it costs a rebuild, so it is worth deciding rather than defaulting into.
