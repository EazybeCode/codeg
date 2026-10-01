//! Task 9 (A1.6): withheld opacity in every read model and the export.

use super::test_support::{database, K};
use super::{export, status};
use crate::neev::content::types::Visibility;
use crate::neev::db;
use serde_json::Value;

const SECRET_SLUG: &str = "secret-slug";
const SECRET_TITLE: &str = "Secret Title";
const SECRET_BODY: &str = "WITHHELD BODY TEXT";

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

struct Fixture {
    k: K,
    visible: String,
    withheld: String,
    b_source: String,
    b_key: String,
}

async fn fixture() -> Fixture {
    let k = K::new(database().await).await;
    k.enable_capture().await;
    let a = k.shared_doc("Revoked source").await;
    k.save_markdown(&a.id, "# A\n\nalpha").await;
    let b = k.shared_chat("Open source").await;
    k.post(&b.id, "beta").await;
    k.capture().await;
    let b_key = k.current_snapshots(&b.id).await[0].cite_key.clone();
    let visible = k.seed_page("visible", "Visible", &format!("# Visible\n\nSee [[{SECRET_SLUG}]], [[missing-page]] and [[visible]].\n\nFact.[^k-{b_key}]\n"), &[&b.id]).await;
    let withheld = k.seed_page(SECRET_SLUG, SECRET_TITLE, &format!("# {SECRET_TITLE}\n\n{SECRET_BODY}\n"), &[&a.id]).await;
    for page in [&visible, &withheld] {
        super::log::append(&k.state.database, k.admin.org_id, None, "page_revision", Some(page), None, "revision=1", k.now()).await.unwrap();
    }
    k.set_visibility(&a.id, Visibility::Private).await;
    Fixture { k, visible, withheld, b_source: b.id, b_key }
}

fn assert_opaque(text: &str, label: &str) {
    for secret in [SECRET_SLUG, SECRET_TITLE, SECRET_BODY] {
        assert!(!text.contains(secret), "{label} leaks {secret:?}: {text}");
    }
}

#[tokio::test]
async fn a_withheld_page_is_opaque_in_every_read_model() {
    let f = fixture().await;
    let db = &f.k.state.database;
    let org = f.k.admin.org_id;
    let pages = status::pages(&f.k.state, db, org).await.unwrap();
    assert_opaque(&pages.to_string(), "page list");
    let withheld = pages.as_array().unwrap().iter().find(|p| p["id"] == f.withheld.as_str()).unwrap();
    assert_eq!(keys(withheld), ["id", "revision", "status", "unreadableSources", "updatedAt"]);
    assert_eq!((withheld["status"].as_str(), withheld["unreadableSources"].as_i64()), (Some("withheld"), Some(1)));
    let page = status::page(&f.k.state, db, org, &f.withheld).await.unwrap().unwrap();
    assert_eq!(keys(&page), ["id", "revision", "status", "unreadableSources"]);
    let index = {
        let (open, held) = status::pages_split(&f.k.state, db, org).await.unwrap();
        status::index_markdown(&open, &held)
    };
    assert_opaque(&index, "index.md");
    assert!(index.contains(&format!("- (withheld page {}, 1 sources no longer shared)", f.withheld)));
    let log = status::log_entries(&f.k.state, db, org, None).await.unwrap();
    assert_opaque(&log.to_string(), "log");
    for entry in log.as_array().unwrap() {
        assert_eq!(keys(entry), ["actorId", "at", "detail", "event", "pageId", "pageTitle", "seq"]);
        if entry["pageId"] == f.withheld.as_str() {
            assert!(entry["pageTitle"].is_null());
        }
        if entry["pageId"] == f.visible.as_str() {
            assert_eq!(entry["pageTitle"], "Visible");
        }
    }
    let zip = export::zip(&f.k.state, db, org).await.unwrap();
    let mut names = export::entry_names(&zip);
    names.sort();
    assert_eq!(names, ["index.md", "log.md", "pages/visible.md", "provenance.json"]);
    assert_opaque(&export::unzip_all_text(&zip), "export");
}

#[tokio::test]
async fn a_readable_page_resolves_its_citations_and_provenance_and_lints() {
    let f = fixture().await;
    let db = &f.k.state.database;
    let org = f.k.admin.org_id;
    let page = status::page(&f.k.state, db, org, &f.visible).await.unwrap().unwrap();
    assert_eq!(keys(&page), ["citations", "id", "lint", "markdown", "revision", "slug", "stale", "status", "title"]);
    let citation = &page["citations"][0];
    assert_eq!(keys(citation), ["key", "kind", "nodeId", "segment", "sourceId", "title", "withheld"]);
    assert_eq!(citation["key"], format!("k-{}", f.b_key));
    assert_eq!((citation["withheld"].as_bool(), citation["sourceId"].as_str()), (Some(false), Some(f.b_source.as_str())));
    assert_eq!((citation["kind"].as_str(), citation["segment"].as_i64(), citation["title"].as_str()), (Some("chat"), Some(1), Some("Open source")));
    assert!(citation["nodeId"].as_i64().unwrap() > 0);
    // A link to a page that is not readable is a dead link, and a link to a
    // WITHHELD page is redacted from the readable body on read.
    assert!(!page["markdown"].as_str().unwrap().contains(SECRET_SLUG));
    assert!(page["markdown"].as_str().unwrap().contains("[[withheld page]]"));
    assert!(page["lint"].as_array().unwrap().iter().any(|v| v == "dead_link"));
    let zip = export::zip(&f.k.state, db, org).await.unwrap();
    let text = export::unzip_all_text(&zip);
    let provenance: Value = serde_json::from_str(text.split("=== provenance.json\n").nth(1).unwrap().split("\n=== ").next().unwrap()).unwrap();
    let sources = &provenance["pages"][0]["sources"][0];
    assert_eq!(sources["sourceId"], f.b_source.as_str());
    assert_eq!(sources["citeKey"], format!("k-{}", f.b_key));
    assert!(sources["marker"].as_str().is_some() && sources["segment"] == 1);
    // A newer capture of the source makes the page stale.
    f.k.post(&f.b_source, "gamma").await;
    f.k.advance(120);
    f.k.capture().await;
    let page = status::page(&f.k.state, db, org, &f.visible).await.unwrap().unwrap();
    assert_eq!(page["stale"], true);
}

#[tokio::test]
async fn status_matches_the_frontend_contract() {
    let f = fixture().await;
    let value = status::compute(&f.k.state, &f.k.state.database, f.k.admin.org_id).await.unwrap();
    assert_eq!(
        keys(&value),
        ["bytesUsed", "captureBlocked", "captureEnabled", "captured", "coverage", "eligible", "job", "pages", "represented", "sourceProblems", "stale", "withheldPages"]
    );
    assert_eq!(keys(&value["coverage"]), ["cited", "pending", "skippedByAgent", "uncovered", "units"]);
    assert_eq!(keys(&value["sourceProblems"]), ["engineUnavailable", "failed", "tooLarge"]);
    assert_eq!((value["pages"].as_i64(), value["withheldPages"].as_i64()), (Some(2), Some(1)));
    assert_eq!((value["eligible"].as_i64(), value["captured"].as_i64(), value["represented"].as_i64()), (Some(1), Some(1), Some(0)));
    assert_eq!(value["coverage"]["pending"], 1);
    assert!(value["job"].is_null() || value["job"].is_object());
    assert!(value["bytesUsed"].as_i64().unwrap() > 0);
    db::execute(&f.k.state.database, "UPDATE neev_knowledge_coverage SET state='cited'", vec![]).await.unwrap();
    let value = status::compute(&f.k.state, &f.k.state.database, f.k.admin.org_id).await.unwrap();
    assert_eq!(value["represented"], 1);
    let sources = status::sources(&f.k.state.database, f.k.admin.org_id, None).await.unwrap();
    assert_eq!(keys(&sources), ["items", "nextCursor"]);
    let item = &sources["items"][0];
    assert_eq!(keys(item), ["captureState", "engine", "id", "kind", "represented", "stale", "title"]);
    assert_eq!((item["captureState"].as_str(), item["represented"].as_bool()), (Some("ok"), Some(true)));
    assert_eq!(sources["items"].as_array().unwrap().len(), 1, "only eligible sources are listed");
}
