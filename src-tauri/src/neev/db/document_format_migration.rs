use super::{execute, one, AuthError};
use sea_orm::{ConnectionTrait, DatabaseTransaction};

pub(super) const SCHEMA_VERSION: i64 = 6;
pub(super) const TABLE_NAMES: &[&str] = &["neev_document_formats"];

pub(super) async fn upgrade(tx: &DatabaseTransaction) -> Result<(), AuthError> {
    execute(tx, "CREATE TABLE neev_document_formats (
        org_id INTEGER NOT NULL CHECK (org_id>0),
        document_id TEXT NOT NULL CHECK (length(document_id)=36),
        revision BIGINT NOT NULL CHECK (revision>0),
        format TEXT NOT NULL,
        format_version INTEGER NOT NULL,
        PRIMARY KEY (org_id,document_id,revision),
        CHECK (format IN ('markdown','blocks') AND format_version=1),
        FOREIGN KEY (org_id,document_id,revision) REFERENCES neev_document_revisions(org_id,document_id,revision) ON DELETE RESTRICT ON UPDATE RESTRICT
    )", vec![]).await?;
    execute(tx,"INSERT INTO neev_document_formats(org_id,document_id,revision,format,format_version) SELECT org_id,document_id,revision,'markdown',1 FROM neev_document_revisions",vec![]).await?;
    validate(tx).await
}

pub(super) async fn validate<C: ConnectionTrait>(db: &C) -> Result<(), AuthError> {
    // Validate data even when an archive was produced with constraints disabled.
    // Both directions matter: inner joins alone silently omit missing/orphan rows.
    for sql in [
        "SELECT 1 AS invalid FROM neev_document_formats WHERE org_id IS NULL OR org_id<=0 OR document_id IS NULL OR length(document_id)<>36 OR revision IS NULL OR revision<=0 OR format IS NULL OR format NOT IN ('markdown','blocks') OR format_version IS NULL OR format_version<>1 LIMIT 1",
        "SELECT 1 AS invalid FROM neev_document_formats f LEFT JOIN neev_document_revisions r ON r.org_id=f.org_id AND r.document_id=f.document_id AND r.revision=f.revision WHERE r.revision IS NULL LIMIT 1",
        "SELECT 1 AS invalid FROM neev_document_revisions r LEFT JOIN neev_document_formats f ON r.org_id=f.org_id AND r.document_id=f.document_id AND r.revision=f.revision GROUP BY r.org_id,r.document_id,r.revision HAVING COUNT(f.revision)<>1 LIMIT 1",
    ] {
        if one(db,sql,vec![]).await?.is_some() {return Err(AuthError::Unavailable);}
    }
    Ok(())
}
