//! The Neev document engine: La Suite Docs v5.7.0 behind the Neev server.
//!
//! Neev is the **sole** public route to Docs — the Docs backend, y-provider and
//! the static frontend publish no ports. Every request under
//! [`routes::PREFIX`] is classified by an allowlist, authorized against Neev's
//! own tables, and only then forwarded. See
//! `docs/decisions/neev-document-engine-2026-09-22.md` for why the pieces are
//! shaped this way, including the two upstream facts that decided them: that
//! y-provider forwards only `cookie`/`origin`/its API key to the backend, and
//! that Hocuspocus 3.4.4 fixes a connection's document from the request URL.
//!
//! This engine backs **new** documents only. Existing native documents keep the
//! native engine and are not migrated.
#![allow(dead_code)]

pub(crate) mod assets;
pub(crate) mod binding;
pub(crate) mod decision;
pub(crate) mod identity;
pub(crate) mod proxy;
pub(crate) mod routes;
pub(crate) mod transport;
pub(crate) mod upstream;

#[cfg(test)]
mod binding_tests;
