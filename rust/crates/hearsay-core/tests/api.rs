//! Integration tests over the assembled router, driven with `tower::ServiceExt::oneshot` against a
//! single-connection in-memory SQLite DB. The live-capture routes run against `DisabledEngine`
//! (503 / clean close), so this covers the whole self-contained surface without a real pipeline.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use serde_json::Value;
use sqlx::SqlitePool;
use tower::ServiceExt;
use uuid::Uuid;

use hearsay_core::{create_app, AppState, DisabledEngine, LiveEngine, Settings};
use hearsay_db::models::Stream;
use hearsay_db::queries;
use hearsay_db::test_support::memory_pool;
use hearsay_orchestrator::testing::{chunk, seg, ScriptedBackend, ScriptedRefiner};
use hearsay_orchestrator::{Orchestrator, RefinedThemSegment, SegmentKind};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header as ws_header;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const TOKEN: &str = "test-session-token";

/// Test settings over an in-memory DB with the given `output_dir` / `web_dir` (loopback, no real
/// sidecars). One place to build `Settings` so a new field does not fan out across the tests.
fn test_settings(output_dir: PathBuf, web_dir: PathBuf) -> Settings {
    Settings {
        database_url: "sqlite::memory:".into(),
        output_dir,
        web_dir,
        server_host: "127.0.0.1".into(),
        server_port: 0,
        environment: "test".into(),
        helper_path: PathBuf::from("no-helper"),
        scripted: false,
        refine_timeout: std::time::Duration::from_secs(1800),
        auto_refine: false,
        record: true,
        recognition_threshold: 0.6,
        inactivity_prompt: true,
        inactivity_auto_end: true,
        inactivity_prompt_minutes: 5,
        inactivity_end_minutes: 10,
        compress_audio: true,
        compress_after_days: 7,
        notes_enabled: false,
        notes_model: PathBuf::from("no-notes-model"),
        notes_prompt: "Summarize:\n{transcript}".into(),
        notes_binary: PathBuf::from("no-notes-sidecar"),
        models_dir: PathBuf::from("no-models-dir"),
        handshake_path: None,
        notices_path: PathBuf::from("no-notices"),
        home_dir: None,
    }
}

/// Build the app over an in-memory DB. Returns the app, the pool (for seeding), and the temp dir
/// used as `output_dir` (kept alive for the test's duration).
async fn setup() -> (Router, SqlitePool, tempfile::TempDir) {
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let settings = test_settings(tmp.path().to_path_buf(), tmp.path().join("no-web"));
    let state = AppState::new(
        pool.clone(),
        settings,
        TOKEN.to_string(),
        Arc::new(DisabledEngine),
        Default::default(),
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

fn put(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn patch(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn del(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
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
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "response body was not valid JSON ({e}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
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
async fn index_gates_the_token_and_pins_the_ws_host() {
    // A minimal built UI so the web router mounts `GET /`.
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<html><head></head><body>hi</body></html>",
    )
    .unwrap();
    let settings = test_settings(tmp.path().to_path_buf(), web);
    let state = AppState::new(
        pool,
        settings,
        TOKEN.to_string(),
        Arc::new(DisabledEngine),
        Default::default(),
    );
    let app = create_app(state);

    let index_req = |uri: &str| {
        Request::builder()
            .uri(uri)
            .header("host", "127.0.0.1:8137")
            .body(Body::empty())
            .unwrap()
    };
    let raw_body = |resp: axum::response::Response| async move {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    };

    // No token: 401, and the token never appears in the served body.
    let resp = app.clone().oneshot(index_req("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !raw_body(resp).await.contains(TOKEN),
        "an unauthenticated GET / must not leak the session token"
    );

    // Wrong token: also 401.
    let resp = app
        .clone()
        .oneshot(index_req("/?token=nope"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Valid token: 200, the bootstrap injects the token, and the CSP pins the WS host to the request
    // Host (no localhost / port wildcard).
    let resp = app
        .clone()
        .oneshot(index_req(&format!("/?token={TOKEN}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp
        .headers()
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        csp.contains("ws://127.0.0.1:8137"),
        "CSP must pin the WS host to the request Host: {csp}"
    );
    assert!(
        !csp.contains("ws://localhost"),
        "CSP must not wildcard localhost: {csp}"
    );
    assert!(
        raw_body(resp)
            .await
            .contains(&format!("window.__HEARSAY_TOKEN__=\"{TOKEN}\"")),
        "the bootstrap must inject the token for an authenticated request"
    );
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
    let meeting = queries::create_meeting(&pool, "Standup", "2026-07-01_0900_standup", "", started)
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
async fn list_meetings_filters_by_folder_title_and_sort() {
    let (app, pool, _tmp) = setup().await;
    let base = chrono::Utc::now();
    let (_, folder) = send(&app, post("/api/folders", "{\"name\":\"Clients\"}")).await;
    let folder_id = folder["id"].as_str().unwrap().to_string();
    for (i, title) in ["Acme sync", "Acme review", "Internal standup"]
        .iter()
        .enumerate()
    {
        let started = base + chrono::Duration::seconds(i as i64);
        let meeting = queries::create_meeting(&pool, title, title, "", started)
            .await
            .unwrap();
        if title.starts_with("Acme") {
            send(
                &app,
                put(
                    &format!("/api/meetings/{}/folder", meeting.id),
                    &format!("{{\"folder_id\":\"{folder_id}\"}}"),
                ),
            )
            .await;
        }
    }

    // `total` counts the matches, not the database -- it is what drives the pager.
    let (status, body) = send(&app, get(&format!("/api/meetings?folder_id={folder_id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);

    let (_, body) = send(&app, get("/api/meetings?unfiled=true")).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["title"], "Internal standup");

    let (_, body) = send(&app, get("/api/meetings?q=acme")).await;
    assert_eq!(body["total"], 2);

    let (_, body) = send(&app, get("/api/meetings?sort=oldest")).await;
    assert_eq!(body["items"][0]["title"], "Acme sync");

    // A filter applies across pages, not within the page it returns.
    let (_, body) = send(&app, get("/api/meetings?q=acme&page=2&page_size=1")).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"][0]["title"], "Acme sync");

    let (status, _) = send(
        &app,
        get(&format!("/api/meetings?folder_id={folder_id}&unfiled=true")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = send(&app, get("/api/meetings?sort=sideways")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let long = "x".repeat(256);
    let (status, _) = send(&app, get(&format!("/api/meetings?q={long}"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn meeting_counts_cover_every_meeting_not_just_a_page() {
    let (app, pool, _tmp) = setup().await;
    let (_, folder) = send(&app, post("/api/folders", "{\"name\":\"Clients\"}")).await;
    let folder_id = folder["id"].as_str().unwrap().to_string();
    send(&app, post("/api/folders", "{\"name\":\"Empty\"}")).await;
    for i in 0..3 {
        let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
            .await
            .unwrap();
        if i < 2 {
            send(
                &app,
                put(
                    &format!("/api/meetings/{}/folder", meeting.id),
                    &format!("{{\"folder_id\":\"{folder_id}\"}}"),
                ),
            )
            .await;
        }
    }

    // A static segment, so this is the counts route and not a meeting id.
    let (status, body) = send(&app, get("/api/meetings/counts")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 3);
    assert_eq!(body["unfiled"], 1);
    let folders = body["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1, "a folder with no meetings has no entry");
    assert_eq!(folders[0]["folder_id"], folder_id.as_str());
    assert_eq!(folders[0]["meetings"], 2);
}

#[tokio::test]
async fn lists_segments_ordered_by_start() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Me,
        "Me",
        "second",
        5.0,
        6.0,
        None,
    )
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
        None,
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
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
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

/// A meeting with two Them clusters (one line each) plus a Me line, for the merge tests.
async fn seed_two_speakers(pool: &SqlitePool) -> (Uuid, Uuid, Uuid) {
    let meeting = queries::create_meeting(pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let c1 = queries::create_cluster(pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    let c2 = queries::create_cluster(pool, meeting.id, 2, false, None)
        .await
        .unwrap();
    queries::insert_segment(
        pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "one",
        0.0,
        1.0,
        Some(c1.id),
    )
    .await
    .unwrap();
    queries::insert_segment(
        pool,
        meeting.id,
        Stream::Them,
        "Speaker 2",
        "two",
        1.0,
        2.0,
        Some(c2.id),
    )
    .await
    .unwrap();
    queries::insert_segment(pool, meeting.id, Stream::Me, "Me", "mine", 2.0, 3.0, None)
        .await
        .unwrap();
    (meeting.id, c1.id, c2.id)
}

#[tokio::test]
async fn merges_two_speakers_within_a_meeting() {
    let (app, pool, _tmp) = setup().await;
    let (meeting_id, c1, c2) = seed_two_speakers(&pool).await;
    queries::rename_cluster(&pool, meeting_id, c2, "Alice")
        .await
        .unwrap();

    let (status, body) = send(
        &app,
        post(
            &format!("/api/meetings/{meeting_id}/speakers/{c1}/merge"),
            &format!("{{\"into\":\"{c2}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The source chip is gone and the survivor keeps its own ordinal — the gap left behind is
    // deliberate, since renumbering would relabel unrelated speakers.
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["label"], "Alice");
    assert_eq!(body["items"][0]["ordinal"], 2);

    let (status, segments) = send(&app, get(&format!("/api/meetings/{meeting_id}/segments"))).await;
    assert_eq!(status, StatusCode::OK);
    let them: Vec<&Value> = segments["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["stream"] == "them")
        .collect();
    assert_eq!(them.len(), 2);
    for segment in &them {
        assert_eq!(segment["cluster_id"], c2.to_string());
        assert_eq!(segment["speaker_label"], "Alice");
    }
    // Only the lines that actually moved are flagged: a merge is a bulk reassignment of the source,
    // and the target's own lines were never reassigned.
    let moved = them.iter().find(|s| s["text"] == "one").unwrap();
    let kept = them.iter().find(|s| s["text"] == "two").unwrap();
    assert_eq!(moved["edited"], true);
    assert_eq!(kept["edited"], false);
    // Me is not diarized, so a merge must not touch it.
    let me = segments["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stream"] == "me")
        .unwrap();
    assert_eq!(me["speaker_label"], "Me");
    assert_eq!(me["edited"], false);
    assert!(me["cluster_id"].is_null());
}

#[tokio::test]
async fn merge_rejects_self_target_and_foreign_clusters() {
    let (app, pool, _tmp) = setup().await;
    let (meeting_id, c1, _c2) = seed_two_speakers(&pool).await;
    let (other_meeting, other_cluster, _) = seed_two_speakers(&pool).await;

    let cases = [
        // Merging a speaker into itself is meaningless input, not a missing resource.
        (
            format!("/api/meetings/{meeting_id}/speakers/{c1}/merge"),
            format!("{{\"into\":\"{c1}\"}}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // The target comes from the body, so a target outside this meeting is a 422 ...
        (
            format!("/api/meetings/{meeting_id}/speakers/{c1}/merge"),
            format!("{{\"into\":\"{other_cluster}\"}}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            format!("/api/meetings/{meeting_id}/speakers/{c1}/merge"),
            format!("{{\"into\":\"{}\"}}", Uuid::new_v4()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // ... while the source comes from the path, so it is a 404.
        (
            format!(
                "/api/meetings/{meeting_id}/speakers/{}/merge",
                Uuid::new_v4()
            ),
            format!("{{\"into\":\"{c1}\"}}"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/api/meetings/{meeting_id}/speakers/{other_cluster}/merge"),
            format!("{{\"into\":\"{c1}\"}}"),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (uri, body, want) in cases {
        let (status, _) = send(&app, post(&uri, &body)).await;
        assert_eq!(status, want, "{uri} {body}");
    }

    // Nothing was moved by any of the rejections.
    let (_, speakers) = send(&app, get(&format!("/api/meetings/{meeting_id}/speakers"))).await;
    assert_eq!(speakers["total"], 2);
    let (_, other) = send(
        &app,
        get(&format!("/api/meetings/{other_meeting}/speakers")),
    )
    .await;
    assert_eq!(other["total"], 2);
}

#[tokio::test]
async fn merges_a_speaker_that_has_no_lines() {
    let (app, pool, _tmp) = setup().await;
    let (meeting_id, c1, _c2) = seed_two_speakers(&pool).await;
    let empty = queries::create_cluster(&pool, meeting_id, 3, false, None)
        .await
        .unwrap();

    let (status, body) = send(
        &app,
        post(
            &format!("/api/meetings/{meeting_id}/speakers/{}/merge", empty.id),
            &format!("{{\"into\":\"{c1}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2, "the ghost cluster is cleaned up");

    let (_, segments) = send(&app, get(&format!("/api/meetings/{meeting_id}/segments"))).await;
    assert_eq!(segments["total"], 3, "no line was touched");
}

#[tokio::test]
async fn renames_an_identity_across_every_meeting() {
    let (app, pool, _tmp) = setup().await;
    let (first, c1, _) = seed_two_speakers(&pool).await;
    let (second, c2, _) = seed_two_speakers(&pool).await;
    queries::rename_cluster(&pool, first, c1, "Alice")
        .await
        .unwrap();
    queries::rename_cluster(&pool, second, c2, "Alice")
        .await
        .unwrap();

    // Via /api/identities, not /api/voiceprints: these clusters have no centroid, so Alice is a
    // known person but not part of the voiceprint roster. Renaming must work for her all the same.
    let (_, identities) = send(&app, get("/api/identities")).await;
    let identity_id = identities["items"][0]["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        patch(
            &format!("/api/identities/{identity_id}"),
            "{\"display_name\":\"  Alicia  \"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], "Alicia");

    // Both meetings' stored labels follow, not just the one that was open.
    for meeting_id in [first, second] {
        let (_, segments) = send(&app, get(&format!("/api/meetings/{meeting_id}/segments"))).await;
        let labels: Vec<&str> = segments["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["speaker_label"].as_str().unwrap())
            .collect();
        assert!(labels.contains(&"Alicia"), "{meeting_id}: {labels:?}");
        assert!(!labels.contains(&"Alice"), "{meeting_id}: {labels:?}");
    }
    // One person, renamed — not a second identity.
    let (_, identities) = send(&app, get("/api/identities")).await;
    assert_eq!(identities["total"], 1);
}

#[tokio::test]
async fn identity_rename_conflicts_and_validates() {
    let (app, pool, _tmp) = setup().await;
    let (meeting_id, c1, c2) = seed_two_speakers(&pool).await;
    queries::rename_cluster(&pool, meeting_id, c1, "Alice")
        .await
        .unwrap();
    queries::rename_cluster(&pool, meeting_id, c2, "Bob")
        .await
        .unwrap();

    let (_, identities) = send(&app, get("/api/identities")).await;
    let by_name = |name: &str| -> String {
        identities["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["display_name"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let alice = by_name("Alice");

    // Taking a name already held by someone else is a conflict, never a silent identity merge.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/identities/{alice}"),
            "{\"display_name\":\"Bob\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Re-applying the name they already have is a no-op success.
    let (status, body) = send(
        &app,
        patch(
            &format!("/api/identities/{alice}"),
            "{\"display_name\":\"Alice\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], "Alice");

    for bad in ["{\"display_name\":\"\"}", "{\"display_name\":\"   \"}"] {
        let (status, _) = send(&app, patch(&format!("/api/identities/{alice}"), bad)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/identities/{}", Uuid::new_v4()),
            "{\"display_name\":\"Zed\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn lists_voiceprints_with_their_samples() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "Standup", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let centroid = hearsay_attribution::centroid_to_bytes(&[0.6, 0.8]);
    let bound = queries::create_cluster(&pool, meeting.id, 1, false, Some(centroid))
        .await
        .unwrap();
    queries::rename_cluster(&pool, meeting.id, bound.id, "Alice")
        .await
        .unwrap();
    // A person named in a meeting that was never refined has no centroid. They are a known identity
    // but not a voiceprint, so the roster must leave them out — a row with nothing to match on can
    // neither be explained nor acted on.
    let unrefined = queries::create_cluster(&pool, meeting.id, 2, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, meeting.id, unrefined.id, "Bob")
        .await
        .unwrap();

    let (status, body) = send(&app, get("/api/voiceprints")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    let people = body["items"].as_array().unwrap();
    assert_eq!(people.len(), 1);
    let alice = &people[0];
    assert_eq!(alice["display_name"], "Alice");
    assert_eq!(alice["sample_count"], 1);
    assert_eq!(alice["active_count"], 1);
    assert!(alice["last_heard"].is_string());
    assert_eq!(alice["samples"][0]["meeting_title"], "Standup");
    assert_eq!(alice["samples"][0]["dimension"], 2);
    assert_eq!(alice["samples"][0]["locked"], true);

    // Bob is still a known person for the rename autocomplete; he just has no voice on file.
    let (_, identities) = send(&app, get("/api/identities")).await;
    assert_eq!(identities["total"], 2);
}

#[tokio::test]
async fn deleting_a_sample_stops_recognition_but_keeps_the_transcript() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let centroid = hearsay_attribution::centroid_to_bytes(&[0.6, 0.8]);
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, Some(centroid))
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        meeting.id,
        Stream::Them,
        "Speaker 1",
        "hello",
        0.0,
        1.0,
        Some(cluster.id),
    )
    .await
    .unwrap();
    queries::rename_cluster(&pool, meeting.id, cluster.id, "Alice")
        .await
        .unwrap();
    // Recognition sees her from any other meeting.
    assert_eq!(
        queries::known_voiceprints(&pool, Uuid::new_v4())
            .await
            .unwrap()
            .len(),
        1
    );

    let (status, _) = send(&app, del(&format!("/api/voiceprints/{}", cluster.id))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // The whole point: the voice stops matching ...
    assert!(queries::known_voiceprints(&pool, Uuid::new_v4())
        .await
        .unwrap()
        .is_empty());
    // ... while the name stays on the transcript and the binding stays locked.
    let (_, speakers) = send(&app, get(&format!("/api/meetings/{}/speakers", meeting.id))).await;
    assert_eq!(speakers["items"][0]["label"], "Alice");
    assert_eq!(speakers["items"][0]["locked"], true);
    let (_, segments) = send(&app, get(&format!("/api/meetings/{}/segments", meeting.id))).await;
    assert_eq!(segments["items"][0]["speaker_label"], "Alice");

    // Clearing an already-cleared voiceprint is idempotent, not a 404.
    let (status, _) = send(&app, del(&format!("/api/voiceprints/{}", cluster.id))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, del(&format!("/api/voiceprints/{}", Uuid::new_v4()))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn forgetting_a_voice_clears_every_sample() {
    let (app, pool, _tmp) = setup().await;
    let centroid = hearsay_attribution::centroid_to_bytes(&[0.6, 0.8]);
    for name in ["one", "two"] {
        let meeting = queries::create_meeting(&pool, name, name, "", chrono::Utc::now())
            .await
            .unwrap();
        let cluster = queries::create_cluster(&pool, meeting.id, 1, false, Some(centroid.clone()))
            .await
            .unwrap();
        queries::rename_cluster(&pool, meeting.id, cluster.id, "Alice")
            .await
            .unwrap();
    }
    let (_, body) = send(&app, get("/api/voiceprints")).await;
    assert_eq!(body["items"][0]["sample_count"], 2);
    let identity_id = body["items"][0]["identity_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = send(
        &app,
        del(&format!("/api/identities/{identity_id}/voiceprint")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // With no embedding left she drops off the roster entirely — otherwise the panel would keep a
    // row that matches nothing and offers nothing to remove.
    let (_, body) = send(&app, get("/api/voiceprints")).await;
    assert_eq!(body["total"], 0);
    assert!(body["items"].as_array().unwrap().is_empty());
    assert!(queries::known_voiceprints(&pool, Uuid::new_v4())
        .await
        .unwrap()
        .is_empty());
    // She is not deleted, though: her name still labels every past transcript line.
    let (_, identities) = send(&app, get("/api/identities")).await;
    assert_eq!(identities["items"][0]["display_name"], "Alice");

    let (status, _) = send(
        &app,
        del(&format!("/api/identities/{}/voiceprint", Uuid::new_v4())),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unbound_speaker_falls_back_to_ordinal_label() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
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
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let cluster = queries::create_cluster(&pool, meeting.id, 1, false, None)
        .await
        .unwrap();
    queries::rename_cluster(&pool, cluster.meeting_id, cluster.id, "Carol")
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

    let (status, _) = send(
        &app,
        post(&format!("/api/meetings/{}/rediarize", Uuid::new_v4()), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(
        &app,
        post(&format!("/api/meetings/{}/rediarize", meeting.id), ""),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn audio_checks_the_query_token_and_404s_without_a_file() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
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
async fn archived_audio_plays_as_wav_and_seeks_to_the_exact_bytes() {
    let (app, pool, tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    // Silence then a ramp then silence: uneven compression, and every sample is identifiable.
    let mut samples = vec![0i16; 2 * 40_000];
    samples.extend((0..2 * 20_000).map(|i| (i % 30_000) as i16 - 15_000));
    samples.extend(vec![0i16; 2 * 9_999]);
    let dir = tmp.path().join("f");
    std::fs::create_dir_all(&dir).unwrap();
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(dir.join("source.wav"), spec).unwrap();
    for s in &samples {
        writer.write_sample(*s).unwrap();
    }
    writer.finalize().unwrap();
    hearsay_audio::encode_wav_to_flac(&dir.join("source.wav"), &dir.join("audio.flac")).unwrap();
    std::fs::remove_file(dir.join("source.wav")).unwrap();
    let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let wav_len = 44 + pcm.len();
    let uri = format!("/api/meetings/{}/audio?token={TOKEN}", meeting.id);

    let full = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(full.headers()["content-type"], "audio/wav");
    assert_eq!(
        full.headers()["content-length"],
        wav_len.to_string().as_str()
    );
    let body = axum::body::to_bytes(full.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[44..], &pcm[..]);

    // A seek into the ramp returns exactly the bytes at that offset.
    let (start, end) = (44 + 4 * 41_000, 44 + 4 * 52_345 + 1);
    let mut ranged = get(&uri);
    ranged
        .headers_mut()
        .insert("range", format!("bytes={start}-{end}").parse().unwrap());
    let resp = app.clone().oneshot(ranged).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers()["content-range"],
        format!("bytes {start}-{end}/{wav_len}").as_str()
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], &pcm[start - 44..=end - 44]);

    let mut past_end = get(&uri);
    past_end
        .headers_mut()
        .insert("range", format!("bytes={wav_len}-").parse().unwrap());
    let resp = app.clone().oneshot(past_end).await.unwrap();
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn deletes_a_meeting_then_404s() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();

    let (status, _) = send(&app, del(&format!("/api/meetings/{}", meeting.id))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, del(&format!("/api/meetings/{}", meeting.id))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn renames_a_meeting_and_validates_the_title() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "Old name", "f", "", chrono::Utc::now())
        .await
        .unwrap();

    let (status, body) = send(
        &app,
        patch(
            &format!("/api/meetings/{}", meeting.id),
            "{\"title\":\"  New name  \"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Trimmed and returned; the list reflects it too.
    assert_eq!(body["title"], "New name");
    let (_, listed) = send(&app, get("/api/meetings")).await;
    assert_eq!(listed["items"][0]["title"], "New name");

    // A blank title is a 422 + the `{ "detail": ... }` envelope (never let the DB store an empty
    // name); input validation is 422 across the API, not a plain 400.
    let (status, body) = send(
        &app,
        patch(
            &format!("/api/meetings/{}", meeting.id),
            "{\"title\":\"   \"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());

    // An unknown id is a 404.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}", Uuid::new_v4()),
            "{\"title\":\"x\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_json_is_served() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["openapi"].is_string());
    assert!(body["paths"]["/api/meetings"].is_object());
    // The active-meeting guards (R2) document a 409 on delete + rediarize.
    assert!(body["paths"]["/api/meetings/{id}"]["delete"]["responses"]["409"].is_object());
    assert!(body["paths"]["/api/meetings/{id}/rediarize"]["post"]["responses"]["409"].is_object());
    // Input validation is documented as 422 (not 400): rename (PATCH) + start (POST).
    assert!(body["paths"]["/api/meetings/{id}"]["patch"]["responses"]["422"].is_object());
    assert!(body["paths"]["/api/meetings/{id}"]["patch"]["responses"]["400"].is_null());
    assert!(body["paths"]["/api/meetings"]["post"]["responses"]["422"].is_object());
    // The WS event frames are modeled + registered so they codegen into the TS client.
    assert!(body["components"]["schemas"]["TranscriptEvent"].is_object());
    assert!(body["components"]["schemas"]["StatusEvent"].is_object());
    assert!(body["components"]["schemas"]["ResyncEvent"].is_object());
    // The folder endpoints + schemas are registered so they codegen into the TS client.
    assert!(body["paths"]["/api/folders"]["get"].is_object());
    assert!(body["paths"]["/api/folders"]["post"].is_object());
    assert!(body["paths"]["/api/folders/{id}"]["patch"].is_object());
    assert!(body["paths"]["/api/folders/{id}"]["delete"].is_object());
    assert!(body["paths"]["/api/folders/{id}/parent"]["put"].is_object());
    assert!(body["paths"]["/api/meetings/{id}/folder"]["put"].is_object());
    assert!(body["components"]["schemas"]["FolderRead"].is_object());
    assert!(body["components"]["schemas"]["FolderCreate"].is_object());
    assert!(body["components"]["schemas"]["FolderReparent"].is_object());
    assert!(body["components"]["schemas"]["MeetingFolderAssign"].is_object());
}

#[tokio::test]
async fn extractor_rejections_are_422_with_the_detail_envelope() {
    // axum's default extractor rejections are plain-text 400s that bypass the `{ "detail": ... }`
    // envelope; the wrapper extractors in `crate::extract` map them to a uniform 422 envelope.
    let (app, _pool, _tmp) = setup().await;

    // A bad UUID in a `Path`.
    let (status, body) = send(&app, get("/api/meetings/not-a-uuid")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());

    // An unparseable `Query` value (`page` is a u32).
    let (status, body) = send(&app, get("/api/meetings?page=abc")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());

    // A malformed JSON body — extraction fails before the handler (so before the engine 503).
    let malformed = post("/api/meetings", "{\"title\": ");
    let (status, body) = send(&app, malformed).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());
}

#[tokio::test]
async fn folders_crud_with_the_page_envelope() {
    let (app, _pool, _tmp) = setup().await;

    let (status, work) = send(&app, post("/api/folders", "{\"name\":\"  Work  \"}")).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(work["name"], "Work"); // trimmed
    assert!(work["parent_id"].is_null());
    let work_id = work["id"].as_str().unwrap().to_string();

    let (status, project) = send(
        &app,
        post(
            "/api/folders",
            &format!("{{\"name\":\"Project\",\"parent_id\":\"{work_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(project["parent_id"], work_id.as_str());

    // The shared { total, page, page_size, items } envelope, name-ordered ("Project" < "Work").
    let (status, page) = send(&app, get("/api/folders")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 2);
    assert_eq!(page["page"], 1);
    assert_eq!(page["page_size"], 200);
    assert_eq!(page["items"][0]["name"], "Project");
    assert_eq!(page["items"][1]["name"], "Work");

    // Rename trims and returns the updated row.
    let (status, renamed) = send(
        &app,
        patch(
            &format!("/api/folders/{work_id}"),
            "{\"name\":\"  Work stuff \"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "Work stuff");
}

#[tokio::test]
async fn folder_endpoints_validate_and_404() {
    let (app, _pool, _tmp) = setup().await;

    // A blank name is a 422 + `{ "detail": ... }` envelope (never store an empty name).
    let (status, body) = send(&app, post("/api/folders", "{\"name\":\"   \"}")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["detail"].is_string());

    // Nesting under a non-existent parent is a 422.
    let (status, _) = send(
        &app,
        post(
            "/api/folders",
            &format!("{{\"name\":\"x\",\"parent_id\":\"{}\"}}", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Rename / delete of an unknown id is a 404.
    let missing = Uuid::new_v4();
    let (status, _) = send(
        &app,
        patch(&format!("/api/folders/{missing}"), "{\"name\":\"y\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, del(&format!("/api/folders/{missing}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reparent_folder_endpoint_guards_cycles() {
    let (app, _pool, _tmp) = setup().await;
    let (_, a) = send(&app, post("/api/folders", "{\"name\":\"A\"}")).await;
    let (_, b) = send(&app, post("/api/folders", "{\"name\":\"B\"}")).await;
    let a_id = a["id"].as_str().unwrap().to_string();
    let b_id = b["id"].as_str().unwrap().to_string();

    // Move B under A.
    let (status, moved) = send(
        &app,
        put(
            &format!("/api/folders/{b_id}/parent"),
            &format!("{{\"parent_id\":\"{a_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(moved["parent_id"], a_id.as_str());

    // Into itself is a 422.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/folders/{a_id}/parent"),
            &format!("{{\"parent_id\":\"{a_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Into a descendant (A under its own child B) is a 422 cycle.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/folders/{a_id}/parent"),
            &format!("{{\"parent_id\":\"{b_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A non-existent parent is a 422.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/folders/{a_id}/parent"),
            &format!("{{\"parent_id\":\"{}\"}}", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Detaching B to the root is allowed.
    let (status, detached) = send(
        &app,
        put(
            &format!("/api/folders/{b_id}/parent"),
            "{\"parent_id\":null}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(detached["parent_id"].is_null());

    // An unknown folder is a 404.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/folders/{}/parent", Uuid::new_v4()),
            "{\"parent_id\":null}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn files_and_unfiles_a_meeting() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "Kickoff", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (_, folder) = send(&app, post("/api/folders", "{\"name\":\"Clients\"}")).await;
    let folder_id = folder["id"].as_str().unwrap().to_string();

    // File the meeting; the returned row and the list both reflect the assignment.
    let (status, filed) = send(
        &app,
        put(
            &format!("/api/meetings/{}/folder", meeting.id),
            &format!("{{\"folder_id\":\"{folder_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filed["folder_id"], folder_id.as_str());
    let (_, listed) = send(&app, get("/api/meetings")).await;
    assert_eq!(listed["items"][0]["folder_id"], folder_id.as_str());

    // Filing under a non-existent folder is a 422.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/meetings/{}/folder", meeting.id),
            &format!("{{\"folder_id\":\"{}\"}}", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A null folder_id un-files it.
    let (status, cleared) = send(
        &app,
        put(
            &format!("/api/meetings/{}/folder", meeting.id),
            "{\"folder_id\":null}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared["folder_id"].is_null());

    // An unknown meeting is a 404.
    let (status, _) = send(
        &app,
        put(
            &format!("/api/meetings/{}/folder", Uuid::new_v4()),
            "{\"folder_id\":null}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_folder_unfiles_its_meetings() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (_, folder) = send(&app, post("/api/folders", "{\"name\":\"Temp\"}")).await;
    let folder_id = folder["id"].as_str().unwrap().to_string();
    send(
        &app,
        put(
            &format!("/api/meetings/{}/folder", meeting.id),
            &format!("{{\"folder_id\":\"{folder_id}\"}}"),
        ),
    )
    .await;

    let (status, _) = send(&app, del(&format!("/api/folders/{folder_id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The meeting survives, merely un-filed.
    let (_, listed) = send(&app, get("/api/meetings")).await;
    assert_eq!(listed["total"], 1);
    assert!(listed["items"][0]["folder_id"].is_null());
}

#[tokio::test]
async fn reads_settings_with_config_defaults_and_about() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(status, StatusCode::OK);
    // No stored overrides yet, so every section resolves to the `Settings` default from `setup()`.
    assert_eq!(body["recording"]["record"], true);
    assert_eq!(body["speakers"]["auto_refine"], false);
    assert_eq!(body["speakers"]["recognition_threshold"], 0.6);
    assert_eq!(body["storage_info"]["meeting_count"], 0);
    assert!(body["about"]["app_version"].is_string());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn permissions_probe_degrades_when_helper_missing() {
    // `setup()` points `helper_path` at a nonexistent file, so the probe reports unavailable.
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/api/settings/permissions")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["helper_available"], false);
    assert_eq!(body["helper_version"], Value::Null);
    assert_eq!(body["microphone"], "unknown");
    assert_eq!(body["audio_capture"], "unknown");
}

// --- Live-transcript WebSocket handshake gating (routes/ws.rs) -------------------------------------
// Auth mirrors REST but over the handshake: a loopback Origin + the session token as `?token=`. Driven
// with a real WS client against a server on an ephemeral port, because axum's `WebSocketUpgrade`
// extractor runs before the handler body and cannot be driven by `oneshot` (it rejects with 426).
// Delivering the actual transcript/status frames additionally needs a scripted engine, covered by
// the orchestrator's streaming-pipeline tests.
type WsRequest = tokio_tungstenite::tungstenite::handshake::client::Request;

/// Serve the app (`DisabledEngine`) on an ephemeral loopback port; returns the `host:port` authority
/// and the `TempDir` kept alive for the test's duration.
async fn serve_ws() -> (String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let settings = test_settings(tmp.path().to_path_buf(), tmp.path().join("no-web"));
    let state = AppState::new(
        memory_pool().await,
        settings,
        TOKEN.to_string(),
        Arc::new(DisabledEngine),
        Default::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, create_app(state).into_make_service())
            .await
            .unwrap();
    });
    (authority, tmp)
}

/// A WebSocket handshake request for a meeting, with the given token and Origin.
fn ws_request(authority: &str, id: Uuid, token: Option<&str>, origin: &str) -> WsRequest {
    let url = match token {
        Some(t) => format!("ws://{authority}/ws/meetings/{id}?token={t}"),
        None => format!("ws://{authority}/ws/meetings/{id}"),
    };
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert(ws_header::ORIGIN, origin.parse().unwrap());
    req
}

#[tokio::test]
async fn ws_handshake_rejects_a_non_loopback_origin() {
    let (authority, _tmp) = serve_ws().await;
    let req = ws_request(
        &authority,
        Uuid::new_v4(),
        Some(TOKEN),
        "https://evil.example",
    );
    match connect_async(req).await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 403),
        Ok(_) => panic!("a cross-site Origin must not complete the handshake"),
        Err(other) => panic!("expected HTTP 403, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_handshake_rejects_a_missing_or_wrong_token() {
    let (authority, _tmp) = serve_ws().await;
    for token in [None, Some("not-the-token")] {
        let req = ws_request(&authority, Uuid::new_v4(), token, "http://127.0.0.1:5173");
        match connect_async(req).await {
            Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 401, "token {token:?}"),
            Ok(_) => panic!("token {token:?} must not complete the handshake"),
            Err(other) => panic!("token {token:?}: expected HTTP 401, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn ws_handshake_accepts_a_valid_loopback_token() {
    let (authority, _tmp) = serve_ws().await;
    let req = ws_request(
        &authority,
        Uuid::new_v4(),
        Some(TOKEN),
        "http://127.0.0.1:5173",
    );
    // A valid handshake upgrades (101); `DisabledEngine` then closes it cleanly. Reaching 101 proves
    // the Origin + token gates let it through.
    let (_stream, resp) = connect_async(req).await.expect("valid handshake accepted");
    assert_eq!(resp.status().as_u16(), 101);
}

// --- Full-stack HTTP + WS integration over a real socket (routes + orchestrator + persistence) ----
// One shared AppState with a model-free scripted Orchestrator engine: HTTP is driven with `oneshot`,
// the live transcript over a real WS client against a TCP server (the two routers share the engine +
// pool Arcs). The scripted transcribers emit at stop, so a socket connected mid-meeting must be
// subscribed before the stop broadcasts; pausing first yields a deterministic `capture_state: paused`
// connect snapshot to synchronize on (a broadcast receiver only sees sends made after it subscribed).

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Next JSON text frame off the socket (skipping ping/pong), or `None` once it closes. Bounded so a
/// hang fails fast instead of blocking the suite.
async fn next_ws_json(socket: &mut WsStream) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("WS read timed out");
        match msg {
            Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Close(_))) | None => return None,
            Some(Ok(_)) => continue,
            // The server drops the socket when the meeting's broadcast closes, which arrives as a
            // TCP reset rather than a WS Close frame — treat that (and the closed states) as the
            // end of the stream, not a failure.
            Some(Err(
                WsError::ConnectionClosed
                | WsError::AlreadyClosed
                | WsError::Protocol(
                    tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                ),
            )) => return None,
            Some(Err(err)) => panic!("WS error: {err}"),
        }
    }
}

#[tokio::test]
async fn full_stack_meeting_drives_events_and_persistence() {
    // Me anchors t0; Them arrives +0.5s. The scripted segments (and thus the WS frames + persisted
    // rows) are emitted at stop.
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (backend, _fed) = ScriptedBackend::new(
        vec![
            chunk(Stream::Me, 1_000_000_000, &[0.1, 0.2]),
            chunk(Stream::Them, 1_500_000_000, &[0.3, 0.4, 0.5]),
        ],
        vec![
            seg(SegmentKind::Partial, "hello", 0.0, 0.5, None),
            seg(SegmentKind::Final, "hello there", 0.0, 1.0, None),
        ],
        vec![
            seg(SegmentKind::Partial, "hi", 0.0, 0.5, None),
            seg(SegmentKind::Final, "hi everyone", 1.0, 2.0, Some(0)),
        ],
    );
    let engine: Arc<dyn LiveEngine> =
        Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend).into_arc();
    let settings = test_settings(tmp.path().to_path_buf(), tmp.path().join("no-web"));
    let state = AppState::new(
        pool.clone(),
        settings,
        TOKEN.to_string(),
        engine,
        Default::default(),
    );

    let app = create_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, create_app(state).into_make_service())
            .await
            .unwrap();
    });

    // Start the meeting over HTTP.
    let (status, meeting) = send(&app, post("/api/meetings", r#"{"title":"Full Stack"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(meeting["status"], "recording");
    let id = Uuid::parse_str(meeting["id"].as_str().unwrap()).unwrap();

    // A per-line speaker reassignment is refused (409) while the meeting is still recording — the
    // guard short-circuits before any segment lookup, so a placeholder id suffices.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{id}/segments/{}/speaker", Uuid::new_v4()),
            r#"{"display_name":"Nope"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Same for a speaker merge: the diarizer is still creating and dropping clusters, so folding two
    // together mid-meeting would race it. Also guarded before any lookup.
    let (status, _) = send(
        &app,
        post(
            &format!("/api/meetings/{id}/speakers/{}/merge", Uuid::new_v4()),
            &format!("{{\"into\":\"{}\"}}", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // And archiving on demand: encoding is CPU work, so it must never compete with live capture.
    let (status, body) = send(&app, post("/api/settings/storage/compress", "")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["detail"]
        .as_str()
        .is_some_and(|d| d.contains("recording")));

    // Pause so the WS connect yields a deterministic paused snapshot — our subscribe barrier.
    let (status, _) = send(&app, post(&format!("/api/meetings/{id}/pause"), "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (mut socket, _resp) = connect_async(ws_request(
        &authority,
        id,
        Some(TOKEN),
        "http://127.0.0.1:5173",
    ))
    .await
    .expect("ws handshake");
    let snapshot = next_ws_json(&mut socket)
        .await
        .expect("paused snapshot frame");
    assert_eq!(snapshot["kind"], "capture_state");
    assert_eq!(snapshot["state"], "paused");

    // Resume, then stop: the four scripted transcript frames now broadcast to the subscribed socket.
    send(&app, post(&format!("/api/meetings/{id}/resume"), "")).await;
    let (status, stopped) = send(&app, post(&format!("/api/meetings/{id}/stop"), "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["status"], "finalized");

    // Drain the live transcript frames off the socket until it closes.
    let mut events = Vec::new();
    while let Some(frame) = next_ws_json(&mut socket).await {
        if frame["kind"] == "partial" || frame["kind"] == "final" {
            events.push(frame);
        }
    }
    let find = |kind: &str, stream: &str| {
        events
            .iter()
            .find(|e| e["kind"] == kind && e["stream"] == stream)
            .unwrap_or_else(|| panic!("missing {kind}/{stream} frame over the WS"))
    };
    assert_eq!(find("final", "me")["text"], "hello there");
    let them_final = find("final", "them");
    assert_eq!(them_final["text"], "hi everyone");
    assert_eq!(them_final["speaker_label"], "Speaker 1");
    assert!(events
        .iter()
        .any(|e| e["kind"] == "partial" && e["stream"] == "me"));

    // Read the persisted result back over HTTP: two finals, Them bound to a Speaker 1 cluster.
    let (status, segs) = send(&app, get(&format!("/api/meetings/{id}/segments"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(segs["items"].as_array().unwrap().len(), 2);
    let (status, speakers) = send(&app, get(&format!("/api/meetings/{id}/speakers"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(speakers["items"][0]["label"], "Speaker 1");
}

#[tokio::test]
async fn full_stack_refine_replaces_them_segments_on_read_back() {
    // The live path yields a single Them "Speaker 1" final; a wired refiner re-diarizes it into two
    // speakers at stop. Everything is driven over the HTTP API and read back over it.
    let pool = memory_pool().await;
    let tmp = tempfile::tempdir().unwrap();
    let (backend, _fed) = ScriptedBackend::new(
        vec![chunk(Stream::Them, 1_000_000_000, &[0.1, 0.2, 0.3])],
        vec![seg(SegmentKind::Final, "me kept", 0.0, 1.0, None)],
        vec![seg(SegmentKind::Final, "live guess", 0.0, 2.0, Some(0))],
    );
    let (refiner, calls) = ScriptedRefiner::new(vec![
        RefinedThemSegment {
            ordinal: 1,
            text: "refined one".into(),
            start_s: 0.0,
            end_s: 1.0,
        },
        RefinedThemSegment {
            ordinal: 2,
            text: "refined two".into(),
            start_s: 1.0,
            end_s: 2.0,
        },
    ]);
    // Keep the concrete Arc so the test can await the background refine; it coerces to the trait
    // object AppState wants.
    let orch = Orchestrator::new(pool.clone(), tmp.path().to_path_buf(), backend)
        .with_refiner(refiner)
        .into_arc();
    let settings = test_settings(tmp.path().to_path_buf(), tmp.path().join("no-web"));
    let state = AppState::new(
        pool.clone(),
        settings,
        TOKEN.to_string(),
        orch.clone(),
        Default::default(),
    );
    let app = create_app(state);

    // Start over HTTP, then drop a placeholder audio.wav into the meeting folder — the refiner ignores
    // its content, but the auto-refine at stop only runs when a recording exists.
    let (status, meeting) = send(&app, post("/api/meetings", r#"{"title":"Refine"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = Uuid::parse_str(meeting["id"].as_str().unwrap()).unwrap();
    let folder = queries::get_meeting(&pool, id)
        .await
        .unwrap()
        .unwrap()
        .folder;
    std::fs::write(tmp.path().join(&folder).join("audio.wav"), b"placeholder").unwrap();

    // Stop returns an interim "refining"; the refine runs in the background, then finalizes.
    let (status, stopped) = send(&app, post(&format!("/api/meetings/{id}/stop"), "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["status"], "refining");
    orch.wait_for_refines().await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "auto-refine ran once at stop"
    );
    assert_eq!(
        queries::get_meeting(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .status,
        hearsay_db::models::MeetingStatus::Finalized
    );

    // Read back over HTTP: the live Them guess is replaced by the refiner's two segments, the Me track
    // is untouched, and the refined speakers surface on the speakers endpoint.
    let (status, segs) = send(&app, get(&format!("/api/meetings/{id}/segments"))).await;
    assert_eq!(status, StatusCode::OK);
    let items = segs["items"].as_array().unwrap();
    let them: Vec<&Value> = items.iter().filter(|s| s["stream"] == "them").collect();
    assert_eq!(
        them.len(),
        2,
        "the single live Them final is replaced by the two refined segments"
    );
    assert_eq!(them[0]["text"], "refined one");
    assert_eq!(them[1]["text"], "refined two");
    assert!(
        !items.iter().any(|s| s["text"] == "live guess"),
        "the pre-refine Them guess is gone"
    );
    let me = items
        .iter()
        .find(|s| s["stream"] == "me")
        .expect("the Me segment");
    assert_eq!(
        me["text"], "me kept",
        "the Me track is untouched by the refine"
    );

    let (status, speakers) = send(&app, get(&format!("/api/meetings/{id}/speakers"))).await;
    assert_eq!(status, StatusCode::OK);
    let labels: Vec<&str> = speakers["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["label"].as_str().unwrap())
        .collect();
    assert!(
        labels.contains(&"Speaker 1") && labels.contains(&"Speaker 2"),
        "both refined speakers read back: {labels:?}"
    );

    // Manual speaker corrections must reach the exported transcript, not just the DB. This is the
    // only test with a real Orchestrator writing into a real output dir — every other one runs on
    // DisabledEngine, whose export is a no-op — so it is the only place the re-export can be caught.
    let transcript = tmp.path().join(&folder).join("transcript.md");
    let cluster_id = speakers["items"][0]["id"].as_str().unwrap();
    let (status, _) = send(
        &app,
        put(
            &format!("/api/meetings/{id}/speakers/{cluster_id}"),
            r#"{"display_name":"Dana"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let written = std::fs::read_to_string(&transcript).unwrap();
    assert!(
        written.contains("Dana"),
        "renaming a speaker rewrites transcript.md: {written}"
    );

    // Likewise for a merge, which relabels every line of the folded-in speaker.
    let (status, after) = send(
        &app,
        post(
            &format!(
                "/api/meetings/{id}/speakers/{}/merge",
                speakers["items"][1]["id"].as_str().unwrap()
            ),
            &format!("{{\"into\":\"{cluster_id}\"}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["total"], 1);
    let written = std::fs::read_to_string(&transcript).unwrap();
    assert!(
        !written.contains("Speaker 2"),
        "the merged-away speaker is gone from transcript.md: {written}"
    );
}

#[tokio::test]
async fn updates_recording_and_persists_the_override() {
    let (app, _pool, _tmp) = setup().await;
    // Callers send the whole `recording` section (the server full-replaces it).
    let (status, body) = send(
        &app,
        put(
            "/api/settings/recording",
            "{\"record\":false,\"inactivity_prompt_enabled\":true,\
             \"inactivity_auto_end_enabled\":false,\
             \"inactivity_prompt_minutes\":7,\"inactivity_end_minutes\":12}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["record"], false);
    assert_eq!(body["inactivity_auto_end_enabled"], false);
    assert_eq!(body["inactivity_prompt_minutes"], 7);

    // The stored override now wins over the config default on the next read.
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["recording"]["record"], false);
    assert_eq!(body["recording"]["inactivity_auto_end_enabled"], false);
    assert_eq!(body["recording"]["inactivity_end_minutes"], 12);
}

#[tokio::test]
async fn rejects_inactivity_end_not_after_prompt() {
    let (app, _pool, _tmp) = setup().await;
    // With both toggles on, the auto-end threshold must exceed the prompt threshold; an inverted
    // pair is a 422.
    let (status, _) = send(
        &app,
        put(
            "/api/settings/recording",
            "{\"record\":true,\"inactivity_prompt_enabled\":true,\
             \"inactivity_auto_end_enabled\":true,\
             \"inactivity_prompt_minutes\":10,\"inactivity_end_minutes\":10}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn rejects_out_of_range_recognition_threshold() {
    let (app, _pool, _tmp) = setup().await;
    let (status, _) = send(
        &app,
        put(
            "/api/settings/speakers",
            "{\"auto_refine\":true,\"recognition_threshold\":2.0}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// A `storage` row written before audio archival existed carries only `output_dir`. Deserializing
/// the section as a struct would resolve the absent keys to their type defaults (`false` / `0`),
/// shipping archival disabled on precisely the installs with the most audio to reclaim. Guard that
/// each field falls back to its config default instead.
#[tokio::test]
async fn compress_now_starts_a_pass_and_reports_progress() {
    let (app, _pool, _tmp) = setup().await;

    // Idle before anything is asked for.
    let (status, body) = send(&app, get("/api/settings/storage/compress")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["running"], false);
    assert_eq!(body["compressed"], 0);

    // The button hands the work to the background and answers immediately -- a backlog takes far
    // longer than a request should hold.
    let (status, body) = send(&app, post("/api/settings/storage/compress", "")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(body["total"].is_number());
    assert!(body["reclaimed_bytes"].is_i64());
}

#[tokio::test]
async fn settings_survive_a_legacy_storage_preference() {
    let (app, pool, _tmp) = setup().await;
    hearsay_db::queries::set_preference(
        &pool,
        hearsay_db::queries::SECTION_STORAGE,
        r#"{"output_dir":"/legacy/recordings"}"#,
    )
    .await
    .unwrap();

    let (status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["storage"]["output_dir"], "/legacy/recordings");
    // The fields the row predates come from the config defaults, so archival ships on rather than
    // silently disabled.
    assert_eq!(body["storage"]["compress_audio"], true);
    assert_eq!(body["storage"]["compress_after_days"], 7);
    // And the rest of the page still rendered.
    assert!(body["recording"].is_object());
    assert!(body["models"].is_object());
    assert!(body["storage_info"]["uncompressed_bytes"].is_i64());
}

#[tokio::test]
async fn storage_update_round_trips_compression_and_validates_it() {
    let (app, _pool, tmp) = setup().await;
    let dir = serde_json::to_string(&tmp.path().to_string_lossy()).unwrap();

    let (status, body) = send(
        &app,
        put(
            "/api/settings/storage",
            &format!("{{\"output_dir\":{dir},\"compress_audio\":true,\"compress_after_days\":30}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["compress_audio"], true);
    assert_eq!(body["compress_after_days"], 30);

    // And it survives a re-read.
    let (_, settings) = send(&app, get("/api/settings")).await;
    assert_eq!(settings["storage"]["compress_after_days"], 30);

    // Zero would race the post-stop refine, which is still reading the WAV.
    for days in ["0", "400"] {
        let (status, _) = send(
            &app,
            put(
                "/api/settings/storage",
                &format!(
                    "{{\"output_dir\":{dir},\"compress_audio\":true,\"compress_after_days\":{days}}}"
                ),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "compress_after_days {days} should be rejected"
        );
    }

    // A disabled threshold is inert, so it is not checked.
    let (status, _) = send(
        &app,
        put(
            "/api/settings/storage",
            &format!("{{\"output_dir\":{dir},\"compress_audio\":false,\"compress_after_days\":0}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// The PUT full-replaces the section, so a body that omits the archival fields must be rejected —
/// accepting it would silently switch archival off while the user was only changing the folder.
#[tokio::test]
async fn storage_update_rejects_a_partial_body() {
    let (app, _pool, tmp) = setup().await;
    let dir = serde_json::to_string(&tmp.path().to_string_lossy()).unwrap();

    let (status, _) = send(
        &app,
        put(
            "/api/settings/storage",
            &format!("{{\"output_dir\":{dir}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // And the stored policy is unchanged.
    let (_, settings) = send(&app, get("/api/settings")).await;
    assert_eq!(settings["storage"]["compress_audio"], true);
    assert_eq!(settings["storage"]["compress_after_days"], 7);
}

#[tokio::test]
async fn validates_output_dir_on_storage_update() {
    let (app, _pool, tmp) = setup().await;

    // A nonexistent absolute path is rejected at the boundary (422, not a DB 500).
    let (status, _) = send(
        &app,
        put(
            "/api/settings/storage",
            "{\"output_dir\":\"/no/such/hearsay/dir\",\"compress_audio\":true,\"compress_after_days\":7}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The temp dir is absolute, existing, and writable.
    let dir = serde_json::to_string(&tmp.path().to_string_lossy()).unwrap();
    let (status, body) = send(
        &app,
        put(
            "/api/settings/storage",
            &format!("{{\"output_dir\":{dir},\"compress_audio\":true,\"compress_after_days\":7}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["output_dir"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn a_legacy_refine_model_field_is_ignored() {
    // A `models` row (or a client) from before the refine model was removed still carries
    // `refine_model`; it must neither fail to parse nor reappear in the response.
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(
        &app,
        put(
            "/api/settings/models",
            "{\"refine_model\":\"/old/ggml.bin\",\"notes_enabled\":false,\"notes_model\":\"\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("refine_model").is_none());
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert!(body["models"].get("refine_model").is_none());
}

#[tokio::test]
async fn settings_models_section_carries_notes_fields() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(status, StatusCode::OK);
    // The notes controls live on the models section (config defaults; no override, no file on disk).
    assert_eq!(body["models"]["notes_enabled"], false);
    assert_eq!(body["models"]["notes_model"], "no-notes-model");
    assert_eq!(body["models_info"]["default_notes_model"], "no-notes-model");
    assert_eq!(body["models_info"]["notes_model_exists"], false);
    // With no stored override, the effective prompt is the config default (also the reset target).
    assert_eq!(body["models"]["notes_prompt"], "Summarize:\n{transcript}");
    assert_eq!(
        body["models_info"]["default_notes_prompt"],
        "Summarize:\n{transcript}"
    );
}

#[tokio::test]
async fn validates_notes_model_on_models_update() {
    let (app, _pool, tmp) = setup().await;

    // A non-GGUF notes model is rejected on the magic check (422, not a DB 500).
    let not_gguf = tmp.path().join("not-a-model.gguf");
    std::fs::write(&not_gguf, b"this is not gguf").unwrap();
    let notes_arg = serde_json::to_string(&not_gguf.to_string_lossy()).unwrap();
    let (status, _) = send(
        &app,
        put(
            "/api/settings/models",
            &format!("{{\"notes_enabled\":true,\"notes_model\":{notes_arg}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A file with the GGUF magic ("GGUF") is accepted and the toggle round-trips.
    let gguf = tmp.path().join("qwen3.gguf");
    std::fs::write(&gguf, [0x47, 0x47, 0x55, 0x46, 0, 0, 0, 0]).unwrap();
    let gguf_arg = serde_json::to_string(&gguf.to_string_lossy()).unwrap();
    let (status, body) = send(
        &app,
        put(
            "/api/settings/models",
            &format!("{{\"notes_enabled\":true,\"notes_model\":{gguf_arg}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["notes_enabled"], true);
    assert!(body["notes_model"].as_str().is_some_and(|s| !s.is_empty()));

    // The override wins on the next read and the notes model resolves.
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["models"]["notes_enabled"], true);
    assert_eq!(body["models_info"]["notes_model_exists"], true);
}

#[tokio::test]
async fn notes_prompt_round_trips_and_caps_length() {
    let (app, _pool, _tmp) = setup().await;

    // A custom prompt is stored and wins on the next read; notes_model stays empty.
    let (status, body) = send(
        &app,
        put(
            "/api/settings/models",
            "{\"notes_enabled\":false,\"notes_model\":\"\",\"notes_prompt\":\"Recap:\\n{transcript}\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["notes_prompt"], "Recap:\n{transcript}");
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["models"]["notes_prompt"], "Recap:\n{transcript}");

    // An over-long prompt is a 422 at the boundary, never a runaway prompt at generate time.
    let huge = "x".repeat(8_001);
    let (status, _) = send(
        &app,
        put(
            "/api/settings/models",
            &format!(
                "{{\"notes_enabled\":false,\"notes_model\":\"\",\"notes_prompt\":\"{huge}\"}}"
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn generate_notes_is_404_then_unavailable() {
    let (app, pool, _tmp) = setup().await;

    // Unknown meeting is a 404 even without an engine wired.
    let (status, _) = send(
        &app,
        post(&format!("/api/meetings/{}/rediarize", Uuid::new_v4()), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A real meeting against `DisabledEngine` reports the notes step unavailable.
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(
        &app,
        post(&format!("/api/meetings/{}/rediarize", meeting.id), ""),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn read_notes_is_404_when_absent() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(&app, get(&format!("/api/meetings/{}/notes", meeting.id))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn models_catalog_lists_curated_models_and_download_starts_idle() {
    let (app, _pool, _tmp) = setup().await;

    let (status, body) = send(&app, get("/api/models/catalog")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert!(items.len() >= 2, "catalog should list several models");
    // Exactly one recommended default; none installed against a scratch models dir.
    assert_eq!(items.iter().filter(|m| m["recommended"] == true).count(), 1);
    assert!(items.iter().all(|m| m["installed"] == false));
    assert!(body["models_dir"].as_str().is_some());

    // No download has run yet.
    let (status, body) = send(&app, get("/api/models/download")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "idle");
    assert_eq!(body["downloaded_bytes"], 0);
}

#[tokio::test]
async fn settings_tolerates_a_partial_models_section_from_a_download() {
    // A completed download merges in only `notes_model`, leaving the stored
    // `models` section partial. `GET /settings` must resolve each field against its default rather
    // than failing to deserialize the section (which would 500 an otherwise-default install).
    let (app, pool, _tmp) = setup().await;
    queries::set_notes_model(&pool, "/models/qwen3.gguf")
        .await
        .unwrap();

    let (status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["models"]["notes_model"], "/models/qwen3.gguf");
    // The fields the download did not write fall back to their config defaults, not an error.
    assert_eq!(body["models"]["notes_enabled"], false);
}

#[tokio::test]
async fn download_unknown_model_is_404() {
    let (app, _pool, _tmp) = setup().await;
    let req = post("/api/models/download", r#"{"id":"no-such-model"}"#);
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn setup_reports_the_work_left_when_models_are_missing() {
    // `setup()` has no FluidAudio cache, so the
    // installer-ships-no-models state is exactly what these settings describe.
    let (app, _pool, _tmp) = setup().await;

    let (status, body) = send(&app, get("/api/setup")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["required"], true);
    assert_eq!(body["status"], "idle");
    let steps = body["steps"].as_array().unwrap();
    // The FluidAudio speech models (live and refine) are the one step a run fetches here.
    assert_eq!(steps[0]["id"], "live");
    assert_eq!(steps[0]["status"], "pending");
    assert!(steps[0]["total_bytes"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn setup_with_an_unknown_notes_model_is_404() {
    let (app, _pool, _tmp) = setup().await;
    let req = post("/api/setup", r#"{"notes_model_id":"no-such-model"}"#);
    let (status, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn search_finds_segments_across_meetings() {
    let (app, pool, _tmp) = setup().await;
    let a = queries::create_meeting(&pool, "Planning", "a", "", chrono::Utc::now())
        .await
        .unwrap();
    let b = queries::create_meeting(&pool, "Retro", "b", "", chrono::Utc::now())
        .await
        .unwrap();
    queries::insert_segment(
        &pool,
        a.id,
        Stream::Them,
        "Speaker 1",
        "we should ship the widget",
        1.0,
        2.0,
        None,
    )
    .await
    .unwrap();
    queries::insert_segment(
        &pool,
        b.id,
        Stream::Me,
        "Me",
        "the widget needs tests",
        3.0,
        4.0,
        None,
    )
    .await
    .unwrap();
    queries::insert_segment(
        &pool,
        b.id,
        Stream::Them,
        "Speaker 1",
        "unrelated chatter",
        5.0,
        6.0,
        None,
    )
    .await
    .unwrap();

    // A term present in two meetings returns a hit from each, with its meeting context + a snippet.
    let (status, body) = send(&app, get("/api/search?q=widget")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    let meeting_ids: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["meeting_id"].as_str().unwrap().to_string())
        .collect();
    assert!(meeting_ids.contains(&a.id.to_string()));
    assert!(meeting_ids.contains(&b.id.to_string()));
    // The snippet wraps the match in the U+E000/U+E001 sentinels for client-side highlighting.
    assert!(body["items"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains('\u{E000}'));

    // A non-matching term is an empty page, not an error.
    let (status, body) = send(&app, get("/api/search?q=nonexistentxyz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);

    // An empty query is a valid empty result (not a 422).
    let (status, body) = send(&app, get("/api/search?q=")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn editing_a_segment_updates_the_search_index() {
    let (app, pool, _tmp) = setup().await;
    let m = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let seg = queries::insert_segment(
        &pool,
        m.id,
        Stream::Me,
        "Me",
        "aardvark original",
        1.0,
        2.0,
        None,
    )
    .await
    .unwrap();

    let (status, body) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}", m.id, seg.id),
            r#"{"text":"pangolin replacement"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["text"], "pangolin replacement");
    assert_eq!(body["edited"], true);

    // The FTS `segments_au` trigger re-indexed the edit: the new text matches, the old does not.
    let (_s, found) = send(&app, get("/api/search?q=pangolin")).await;
    assert_eq!(found["total"], 1);
    let (_s, gone) = send(&app, get("/api/search?q=aardvark")).await;
    assert_eq!(gone["total"], 0);
}

#[tokio::test]
async fn edit_segment_validates_and_404s() {
    let (app, pool, _tmp) = setup().await;
    let m = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let seg = queries::insert_segment(&pool, m.id, Stream::Me, "Me", "hello", 1.0, 2.0, None)
        .await
        .unwrap();

    // Empty/whitespace text is rejected at the boundary.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}", m.id, seg.id),
            r#"{"text":"   "}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown segment id is a 404.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}", m.id, Uuid::new_v4()),
            r#"{"text":"x"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reassign_segment_speaker_moves_line_and_validates() {
    let (app, pool, _tmp) = setup().await;
    let m = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let c1 = queries::create_cluster(&pool, m.id, 1, false, None)
        .await
        .unwrap();
    let c2 = queries::create_cluster(&pool, m.id, 2, false, None)
        .await
        .unwrap();
    let them = queries::insert_segment(
        &pool,
        m.id,
        Stream::Them,
        "Speaker 1",
        "hi",
        0.0,
        1.0,
        Some(c1.id),
    )
    .await
    .unwrap();
    let me = queries::insert_segment(&pool, m.id, Stream::Me, "Me", "mine", 1.0, 2.0, None)
        .await
        .unwrap();
    let speaker_uri = format!("/api/meetings/{}/segments/{}/speaker", m.id, them.id);

    // Move the Them line to the other existing (unbound) cluster: label falls back to its ordinal.
    let (status, body) = send(
        &app,
        patch(&speaker_uri, &format!(r#"{{"cluster_id":"{}"}}"#, c2.id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["speaker_label"], "Speaker 2");
    assert_eq!(body["cluster_id"], c2.id.to_string());
    assert_eq!(body["edited"], true);

    // Assign it to a brand-new named speaker (trimmed): a new cluster shows in the speakers list.
    let (status, body) = send(&app, patch(&speaker_uri, r#"{"display_name":"  Dana  "}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["speaker_label"], "Dana");
    let (status, speakers) = send(&app, get(&format!("/api/meetings/{}/speakers", m.id))).await;
    assert_eq!(status, StatusCode::OK);
    let labels: Vec<&str> = speakers["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"Dana"));

    // 422: both fields set.
    let (status, _) = send(
        &app,
        patch(
            &speaker_uri,
            &format!(r#"{{"cluster_id":"{}","display_name":"X"}}"#, c1.id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 422: neither field set.
    let (status, _) = send(&app, patch(&speaker_uri, r#"{}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 422: a blank (whitespace-only) display_name.
    let (status, _) = send(&app, patch(&speaker_uri, r#"{"display_name":"   "}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 422: a display_name over the 255-char bound.
    let long = "x".repeat(256);
    let (status, _) = send(
        &app,
        patch(&speaker_uri, &format!(r#"{{"display_name":"{long}"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 422: reassigning a Me line.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}/speaker", m.id, me.id),
            &format!(r#"{{"cluster_id":"{}"}}"#, c1.id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 422: target cluster is not in this meeting.
    let (status, _) = send(
        &app,
        patch(
            &speaker_uri,
            &format!(r#"{{"cluster_id":"{}"}}"#, Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 404: unknown segment id.
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}/speaker", m.id, Uuid::new_v4()),
            &format!(r#"{{"cluster_id":"{}"}}"#, c1.id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn edit_notes_marks_edited_and_tracks_stale() {
    let (app, pool, _tmp) = setup().await;
    let m = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    queries::insert_segment(&pool, m.id, Stream::Me, "Me", "hello there", 1.0, 2.0, None)
        .await
        .unwrap();
    // Seed generated notes (as the LLM step would).
    queries::upsert_meeting_notes(
        &pool,
        m.id,
        &queries::NotesResult {
            content: "auto summary\n\n- do a thing".into(),
        },
        "test-model",
    )
    .await
    .unwrap();

    // Freshly generated notes: not edited, not stale.
    let (status, body) = send(&app, get(&format!("/api/meetings/{}/notes", m.id))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["edited"], false);
    assert_eq!(body["stale"], false);

    // Editing a segment after the notes were generated makes them stale.
    let seg = queries::list_segments(&pool, m.id).await.unwrap().remove(0);
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/segments/{}", m.id, seg.id),
            r#"{"text":"hello world"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_s, body) = send(&app, get(&format!("/api/meetings/{}/notes", m.id))).await;
    assert_eq!(body["stale"], true);

    // Editing the notes sets `edited` and clears `stale` (they are now the newest write).
    let (status, body) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/notes", m.id),
            r#"{"content":"hand edited\n\n- fixed item"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["content"], "hand edited\n\n- fixed item");
    assert_eq!(body["edited"], true);
    assert_eq!(body["stale"], false);

    // The edit persists on a subsequent read (until a regenerate would clear `edited`).
    let (_s, body) = send(&app, get(&format!("/api/meetings/{}/notes", m.id))).await;
    assert_eq!(body["edited"], true);
    assert_eq!(body["stale"], false);
}

#[tokio::test]
async fn edit_notes_is_404_without_generated_notes() {
    let (app, pool, _tmp) = setup().await;
    let m = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(
        &app,
        patch(
            &format!("/api/meetings/{}/notes", m.id),
            r#"{"content":"x"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn notices_reports_a_missing_file_rather_than_opening_nothing() {
    // The test config points at a notices path that does not exist, so this exercises the guard
    // without handing anything to the OS file manager. The success path opens the bundled file and
    // is left to manual verification, like the reveal routes.
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, post("/api/settings/notices", "")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["detail"]
        .as_str()
        .is_some_and(|d| d.contains("third-party notices not found")));
}

#[tokio::test]
async fn reveal_unknown_meeting_is_404() {
    // Only the not-found path is exercised (it returns before touching the OS file manager, so the
    // test has no side effect); the success path opens Finder and is left to manual verification.
    let (app, _pool, _tmp) = setup().await;
    let (status, _) = send(
        &app,
        post(&format!("/api/meetings/{}/reveal", Uuid::new_v4()), ""),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
