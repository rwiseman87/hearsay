//! Wrapper extractors that funnel axum's default extractor rejections — a bad UUID in a `Path`, an
//! unparseable `Query` string, a malformed or wrong-typed JSON body — into
//! [`ApiError::Unprocessable`]. Without them a failed extraction returns axum's plain-text `400`,
//! bypassing the `{ "detail": ... }` envelope every other error renders; with them the whole API
//! answers invalid input with a uniform `422` envelope.
//!
//! Handlers use these newtypes in place of `axum::extract::{Path, Query}` and `axum::Json`. [`Json`]
//! also re-implements [`IntoResponse`] by delegating to `axum::Json`, so it is a drop-in for both
//! request extraction and response serialization exactly like the type it wraps.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;

/// [`axum::extract::Path`] whose rejection renders as a `422` `{ "detail": ... }` envelope.
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    axum::extract::Path<T>: FromRequestParts<S, Rejection = PathRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::Unprocessable(rejection.body_text())),
        }
    }
}

/// [`axum::extract::Query`] whose rejection renders as a `422` `{ "detail": ... }` envelope.
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    axum::extract::Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::Unprocessable(rejection.body_text())),
        }
    }
}

/// [`axum::Json`] whose *extraction* rejection renders as a `422` `{ "detail": ... }` envelope. It
/// still serializes as a JSON response body (see the [`IntoResponse`] impl), so handlers use it in
/// both directions just like `axum::Json`.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::Unprocessable(rejection.body_text())),
        }
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}
