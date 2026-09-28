use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::extract::JsonBody;
use super::state::ApiState;
use crate::server::database::Database;

#[derive(Deserialize)]
pub struct VerifyRequest {
	#[serde(alias = "id")]
	username: String,
	#[serde(alias = "pw")]
	password: String,
}

impl VerifyRequest {
	fn is_complete(&self) -> bool {
		!self.username.is_empty() && !self.password.is_empty()
	}
}

#[derive(Serialize)]
pub struct VerifyResponse {
	user_id: i64,
	username: String,
	online_name: String,
	avatar_url: String,
	admin: bool,
	banned: bool,
}

// 로그인 세션이나 토큰은 만들지 않고 id/pw가 맞는지만 확인한다
pub async fn verify(State(state): State<Arc<ApiState>>, JsonBody(request): JsonBody<VerifyRequest>) -> Result<Json<VerifyResponse>, ApiError> {
	if !request.is_complete() {
		return Err(ApiError::InvalidRequest);
	}

	let db_pool = state.db_pool.clone();
	// check_user는 argon2 해시라 느리므로 blocking 스레드에서 돌린다
	let user = tokio::task::spawn_blocking(move || {
		let db = Database::new(db_pool.get().map_err(|_| ApiError::Internal)?);
		db.check_user(&request.username, &request.password, "", false).map_err(ApiError::from)
	})
	.await
	.map_err(|_| ApiError::Internal)??;

	Ok(Json(VerifyResponse {
		user_id: user.user_id,
		username: user.username,
		online_name: user.online_name,
		avatar_url: user.avatar_url,
		admin: user.admin,
		banned: user.banned,
	}))
}
