use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::server::database::DbError;

// 응답 본문은 기존 stat 서버와 같은 {"error": "<code>"}
pub enum ApiError {
	InvalidRequest,
	InvalidCredentials,
	Forbidden,
	UserNotFound,
	MethodNotAllowed,
	Internal,
}

impl ApiError {
	fn status_and_code(&self) -> (StatusCode, &'static str) {
		match self {
			ApiError::InvalidRequest => (StatusCode::BAD_REQUEST, "invalid_request"),
			ApiError::InvalidCredentials => (StatusCode::UNAUTHORIZED, "invalid_credentials"),
			ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
			ApiError::UserNotFound => (StatusCode::NOT_FOUND, "user_not_found"),
			ApiError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
			ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
		}
	}
}

impl IntoResponse for ApiError {
	fn into_response(self) -> Response {
		let (status, code) = self.status_and_code();
		(status, Json(json!({ "error": code }))).into_response()
	}
}

// 로그인 실패는 유저가 없는 경우와 비밀번호가 틀린 경우를 구분하지 않는다
impl From<DbError> for ApiError {
	fn from(e: DbError) -> ApiError {
		match e {
			DbError::Empty | DbError::WrongPass => ApiError::InvalidCredentials,
			_ => ApiError::Internal,
		}
	}
}
