use hyper::{Request, Response, StatusCode};
use openssl::memcmp;

use super::StatContext;
use super::response::error_response;

// Key-protected routes answer 404 while ExternalUserApiKey is empty, and 403 on a missing or wrong X-API-Key.
pub(super) fn reject_without_api_key<B>(req: &Request<B>, ctx: &StatContext) -> Option<Response<String>> {
	let Some(api_key) = ctx.external_user_api_key.as_deref() else {
		return Some(Response::builder().status(StatusCode::NOT_FOUND).body("".to_owned()).unwrap());
	};

	let is_authorized = req
		.headers()
		.get("X-API-Key")
		.and_then(|value| value.to_str().ok())
		.is_some_and(|value| value.len() == api_key.len() && memcmp::eq(value.as_bytes(), api_key.as_bytes()));
	if !is_authorized {
		return Some(error_response(StatusCode::FORBIDDEN, "forbidden"));
	}

	None
}
