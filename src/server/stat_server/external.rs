use crate::server::database::{Database, DbError};
use http_body_util::{BodyExt, Limited};
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use super::StatContext;
use super::auth::reject_without_api_key;
use super::response::{error_response, json_response};

const MAX_BODY_SIZE: usize = 4096;

#[derive(Deserialize)]
struct VerifyRequest {
	#[serde(alias = "id")]
	username: String,
	#[serde(alias = "pw")]
	password: String,
}

#[derive(Serialize)]
struct VerifyResponse {
	user_id: i64,
	username: String,
	online_name: String,
	avatar_url: String,
	admin: bool,
	banned: bool,
}

pub(super) async fn handle_verify_req(req: Request<hyper::body::Incoming>, ctx: &StatContext) -> Response<String> {
	if let Some(response) = reject_without_api_key(&req, ctx) {
		return response;
	}

	if req.method() != Method::POST {
		return error_response(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
	}

	let Ok(body) = Limited::new(req.into_body(), MAX_BODY_SIZE).collect().await else {
		return error_response(StatusCode::BAD_REQUEST, "invalid_request");
	};
	let request = match serde_json::from_slice::<VerifyRequest>(&body.to_bytes()) {
		Ok(request) if !request.username.is_empty() && !request.password.is_empty() => request,
		_ => return error_response(StatusCode::BAD_REQUEST, "invalid_request"),
	};

	let db_pool = ctx.db_pool.clone();
	let verification = tokio::task::spawn_blocking(move || {
		let connection = db_pool.get().map_err(|_| DbError::Internal)?;
		Database::new(connection).check_user(&request.username, &request.password, "", false)
	})
	.await;

	match verification {
		Ok(Ok(user)) => {
			let response = VerifyResponse {
				user_id: user.user_id,
				username: user.username,
				online_name: user.online_name,
				avatar_url: user.avatar_url,
				admin: user.admin,
				banned: user.banned,
			};
			json_response(StatusCode::OK, serde_json::to_string(&response).unwrap())
		}
		Ok(Err(DbError::Empty | DbError::WrongPass)) => error_response(StatusCode::UNAUTHORIZED, "invalid_credentials"),
		Ok(Err(_)) | Err(_) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
	}
}
