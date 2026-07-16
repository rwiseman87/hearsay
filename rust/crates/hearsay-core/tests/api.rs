//! Integration tests over the assembled router, driven with `tower::ServiceExt::oneshot` against a
//! single-connection in-memory SQLite DB. The live-capture routes run against `DisabledEngine`
//! (503 / clean close), so this covers the whole self-contained surface without a real pipeline.

use std::path::PathBuf;
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
        refine_model: PathBuf::from("no-model"),
        refine_timeout: std::time::Duration::from_secs(1800),
        auto_refine: false,
        record: true,
        recognition_threshold: 0.6,
        handshake_path: None,
        fluid_models_dir: None,
        home_dir: None,
    }
}

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
    let settings = test_settings(tmp.path().to_path_buf(), tmp.path().join("no-web"));
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
    let state = AppState::new(pool, settings, TOKEN.to_string(), Arc::new(DisabledEngine));
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

    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
        .await
        .unwrap();
    let (status, _) = send(&app, post(meeting.id)).await;
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
async fn deletes_a_meeting_then_404s() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "M", "f", "", chrono::Utc::now())
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
async fn renames_a_meeting_and_validates_the_title() {
    let (app, pool, _tmp) = setup().await;
    let meeting = queries::create_meeting(&pool, "Old name", "f", "", chrono::Utc::now())
        .await
        .unwrap();

    let patch = |id: Uuid, body: &str| {
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/meetings/{id}"))
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let (status, body) = send(&app, patch(meeting.id, "{\"title\":\"  New name  \"}")).await;
    assert_eq!(status, StatusCode::OK);
    // Trimmed and returned; the list reflects it too.
    assert_eq!(body["title"], "New name");
    let (_, listed) = send(&app, get("/api/meetings")).await;
    assert_eq!(listed["items"][0]["title"], "New name");

    // A blank title is a 400 (never let the DB store an empty name).
    let (status, _) = send(&app, patch(meeting.id, "{\"title\":\"   \"}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An unknown id is a 404.
    let (status, _) = send(&app, patch(Uuid::new_v4(), "{\"title\":\"x\"}")).await;
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
    assert_eq!(body["about"]["environment"], "test");
    assert_eq!(body["about"]["protocol_version"], 1);
}

#[tokio::test]
async fn permissions_probe_degrades_when_helper_missing() {
    // `setup()` points `helper_path` at a nonexistent file, so the probe reports unavailable.
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/api/settings/permissions")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["helper_available"], false);
    assert_eq!(body["helper_version"], Value::Null);
    assert_eq!(body["microphone"], "unknown");
    assert_eq!(body["calendar"], "unknown");
}

#[tokio::test]
async fn updates_recording_and_persists_the_override() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, put("/api/settings/recording", "{\"record\":false}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["record"], false);

    // The stored override now wins over the config default on the next read.
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["recording"]["record"], false);
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

#[tokio::test]
async fn validates_output_dir_on_storage_update() {
    let (app, _pool, tmp) = setup().await;

    // A nonexistent absolute path is rejected at the boundary (422, not a DB 500).
    let (status, _) = send(
        &app,
        put(
            "/api/settings/storage",
            "{\"output_dir\":\"/no/such/hearsay/dir\"}",
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
            &format!("{{\"output_dir\":{dir}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["output_dir"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn models_section_reports_default_and_missing_file() {
    let (app, _pool, _tmp) = setup().await;
    let (status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(status, StatusCode::OK);
    // `setup()` sets refine_model to "no-model" (no override stored, no file on disk).
    assert_eq!(body["models"]["refine_model"], "no-model");
    assert_eq!(body["models_info"]["default_refine_model"], "no-model");
    assert_eq!(body["models_info"]["refine_model_exists"], false);
}

#[tokio::test]
async fn validates_refine_model_and_round_trips_override() {
    let (app, _pool, tmp) = setup().await;

    // A relative path is rejected (must be absolute).
    let (status, _) = send(
        &app,
        put("/api/settings/models", "{\"refine_model\":\"model.bin\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A nonexistent absolute path is rejected at the boundary (422, not a DB 500).
    let (status, _) = send(
        &app,
        put(
            "/api/settings/models",
            "{\"refine_model\":\"/no/such/model.bin\"}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A real file that is not a GGML whisper model is rejected on the magic check.
    let not_ggml = tmp.path().join("not-a-model.bin");
    std::fs::write(&not_ggml, b"this is not ggml").unwrap();
    let arg = serde_json::to_string(&not_ggml.to_string_lossy()).unwrap();
    let (status, _) = send(
        &app,
        put(
            "/api/settings/models",
            &format!("{{\"refine_model\":{arg}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A file with the GGML magic (0x67676d6c, little-endian) is accepted and canonicalized.
    let model = tmp.path().join("ggml-test.bin");
    std::fs::write(&model, [0x6c, 0x6d, 0x67, 0x67, 0, 0, 0, 0]).unwrap();
    let arg = serde_json::to_string(&model.to_string_lossy()).unwrap();
    let (status, body) = send(
        &app,
        put(
            "/api/settings/models",
            &format!("{{\"refine_model\":{arg}}}"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = body["refine_model"].as_str().unwrap().to_string();
    assert!(!stored.is_empty());

    // The override now wins on the next read and the file resolves.
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["models"]["refine_model"], stored);
    assert_eq!(body["models_info"]["refine_model_exists"], true);

    // DELETE clears the override, reverting to the config default (even though it is a bare name).
    let del = Request::builder()
        .method("DELETE")
        .uri("/api/settings/models")
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, del).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["refine_model"], "no-model");
    let (_status, body) = send(&app, get("/api/settings")).await;
    assert_eq!(body["models"]["refine_model"], "no-model");
}
