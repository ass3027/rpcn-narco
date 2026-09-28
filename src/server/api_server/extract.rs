use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use serde::de::DeserializeOwned;

use super::error::ApiError;

// axum::Json과 달리 Content-Type을 요구하지 않고, 크기 초과·형식 오류를 모두 400 invalid_request로 돌려준다
pub struct JsonBody<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for JsonBody<T> {
	type Rejection = ApiError;

	async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
		let body = Bytes::from_request(req, state).await.map_err(|_| ApiError::InvalidRequest)?;
		serde_json::from_slice(&body).map(JsonBody).map_err(|_| ApiError::InvalidRequest)
	}
}
