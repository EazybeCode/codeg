//! The admin export: `index.md`, `log.md`, `pages/<slug>.md` and
//! `provenance.json`, readable pages only (A3), capped at `MAX_EXPORT_BYTES`.

use super::status;
use super::types::*;
use crate::neev::{content::types::ContentError, db, startup::ServerState};
use sea_orm::ConnectionTrait;
use serde_json::json;
use std::io::Write;

/// Build the zip in the caller's transaction view.
pub(crate) async fn zip<C: ConnectionTrait>(state: &ServerState, db: &C, org_id: i32) -> Result<Vec<u8>, KnowledgeError> {
    let (open, withheld) = status::pages_split(state, db, org_id).await?;
    let index = status::index_markdown(&open, &withheld);
    let log = status::log_markdown(db, org_id, &open).await?;
    let mut provenance = Vec::new();
    for page in &open {
        let rows = db
            .query_all(db::statement(
                db,
                "SELECT s.source_id,s.source_kind,s.segment,s.marker,s.cite_key,r.title FROM neev_knowledge_revision_deps d \
                 JOIN neev_knowledge_snapshots s ON s.org_id=d.org_id AND s.id=d.snapshot_id \
                 JOIN neev_resources r ON r.org_id=s.org_id AND r.id=s.source_id \
                 WHERE d.org_id=$1 AND d.page_id=$2 AND d.revision=$3 ORDER BY s.source_id,s.segment,s.cite_key",
                vec![org_id.into(), page.id.clone().into(), page.revision.into()],
            ))
            .await
            .map_err(db_error)?;
        let mut sources = Vec::new();
        for row in &rows {
            sources.push(json!({
                "sourceId": row.try_get::<String>("", "source_id").map_err(db_error)?,
                "kind": row.try_get::<String>("", "source_kind").map_err(db_error)?,
                "title": row.try_get::<String>("", "title").map_err(db_error)?,
                "segment": row.try_get::<i32>("", "segment").map_err(db_error)?,
                "marker": row.try_get::<String>("", "marker").map_err(db_error)?,
                "citeKey": format!("k-{}", row.try_get::<String>("", "cite_key").map_err(db_error)?),
            }));
        }
        provenance.push(json!({
            "id": page.id,
            "slug": page.slug,
            "title": page.title,
            "revision": page.revision,
            "sources": sources,
        }));
    }
    let provenance = serde_json::to_vec_pretty(&json!({ "pages": provenance })).map_err(|_| KnowledgeError::storage())?;
    let total = index.len()
        + log.len()
        + provenance.len()
        + open.iter().map(|page| page.body.len()).sum::<usize>();
    if total > MAX_EXPORT_BYTES {
        return Err(KnowledgeError::Content(ContentError::Quota));
    }
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut add = |name: &str, bytes: &[u8]| -> Result<(), KnowledgeError> {
            zip.start_file(name, options).map_err(|_| KnowledgeError::storage())?;
            zip.write_all(bytes).map_err(|_| KnowledgeError::storage())?;
            Ok(())
        };
        add("index.md", index.as_bytes())?;
        add("log.md", log.as_bytes())?;
        add("provenance.json", &provenance)?;
        for page in &open {
            add(&format!("pages/{}.md", page.slug), page.body.as_bytes())?;
        }
        zip.finish().map_err(|_| KnowledgeError::storage())?;
    }
    Ok(buffer.into_inner())
}

/// Every entry's text, concatenated (tests).
#[cfg(test)]
pub(crate) fn unzip_all_text(bytes: &[u8]) -> String {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = String::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).unwrap();
        out.push_str(&format!("=== {}\n", file.name()));
        file.read_to_string(&mut out).unwrap();
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub(crate) fn entry_names(bytes: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    archive.file_names().map(str::to_owned).collect()
}
