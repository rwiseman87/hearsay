//! Integration tests over the assembled router, driven with `tower::ServiceExt::oneshot` against a
//! single-connection in-memory SQLite DB. The live-capture routes run against `DisabledEngine`
//! (503 / clean close), so this covers the whole self-contained surface without a real pipeline.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tower::ServiceExt;
use uuid::Uuid;

use hearsay_core::{create_app, AppState, DisabledEngine, Settings};
use hearsay_db::models::Stream;
use hearsay_db::{connect_options, queries, MIGRATOR};

const TOKEN: &str = "test-session-token";

async fn memory_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options("sqlite::memory:").unwrap())
        .await
        .unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    pool
}

/// Build the app over an in-memory DB. Returns the app, the pool (for seeding), and the temp dir
/// used as `output_dir` (kept alive for the test's duration).
async fn setup() -> (Router, SqlitePool, tempfile::TempDir) {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let settings = Settings {
        database_url: "sqlite::memory:".into(),
        output_dir: tmp.path().to_path_buf(),
        web_dir: tmp.path().join("no-web"),
        server_host: "127.0.0.1".into(),
        server_port: 0,
        environment: "test".into(),
    };
    let state = AppState::new(
        pool.clone(),
        settings,
        TOKEN.to_string(),
        Arc::new(DisabledEngine),
    );
    (create_app(state), pool, tmp)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

#[tokio::test]
async fn requires_a_valid_bearer_token() {
    let (app, _pool, _tmp) = setup().await;
    let no_token = Request::builder()
        .uri("/api/meetings")
        .header("host", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, no_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(&app, get("/api/meetings")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn rejects_non_loopback_host_and_origin() {
    let (app, _pool, _tmp) = setup().await;

    let bad_host = Request::builder()
        .uri("/api/meetings")
        .header("host", "evil.example")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, bad_host).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let bad_origin = Request::builder()
        .uri("/api/meetings")
        .header("host", "127.0.0.1")
        .header("origin", "https://evil.example")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, bad_origin).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn lists_and_gets_meetings_with_the_page_envelope() {
    let (app, pool, _tmp) = setup().await;
    let started = chrono::Utc::now();
    let meeting = queries::create_meeting(&pool, "Standup", "2026-07-01_0900_standup", started)
        .await
        .unwrap();

    let (status, body) = send(&app, get("/api/meetings")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 50);
    assert_eq!(body["items"][0]["title"], "Standup");
    assert_eq!(body["items"][0]["status"], "recording");

    let (status, body) = send(&app, get(&format!("/api/meetings/{}", meeting.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], meeting.id.to_string());

    let (status, _) = send(&app, get(&format!("/api/meetings/{}", Uuid::new_v4()))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lists_segments_ordered_by_start() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();
    queries::insert_segment(&pool, meeting.id, Stream::Me, "Me", "second", 5.0, 6.0)
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "first",
        1.0,
        2.0,
    )
    .await
    .unwrap();

    let (status, body) = send(&app, get(&format!("/api/meetings/{}/segments", meeting.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["page_size"], 200);
    assert_eq!(body["items"][0]["text"], "first");
    assert_eq!(body["items"][0]["stream"], "them");
    assert_eq!(body["items"][1]["text"], "second");
}

#[tokio::test]
async fn start_and_stop_are_unavailable_without_an_engine() {
    let (app, _pool, _tmp) = setup().await;
    let start = Request::builder()
        .method("POST")
        .uri("/api/meetings")
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from("{\"title\":\"x\"}"))
        .unwrap();
    let (status, _) = send(&app, start).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let stop = Request::builder()
        .method("POST")
        .uri(format!("/api/meetings/{}/stop", Uuid::new_v4()))
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, stop).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn renames_a_speaker_and_404s_on_unknown_cluster() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();

    let rename = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/meetings/{}/speakers/{}",
            meeting.id, cluster.id
        ))
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from("{\"display_name\":\"  Alice  \"}"))
        .unwrap();
    let (status, body) = send(&app, rename).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["label"], "Alice");
    assert_eq!(body["locked"], true);

    // The speakers list now resolves the bound name.
    let (status, body) = send(&app, get(&format!("/api/meetings/{}/speakers", meeting.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["label"], "Alice");

    let unknown = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/meetings/{}/speakers/{}",
            meeting.id,
            Uuid::new_v4()
        ))
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from("{\"display_name\":\"Bob\"}"))
        .unwrap();
    let (status, _) = send(&app, unknown).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unbound_speaker_falls_back_to_ordinal_label() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();
    queries::create_cluster(&pool, meeting.id, 3, false, None)
        .await
        .unwrap();

    let (status, body) = send(&app, get(&format!("/api/meetings/{}/speakers", meeting.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["label"], "Speaker 3");
    assert_eq!(body["items"][0]["locked"], false);
}

#[tokio::test]
async fn lists_identities_after_a_rename() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, cluster.id, "Carol")
        .await
        .unwrap();

    let (status, body) = send(&app, get("/api/identities")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["display_name"], "Carol");
}

#[tokio::test]
async fn rediarize_is_404_then_unavailable() {
    let (app, pool, _tmp) = setup().await;
    let post = |id: Uuid| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/meetings/{id}/rediarize"))
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };

    let (status, _) = send(&app, post(Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(&app, post(meeting.id)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn audio_checks_the_query_token_and_404s_without_a_file() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();

    // No token at all (still needs a loopback Host to pass the global layer).
    let no_token = Request::builder()
        .uri(format!("/api/meetings/{}/audio", meeting.id))
        .header("host", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, no_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Valid query token, but no audio.wav on disk.
    let with_token = Request::builder()
        .uri(format!("/api/meetings/{}/audio?token={TOKEN}", meeting.id))
        .header("host", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let (status, _) = send(&app, with_token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deletes_a_meeting_then_404s() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", chrono::Utc::now())
        .await
        .unwrap();

    let del = |id: Uuid| {
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/meetings/{id}"))
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };

    let (status, _) = send(&app, del(meeting.id)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, del(meeting.id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_json_is_served() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["openapi"].is_string());
    assert!(body["paths"]["/api/meetings"].is_object());
}
