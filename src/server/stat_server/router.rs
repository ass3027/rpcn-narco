use std::convert::Infallible;
use std::sync::Arc;

use crate::server::client::ComId;
use hyper::{Method, Request, Response};

use super::StatContext;
use super::auth::reject_without_api_key;
use super::response::empty_response;
use super::{admin, external, rooms, score, usage};

pub(super) async fn route(req: Request<hyper::body::Incoming>, ctx: Arc<StatContext>) -> Result<Response<String>, Infallible> {
	let Some(sub_path) = req.uri().path().strip_prefix(ctx.path.as_str()) else {
		return Ok(empty_response());
	};

	// POST routes are matched before the GET filter.
	if sub_path == "/external/users/verify" {
		return Ok(external::handle_verify_req(req, &ctx).await);
	}

	if sub_path == "/admin/users/info" {
		return Ok(admin::handle_user_info_req(req, &ctx).await);
	}

	if sub_path == "/admin/users/ban" {
		return Ok(admin::handle_ban_req(req, &ctx).await);
	}

	if req.method() != Method::GET {
		return Ok(empty_response());
	}

	// Usage lists every connected player's IP, so it shares the external API key.
	if sub_path == "/usage" {
		return Ok(reject_without_api_key(&req, &ctx).unwrap_or_else(|| usage::handle_usage_req(&ctx)));
	}

	if let Some(com_id) = sub_path.strip_prefix("/rooms/").and_then(parse_com_id) {
		return Ok(rooms::handle_rooms_req(&ctx, &com_id));
	}

	if let Some(rest) = sub_path.strip_prefix("/score/") {
		let (com_id_str, table_id_str) = match rest.split_once('/') {
			Some((com_id_str, table_id_str)) => (com_id_str, Some(table_id_str)),
			None => (rest, None),
		};

		if let Some(com_id) = parse_com_id(com_id_str) {
			match table_id_str.map(str::parse::<u32>) {
				None => return Ok(score::handle_com_id_score_req(&ctx, &com_id)),
				Some(Ok(table_id)) => return Ok(score::handle_table_score_req(&ctx, &com_id, table_id)),
				Some(Err(_)) => {}
			}
		}
	}

	Ok(empty_response())
}

fn parse_com_id(s: &str) -> Option<ComId> {
	s.as_bytes().try_into().ok()
}
