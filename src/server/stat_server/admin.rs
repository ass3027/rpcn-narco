use crate::server::database::{Database, DbError};
use http_body_util::{BodyExt, Limited};
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::StatContext;
use super::auth::reject_without_api_key;
use super::response::{error_response, json_response};

const MAX_BODY_SIZE: usize = 4096;

#[derive(Deserialize)]
struct AdminRequest {
	admin_username: String,
	admin_password: String,
	username: String,
}

#[derive(Serialize)]
struct UserInfoResponse {
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
struct BanResponse {
	user_id: i64,
	username: String,
	banned: bool,
	kicked: bool,
}

enum AdminAction {
	Info,
	Ban,
}

pub(super) async fn handle_user_info_req(req: Request<hyper::body::Incoming>, ctx: &StatContext) -> Response<String> {
	handle_admin_req(req, ctx, AdminAction::Info).await
}

pub(super) async fn handle_ban_req(req: Request<hyper::body::Incoming>, ctx: &StatContext) -> Response<String> {
	handle_admin_req(req, ctx, AdminAction::Ban).await
}

// X-API-Key와 관리자 계정 id/pw를 모두 확인한 뒤 대상 유저를 조회하거나 ban
async fn handle_admin_req(req: Request<hyper::body::Incoming>, ctx: &StatContext, action: AdminAction) -> Response<String> {
	if let Some(response) = reject_without_api_key(&req, ctx) {
		return response;
	}

	if req.method() != Method::POST {
		return error_response(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
	}

	let Ok(body) = Limited::new(req.into_body(), MAX_BODY_SIZE).collect().await else {
		return error_response(StatusCode::BAD_REQUEST, "invalid_request");
	};
	let request = match serde_json::from_slice::<AdminRequest>(&body.to_bytes()) {
		Ok(request) if !request.admin_username.is_empty() && !request.admin_password.is_empty() && !request.username.is_empty() => request,
		_ => return error_response(StatusCode::BAD_REQUEST, "invalid_request"),
	};

	let db_pool = ctx.db_pool.clone();
	let result = tokio::task::spawn_blocking(move || {
		let db = Database::new(db_pool.get().map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?);

		match db.check_user(&request.admin_username, &request.admin_password, "", false) {
			Ok(admin) if admin.admin && !admin.banned => {}
			Ok(_) => {
				warn!("Non-admin user {} attempted to use the admin API", request.admin_username);
				return Err(error_response(StatusCode::FORBIDDEN, "forbidden"));
			}
			Err(DbError::Empty | DbError::WrongPass) => return Err(error_response(StatusCode::UNAUTHORIZED, "invalid_credentials")),
			Err(_) => return Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")),
		}

		let user = match db.get_user_admin_info(&request.username) {
			Ok(user) => user,
			Err(DbError::Empty) => return Err(error_response(StatusCode::NOT_FOUND, "user_not_found")),
			Err(_) => return Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")),
		};

		if let AdminAction::Ban = action {
			db.ban_user(user.user_id).map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
			warn!("Admin {} banned user {} via stat API", request.admin_username, user.username);
		}

		Ok((action, user))
	})
	.await;

	let (action, user) = match result {
		Ok(Ok(res)) => res,
		Ok(Err(response)) => return response,
		Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
	};

	let body = match action {
		AdminAction::Info => {
			let online = ctx.client_infos.read().contains_key(&user.user_id);
			serde_json::to_string(&UserInfoResponse {
				user_id: user.user_id,
				username: user.username,
				online_name: user.online_name,
				avatar_url: user.avatar_url,
				admin: user.admin,
				banned: user.banned,
				online,
				creation: user.creation,
				last_login: user.last_login,
			})
		}
		AdminAction::Ban => {
			let kicked = match ctx.client_infos.read().get(&user.user_id) {
				Some(client_info) => {
					client_info.kick();
					true
				}
				None => false,
			};
			serde_json::to_string(&BanResponse {
				user_id: user.user_id,
				username: user.username,
				banned: true,
				kicked,
			})
		}
	};

	json_response(StatusCode::OK, body.unwrap())
}
