use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use openssl::memcmp;
use tracing::warn;

use super::error::ApiError;
use super::state::ApiState;
use crate::server::database::Database;

// ApiServerApiKey가 비어 있으면 404, X-API-Key가 없거나 틀리면 403
pub async fn require_api_key(State(state): State<Arc<ApiState>>, req: Request, next: Next) -> Response {
	let Some(api_key) = state.api_key.as_deref() else {
		return StatusCode::NOT_FOUND.into_response();
	};

	if !has_api_key(req.headers(), api_key) {
		return ApiError::Forbidden.into_response();
	}

	next.run(req).await
}

fn has_api_key(headers: &HeaderMap, api_key: &str) -> bool {
	headers
		.get("X-API-Key")
		.and_then(|value| value.to_str().ok())
		.is_some_and(|value| value.len() == api_key.len() && memcmp::eq(value.as_bytes(), api_key.as_bytes()))
}

// ban된 관리자 계정은 관리자 권한도 잃는다
pub fn authenticate_admin(db: &Database, username: &str, password: &str) -> Result<(), ApiError> {
	let admin = db.check_user(username, password, "", false)?;
	if !admin.admin || admin.banned {
		warn!("Non-admin user {} attempted to use the admin API", username);
		return Err(ApiError::Forbidden);
	}
	Ok(())
}
