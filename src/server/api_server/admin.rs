use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::auth::authenticate_admin;
use super::error::ApiError;
use super::extract::JsonBody;
use super::state::ApiState;
use crate::server::database::{Database, DbError, UserAdminInfo};

#[derive(Deserialize)]
pub struct AdminRequest {
	admin_username: String,
	admin_password: String,
	username: String,
}

impl AdminRequest {
	fn is_complete(&self) -> bool {
		!self.admin_username.is_empty() && !self.admin_password.is_empty() && !self.username.is_empty()
	}
}

#[derive(Serialize)]
pub struct UserInfoResponse {
	user_id: i64,
	username: String,
	online_name: String,
	avatar_url: String,
	admin: bool,
	banned: bool,
	online: bool,
	creation: Option<u64>,
	last_login: Option<u64>,
}

#[derive(Serialize)]
pub struct BanResponse {
	user_id: i64,
	username: String,
	banned: bool,
	kicked: bool,
}

#[derive(Clone, Copy)]
enum AdminAction {
	Info,
	Ban,
}

pub async fn user_info(State(state): State<Arc<ApiState>>, JsonBody(request): JsonBody<AdminRequest>) -> Result<Json<UserInfoResponse>, ApiError> {
	let user = run_admin_action(&state, request, AdminAction::Info).await?;
	let online = state.client_infos.read().contains_key(&user.user_id);

	Ok(Json(UserInfoResponse {
		user_id: user.user_id,
		username: user.username,
		online_name: user.online_name,
		avatar_url: user.avatar_url,
		admin: user.admin,
		banned: user.banned,
		online,
		creation: user.creation,
		last_login: user.last_login,
	}))
}

// 접속 중이면 연결을 끊는다. ban 플래그 때문에 다시 로그인할 수 없다
pub async fn ban(State(state): State<Arc<ApiState>>, JsonBody(request): JsonBody<AdminRequest>) -> Result<Json<BanResponse>, ApiError> {
	let user = run_admin_action(&state, request, AdminAction::Ban).await?;
	let kicked = state.client_infos.read().get(&user.user_id).inspect(|client_info| client_info.kick()).is_some();

	Ok(Json(BanResponse {
		user_id: user.user_id,
		username: user.username,
		banned: true,
		kicked,
	}))
}

// 관리자 계정 id/pw를 확인한 뒤 대상 유저를 조회하고, Ban이면 DB에 ban을 기록한다
async fn run_admin_action(state: &ApiState, request: AdminRequest, action: AdminAction) -> Result<UserAdminInfo, ApiError> {
	if !request.is_complete() {
		return Err(ApiError::InvalidRequest);
	}

	let db_pool = state.db_pool.clone();
	tokio::task::spawn_blocking(move || {
		let db = Database::new(db_pool.get().map_err(|_| ApiError::Internal)?);
		authenticate_admin(&db, &request.admin_username, &request.admin_password)?;

		let user = db.get_user_admin_info(&request.username).map_err(|e| match e {
			DbError::Empty => ApiError::UserNotFound,
			_ => ApiError::Internal,
		})?;

		if let AdminAction::Ban = action {
			db.ban_user(user.user_id).map_err(|_| ApiError::Internal)?;
			warn!("Admin {} banned user {} via API server", request.admin_username, user.username);
		}

		Ok(user)
	})
	.await
	.map_err(|_| ApiError::Internal)?
}
