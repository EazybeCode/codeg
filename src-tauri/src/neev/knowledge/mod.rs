//! Org knowledge: a saved, admin-only LLM wiki (spec
//! `docs/superpowers/specs/2026-10-01-org-knowledge-design.md`, Amendment 1).
//!
//! - `capture` keeps bounded, deterministic text snapshots of every eligible
//!   shared source (no LLM).
//! - `jobs` + `compile` + `output` compile those snapshots into linked
//!   markdown pages by running batches on the starting admin's OWN connected
//!   agent, through ordinary agent turns with `RunPurpose::Knowledge`.
//! - `status` + `export` serve the admin-only views.
//!
//! Saved only: nothing here publishes to members or feeds any agent context.

pub(crate) mod access;
pub(crate) mod capture;
pub(crate) mod compile;
pub(crate) mod export;
pub(crate) mod jobs;
pub(crate) mod output;
pub(crate) mod render;
pub(crate) mod settings;
pub(crate) mod status;
pub(crate) mod store;
pub(crate) mod types;

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod backup_verify_tests;
#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod compile_tests;
#[cfg(test)]
mod export_tests;
#[cfg(test)]
mod jobs_tests;
#[cfg(test)]
mod output_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod review_fix_tests;
#[cfg(test)]
pub(crate) mod test_support;

/// The append-only knowledge log. `detail` carries codes and counts only —
/// never slugs, titles or source text (Amendment 1, A3).
pub(crate) mod log {
    use super::types::{db_error, KnowledgeError};
    use crate::neev::db;
    use sea_orm::ConnectionTrait;

    pub(crate) const MAX_DETAIL_CHARS: usize = 500;

    /// Append one entry inside the caller's transaction and return its `seq`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn append<C: ConnectionTrait>(
        db: &C,
        org_id: i32,
        actor: Option<i32>,
        event: &str,
        page_id: Option<&str>,
        job_id: Option<&str>,
        detail: &str,
        at: i64,
    ) -> Result<i64, KnowledgeError> {
        let detail: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
        let next: i64 = db::one(
            db,
            "SELECT COALESCE(MAX(seq),0)+1 AS n FROM neev_knowledge_log WHERE org_id=$1",
            vec![org_id.into()],
        )
        .await?
        .ok_or_else(KnowledgeError::storage)?
        .try_get("", "n")
        .map_err(db_error)?;
        db::execute(
            db,
            "INSERT INTO neev_knowledge_log(org_id,seq,at,actor_id,event,page_id,job_id,detail) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
            vec![
                org_id.into(),
                next.into(),
                at.into(),
                actor.into(),
                event.into(),
                page_id.map(str::to_owned).into(),
                job_id.map(str::to_owned).into(),
                detail.into(),
            ],
        )
        .await?;
        Ok(next)
    }
}
