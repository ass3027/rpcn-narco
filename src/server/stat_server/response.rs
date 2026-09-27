use hyper::{Response, StatusCode};

pub(super) fn json_response(status: StatusCode, body: String) -> Response<String> {
	Response::builder().status(status).header("Content-Type", "application/json").body(body).unwrap()
}

pub(super) fn error_response(status: StatusCode, error: &str) -> Response<String> {
	json_response(status, format!("{{\"error\":\"{}\"}}", error))
}

pub(super) fn empty_response() -> Response<String> {
	Response::new("".to_owned())
}

pub(super) fn sanitize_for_json(s: &str) -> String {
	let mut res = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'"' => res.push_str("\\\""),
			'\\' => res.push_str("\\\\"),
			'\x08' | '\x0C' | '\n' | '\r' | '\t' => {}
			_ => res.push(c),
		}
	}
	res
}
