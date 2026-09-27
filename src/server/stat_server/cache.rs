use std::collections::HashMap;

use crate::Client;
use crate::server::client::ComId;
use hyper::Response;
use parking_lot::Mutex;

// Response rebuilt at most once every `cache_life` seconds.
struct CachedResponse {
	inner: Mutex<(u32, Response<String>)>,
}

impl CachedResponse {
	fn new() -> CachedResponse {
		CachedResponse {
			inner: Mutex::new((0, Response::new("".to_owned()))),
		}
	}

	fn get_or_refresh(&self, cache_life: u32, build: impl FnOnce() -> Response<String>) -> Response<String> {
		let now = Client::get_timestamp_seconds();
		let mut inner = self.inner.lock();
		let (timestamp, response) = &mut *inner;

		if now > *timestamp + cache_life {
			*response = build();
			*timestamp = now;
		}

		response.clone()
	}
}

// With `cache_life == 0` every lookup builds a fresh response and nothing is stored.
pub(super) struct JsonCache {
	usage: CachedResponse,
	com_id_scores: Mutex<HashMap<ComId, CachedResponse>>,
	table_scores: Mutex<HashMap<ComId, HashMap<u32, CachedResponse>>>,
}

impl JsonCache {
	pub(super) fn new() -> JsonCache {
		JsonCache {
			usage: CachedResponse::new(),
			com_id_scores: Mutex::new(HashMap::new()),
			table_scores: Mutex::new(HashMap::new()),
		}
	}

	pub(super) fn usage(&self, cache_life: u32, build: impl FnOnce() -> Response<String>) -> Response<String> {
		if cache_life == 0 {
			return build();
		}
		self.usage.get_or_refresh(cache_life, build)
	}

	pub(super) fn com_id_score(&self, cache_life: u32, com_id: &ComId, build: impl FnOnce() -> Response<String>) -> Response<String> {
		if cache_life == 0 {
			return build();
		}
		let mut com_id_scores = self.com_id_scores.lock();
		com_id_scores.entry(*com_id).or_insert_with(CachedResponse::new).get_or_refresh(cache_life, build)
	}

	pub(super) fn table_score(&self, cache_life: u32, com_id: &ComId, table_id: u32, build: impl FnOnce() -> Response<String>) -> Response<String> {
		if cache_life == 0 {
			return build();
		}
		let mut table_scores = self.table_scores.lock();
		table_scores
			.entry(*com_id)
			.or_default()
			.entry(table_id)
			.or_insert_with(CachedResponse::new)
			.get_or_refresh(cache_life, build)
	}
}
