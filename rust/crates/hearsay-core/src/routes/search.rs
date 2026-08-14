//! Full-text transcript search across meetings (SQLite FTS5, backed by the `segments_fts` index).
//! One endpoint returns a ranked, paginated list of matching segments, each carrying the meeting it
//! belongs to plus a highlighted snippet. Thin as usual: sanitize the query, run the FTS lookup,
//! return the page.

use axum::extract::State;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use hearsay_db::queries;

use crate::error::{ApiError, ApiResult};
use crate::extract::{Json, Query};
use crate::routes::Pagination;
use crate::schema::{Page, SearchHit};
use crate::state::AppState;

/// Max search-query length (chars); longer is rejected at the boundary rather than handed to FTS.
const MAX_QUERY_LEN: usize = 256;

/// Routes served under the `/api` prefix (token-gated by the caller).
pub fn router() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

/// `?q=` plus the shared pagination params. Flat (not a `#[serde(flatten)]` of [`Pagination`])
/// because axum's `Query` uses `serde_urlencoded`, which does not support flattening.
#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

/// Turn raw user input into a safe FTS5 `MATCH` expression: keep only alphanumeric characters per
/// whitespace-separated token (dropping every FTS operator, so a stray quote / `-` / `*` can neither
/// be a syntax error nor silently change the query), append `*` for prefix / as-you-type matching,
/// and join with spaces (implicit AND — every token must be present). `None` when nothing usable
/// remains, which the caller renders as an empty result rather than an error.
fn build_match(raw: &str) -> Option<String> {
    let mut tokens: Vec<String> = Vec::new();
    for word in raw.split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if !cleaned.is_empty() {
            tokens.push(format!("{cleaned}*"));
        }
    }
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

#[utoipa::path(
    get, path = "/api/search", tag = "search",
    params(
        ("q" = Option<String>, Query),
        ("page" = Option<u32>, Query), ("page_size" = Option<u32>, Query),
    ),
    responses((status = 200, body = Page<SearchHit>), (status = 422)),
)]
pub(crate) async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> ApiResult<Json<Page<SearchHit>>> {
    let window = Pagination {
        page: params.page,
        page_size: params.page_size,
    }
    .resolve(50, 200);
    let raw = params.q.unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.chars().count() > MAX_QUERY_LEN {
        return Err(ApiError::Unprocessable(format!(
            "query exceeds {MAX_QUERY_LEN} characters"
        )));
    }
    // An empty (or all-punctuation) query is a valid no-op, not a 422: return an empty page so the
    // UI can clear results without special-casing.
    let Some(match_query) = build_match(trimmed) else {
        return Ok(Json(window.empty_page()));
    };
    let total = queries::count_search(&state.pool, &match_query).await?;
    let rows =
        queries::search_segments(&state.pool, &match_query, window.limit, window.offset).await?;
    Ok(Json(window.page_of(total, rows)))
}

#[cfg(test)]
mod tests {
    use super::build_match;

    #[test]
    fn build_match_tokenizes_and_prefixes() {
        assert_eq!(build_match("hello world").as_deref(), Some("hello* world*"));
    }

    #[test]
    fn build_match_strips_fts_operators() {
        // A leading '-' / embedded quote / '*' would be FTS syntax; sanitized to bare prefix tokens.
        assert_eq!(
            build_match("-foo \"bar\" baz*").as_deref(),
            Some("foo* bar* baz*")
        );
    }

    #[test]
    fn build_match_empty_or_punctuation_is_none() {
        assert_eq!(build_match(""), None);
        assert_eq!(build_match("   "), None);
        assert_eq!(build_match("-.,\"'"), None);
    }
}
