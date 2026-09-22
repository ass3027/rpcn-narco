use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Write;
use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::Client;
use crate::server::GameTracker;
use crate::server::Server;
use crate::server::client::{COMMUNICATION_ID_SIZE, ComId, TerminateWatch, com_id_to_string};
use crate::server::database::db_score::DbBoardInfo;
use crate::server::database::{Database, DbError, DbMatchRecord};
use crate::server::game_specific_tus;
use crate::server::room_manager::RoomManager;
use crate::server::score_cache::{GetScoreResultCache, ScoresCache};
use http_body_util::{BodyExt, Limited};
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use openssl::memcmp;
use parking_lot::Mutex;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{info, warn};

const EXTERNAL_USER_API_MAX_BODY_SIZE: usize = 4096;

#[derive(Deserialize)]
struct ExternalUserVerifyRequest {
	#[serde(alias = "id")]
	username: String,
	#[serde(alias = "pw")]
	password: String,
}

#[derive(Serialize)]
struct ExternalUserVerifyResponse {
	user_id: i64,
	username: String,
	online_name: String,
	avatar_url: String,
	admin: bool,
	banned: bool,
}

#[derive(Deserialize)]
struct AdminSetRankRequest {
	npid: String,
	com_id: String,
	#[serde(default = "default_slot")]
	slot: i32,
	character: usize,
	rank: u8,
	rank_points: Option<u16>,
	/// Write even while the account is connected. The client holds the save in
	/// memory for the length of its session and writes it back on its next
	/// save, so an edit made now is discarded; only set this for a session
	/// known to be stale.
	#[serde(default)]
	force: bool,
}

fn default_slot() -> i32 {
	1
}

#[derive(Serialize)]
struct AdminCharacterRank {
	rank: u8,
	rank_points: u16,
}

#[derive(Serialize)]
struct AdminSetRankResponse {
	npid: String,
	character: usize,
	previous: AdminCharacterRank,
	current: AdminCharacterRank,
	data_id: u64,
}

#[derive(Serialize)]
struct PlayerCharacterRank {
	character: usize,
	rank: u8,
	rank_points: u16,
}

#[derive(Serialize)]
struct PlayerRanksResponse {
	npid: String,
	online_name: String,
	com_id: String,
	slot: i32,
	data_id: u64,
	/// The account's running record as the title itself keeps it, for a title
	/// whose save layout is known. Omitted otherwise.
	#[serde(skip_serializing_if = "Option::is_none")]
	record: Option<PlayerRecord>,
	characters: Vec<PlayerCharacterRank>,
}

#[derive(Serialize)]
struct PlayerRecord {
	wins: u32,
	losses: u32,
}

#[derive(Serialize)]
struct LeaderboardResponse {
	com_id: String,
	slot: i32,
	/// How many accounts hold a readable save, which is what was ranked - not
	/// how many are in `entries`, which `limit` cuts short.
	ranked_players: usize,
	/// Accounts holding a save this title's layout could not be read from.
	unreadable_saves: usize,
	entries: Vec<LeaderboardEntry>,
}

#[derive(Serialize)]
struct LeaderboardEntry {
	position: usize,
	npid: String,
	online_name: String,
	/// The account's highest ranked character, which is also what the client
	/// offers for matchmaking.
	best_rank: u8,
	best_rank_points: u16,
	best_character: usize,
	/// The record the title itself keeps, over the account's whole lifetime.
	record: PlayerRecord,
	/// What this server saw: matches played here whose result it recovered.
	/// Always the smaller pair of numbers, and zero for a player whose matches
	/// all predate the server recording them.
	server_record: PlayerRecord,
}

struct CachedResponse {
	timestamp: AtomicU32,
	cached_response: Mutex<Response<String>>,
}

impl CachedResponse {
	fn new() -> CachedResponse {
		CachedResponse {
			timestamp: AtomicU32::new(0),
			cached_response: Mutex::new(Response::new("".to_string())),
		}
	}
}

struct JsonScoreCache {
	table_cache: Mutex<HashMap<ComId, HashMap<u32, CachedResponse>>>,
	com_id_cache: Mutex<HashMap<ComId, CachedResponse>>,
}

impl JsonScoreCache {
	fn new() -> JsonScoreCache {
		JsonScoreCache {
			table_cache: Mutex::new(HashMap::new()),
			com_id_cache: Mutex::new(HashMap::new()),
		}
	}
}

struct JsonCache {
	usage_cache: CachedResponse,
	score_cache: JsonScoreCache,
	/// Building a leaderboard reads every save of a title, so it is cached
	/// like the score tables rather than rebuilt per request.
	leaderboard_cache: Mutex<HashMap<(ComId, i32), CachedResponse>>,
}

impl JsonCache {
	fn new() -> JsonCache {
		JsonCache {
			usage_cache: CachedResponse::new(),
			score_cache: JsonScoreCache::new(),
			leaderboard_cache: Mutex::new(HashMap::new()),
		}
	}
}

pub struct StatServer {
	listener: TcpListener,
	term_watch: TerminateWatch,
	path: String,
	cache_life: u32,
	game_tracker: Arc<GameTracker>,
	score_cache: Arc<ScoresCache>,
	json_cache: Arc<JsonCache>,
	room_manager: Arc<RwLock<RoomManager>>,
	db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	external_user_api_key: Option<String>,
}

fn sanitize_for_json(s: &str) -> String {
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

impl Server {
	pub async fn start_stat_server(
		&self,
		term_watch: TerminateWatch,
		game_tracker: Arc<GameTracker>,
		room_manager: Arc<RwLock<RoomManager>>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	) -> io::Result<()> {
		let (bind_addr, cache_life, path, external_user_api_key);
		{
			let config = self.config.read();
			bind_addr = config.get_stat_server_binds().clone();
			cache_life = config.get_stat_server_cache_life();
			path = format!("/{}", config.get_stat_server_path());
			external_user_api_key = (!config.get_external_user_api_key().is_empty()).then(|| config.get_external_user_api_key().to_owned());
		}

		let score_cache = self.score_cache.clone();

		if let Some((host, port)) = &bind_addr {
			let str_addr = host.to_owned() + ":" + port;
			let mut addr = str_addr
				.to_socket_addrs()
				.map_err(|e| io::Error::new(e.kind(), format!("Stat: {} is not a valid address", &str_addr)))?;
			let addr = addr
				.next()
				.ok_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, format!("Stat: {} is not a valid address", &str_addr)))?;

			let listener = TcpListener::bind(addr)
				.await
				.map_err(|e| io::Error::new(e.kind(), format!("Stat: error binding to <{}>: {}", &addr, e)))?;

			info!("Stat server now waiting for connections on {}", str_addr);

			let mut stat_server = StatServer::new(listener, term_watch, path, cache_life, game_tracker, score_cache, room_manager, db_pool, external_user_api_key);

			tokio::task::spawn(async move {
				stat_server.server_proc().await;
			});
		}

		Ok(())
	}
}

impl StatServer {
	fn new(
		listener: TcpListener,
		term_watch: TerminateWatch,
		path: String,
		cache_life: u32,
		game_tracker: Arc<GameTracker>,
		score_cache: Arc<ScoresCache>,
		room_manager: Arc<RwLock<RoomManager>>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
		external_user_api_key: Option<String>,
	) -> StatServer {
		StatServer {
			listener,
			term_watch,
			path,
			cache_life,
			game_tracker,
			score_cache,
			json_cache: Arc::new(JsonCache::new()),
			room_manager,
			db_pool,
			external_user_api_key,
		}
	}

	async fn server_proc(&mut self) {
		if *self.term_watch.recv.borrow_and_update() {
			return;
		}

		'stat_server_loop: loop {
			tokio::select! {
				accept_res = self.listener.accept() => {
					if let Err(e) = accept_res {
						warn!("Stat: Error accepting a client: {}", e);
						continue 'stat_server_loop;
					}

					let (stream, peer_addr) = accept_res.unwrap();
					let io = TokioIo::new(stream);

					info!("Stat: new client from {}", peer_addr);
					{
						let path = self.path.clone();
						let cache_life = self.cache_life;
						let game_tracker = self.game_tracker.clone();
						let score_cache = self.score_cache.clone();
						let json_cache = self.json_cache.clone();
						let room_manager = self.room_manager.clone();
						let db_pool = self.db_pool.clone();
						let external_user_api_key = self.external_user_api_key.clone();

						tokio::task::spawn(async move {
							if let Err(err) = http1::Builder::new().keep_alive(false).serve_connection(io, service_fn(|r| StatServer::handle_stat_server_req(r, &path, cache_life, game_tracker.clone(), score_cache.clone(), json_cache.clone(), room_manager.clone(), db_pool.clone(), external_user_api_key.clone()))).await {
								warn!("Stat: Error serving connection: {}", err);
							}
						});
					}
				}
				_ = self.term_watch.recv.changed() => {
					break 'stat_server_loop;
				}
			}
		}
		info!("GameTracker::server_proc terminating");
	}

	fn handle_usage_req(cache_life: u32, game_tracker: &Arc<GameTracker>, json_cache: &Arc<JsonCache>, include_ips: bool) -> Result<Response<String>, Infallible> {
		// The cache holds the public answer. An operator asking for addresses
		// is rare and must not put them where the next caller would be served
		// them, so that answer is built fresh and never stored.
		if cache_life == 0 || include_ips {
			return Ok(Response::builder()
				.header("Content-Type", "application/json")
				.body(StatServer::game_tracker_to_json(game_tracker, include_ips))
				.unwrap());
		}

		let new_timestamp = Client::get_timestamp_seconds();
		let mut response = json_cache.usage_cache.cached_response.lock();

		let is_stale = new_timestamp > json_cache.usage_cache.timestamp.load(Ordering::SeqCst) + cache_life;
		if is_stale {
			*response = Response::builder()
				.header("Content-Type", "application/json")
				.body(StatServer::game_tracker_to_json(game_tracker, false))
				.unwrap();
			// Without this the entry is stale on every request and the answer
			// is rebuilt each time, which is the whole cost the cache exists
			// to avoid.
			json_cache.usage_cache.timestamp.store(new_timestamp, Ordering::SeqCst);
		}

		Ok((*response).clone())
	}

	fn com_id_score_to_json(score_cache: &Arc<ScoresCache>, com_id: &ComId) -> String {
		let mut tables = score_cache.get_all_tables(com_id);
		if tables.is_empty() {
			return "[]".to_owned();
		}

		tables.sort_by_key(|(board_id, _)| *board_id);

		let mut res = String::from("[\n");
		for (index, (board_id, table)) in tables.iter().enumerate() {
			let table_infos = {
				let table = table.read();
				table.table_info.clone()
			};

			let result = score_cache.get_score_range(com_id, *board_id, 1, table_infos.rank_limit, true, true);
			let json = StatServer::score_result_to_json(&result, *board_id, &table_infos);
			res += &json;
			if index != tables.len() - 1 {
				res += ",\n";
			} else {
				res += "\n";
			}
		}
		res += "]";
		res
	}

	fn handle_com_id_score_req(cache_life: u32, score_cache: &Arc<ScoresCache>, json_cache: &Arc<JsonCache>, com_id: &ComId) -> Result<Response<String>, Infallible> {
		if cache_life == 0 {
			let json = StatServer::com_id_score_to_json(score_cache, com_id);
			return Ok(Response::builder().header("Content-Type", "application/json").body(json).unwrap());
		}

		let new_timestamp = Client::get_timestamp_seconds();
		let mut com_id_map = json_cache.score_cache.com_id_cache.lock();
		let cached = com_id_map.entry(*com_id).or_insert_with(CachedResponse::new);

		let is_stale = new_timestamp > cached.timestamp.load(Ordering::SeqCst) + cache_life;
		if is_stale {
			let json = StatServer::com_id_score_to_json(score_cache, com_id);
			*cached.cached_response.lock() = Response::builder().header("Content-Type", "application/json").body(json).unwrap();
			cached.timestamp.store(new_timestamp, Ordering::SeqCst);
		}

		Ok(cached.cached_response.lock().clone())
	}

	fn handle_table_score_req(cache_life: u32, score_cache: &Arc<ScoresCache>, json_cache: &Arc<JsonCache>, com_id: &ComId, table_id: u32) -> Result<Response<String>, Infallible> {
		if let Some(table_cache) = score_cache.get_table(com_id, table_id) {
			let table_infos = {
				let table_cache = table_cache.read();
				table_cache.table_info.clone()
			};

			if cache_life == 0 {
				let result = score_cache.get_score_range(com_id, table_id, 1, table_infos.rank_limit, true, true);
				let json = StatServer::score_result_to_json(&result, table_id, &table_infos);
				return Ok(Response::builder().header("Content-Type", "application/json").body(json).unwrap());
			}

			let new_timestamp = Client::get_timestamp_seconds();
			let mut score_map = json_cache.score_cache.table_cache.lock();
			let table_map = score_map.entry(*com_id).or_default();
			let cached = table_map.entry(table_id).or_insert_with(CachedResponse::new);

			let is_stale = new_timestamp > cached.timestamp.load(Ordering::SeqCst) + cache_life;
			if is_stale {
				let result = score_cache.get_score_range(com_id, table_id, 1, table_infos.rank_limit, true, true);
				let json = StatServer::score_result_to_json(&result, table_id, &table_infos);
				*cached.cached_response.lock() = Response::builder().header("Content-Type", "application/json").body(json).unwrap();
				cached.timestamp.store(new_timestamp, Ordering::SeqCst);
			}

			return Ok(cached.cached_response.lock().clone());
		}

		Ok(Response::new("".to_owned()))
	}

	fn json_response(status: StatusCode, body: String) -> Response<String> {
		Response::builder().status(status).header("Content-Type", "application/json").body(body).unwrap()
	}

	/// Whether a request carries the operator API key.
	///
	/// Compared the same way as on the endpoints that require it. Used here to
	/// decide how much of a response to fill in, not to allow or refuse a
	/// request.
	fn is_operator(req: &Request<hyper::body::Incoming>, external_user_api_key: &Option<String>) -> bool {
		let Some(expected) = external_user_api_key else {
			return false;
		};
		req.headers()
			.get("X-API-Key")
			.and_then(|value| value.to_str().ok())
			.is_some_and(|value| value.len() == expected.len() && memcmp::eq(value.as_bytes(), expected.as_bytes()))
	}

	/// Sets one character's rank for an operator.
	///
	/// This writes a new save exactly the way the game does - a fresh data_id,
	/// the slot repointed at it, a history row - rather than overwriting the
	/// file in place, so nothing else has to know the edit happened.
	async fn handle_admin_set_rank_req(
		req: Request<hyper::body::Incoming>,
		external_user_api_key: &str,
		game_tracker: Arc<GameTracker>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	) -> Result<Response<String>, Infallible> {
		if req.method() != Method::POST {
			return Ok(StatServer::json_response(StatusCode::METHOD_NOT_ALLOWED, "{\"error\":\"method_not_allowed\"}".to_owned()));
		}

		let is_authorized = req
			.headers()
			.get("X-API-Key")
			.and_then(|value| value.to_str().ok())
			.is_some_and(|value| value.len() == external_user_api_key.len() && memcmp::eq(value.as_bytes(), external_user_api_key.as_bytes()));
		if !is_authorized {
			return Ok(StatServer::json_response(StatusCode::FORBIDDEN, "{\"error\":\"forbidden\"}".to_owned()));
		}

		let body = match Limited::new(req.into_body(), EXTERNAL_USER_API_MAX_BODY_SIZE).collect().await {
			Ok(body) => body.to_bytes(),
			Err(_) => return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_request\"}".to_owned())),
		};
		let request: AdminSetRankRequest = match serde_json::from_slice::<AdminSetRankRequest>(&body) {
			Ok(request) if !request.npid.is_empty() && request.com_id.len() == COMMUNICATION_ID_SIZE => request,
			_ => return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_request\"}".to_owned())),
		};
		if request.character >= game_specific_tus::CHARACTERS {
			return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"no_such_character\"}".to_owned()));
		}

		let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
		com_id.copy_from_slice(request.com_id.as_bytes());

		// Look the account up, and take the save the slot currently points at.
		let lookup_pool = db_pool.clone();
		let lookup_npid = request.npid.clone();
		let lookup = tokio::task::spawn_blocking(move || {
			let connection = lookup_pool.get().map_err(|_| DbError::Internal)?;
			let db = Database::new(connection);
			let user_id = db.get_user_id(&lookup_npid)?;
			let online_name = db.get_online_name(user_id)?;
			let (status, info) = db.tus_get_user_data(&com_id, user_id, request.slot)?;
			Ok::<_, DbError>((user_id, online_name, status.data_id, info))
		})
		.await;

		let (user_id, online_name, previous_data_id, info) = match lookup {
			Ok(Ok(found)) => found,
			Ok(Err(DbError::Empty)) => return Ok(StatServer::json_response(StatusCode::NOT_FOUND, "{\"error\":\"not_found\"}".to_owned())),
			_ => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};

		if !request.force && StatServer::is_player_online(&game_tracker, &com_id, &online_name) {
			return Ok(StatServer::json_response(StatusCode::CONFLICT, "{\"error\":\"player_online\"}".to_owned()));
		}

		let current = match Client::get_tus_data_file(previous_data_id).await {
			Ok(data) => data,
			Err(_) => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"save_unreadable\"}".to_owned())),
		};

		let (edited, previous) = match game_specific_tus::set_character_rank(&com_id, &current, request.character, request.rank, request.rank_points) {
			Ok(result) => result,
			Err(game_specific_tus::EditError::UnsupportedTitle) => {
				return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"unsupported_title\"}".to_owned()));
			}
			Err(game_specific_tus::EditError::NoSuchCharacter) => {
				return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"no_such_character\"}".to_owned()));
			}
			Err(game_specific_tus::EditError::MalformedSave) => {
				return Ok(StatServer::json_response(StatusCode::CONFLICT, "{\"error\":\"malformed_save\"}".to_owned()));
			}
		};

		let data_id = Client::create_tus_data_file(&edited).await;
		if data_id == 0 {
			return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"save_not_written\"}".to_owned()));
		}

		let timestamp = Client::get_psn_timestamp();
		let slot = request.slot;
		let stored = tokio::task::spawn_blocking(move || {
			let connection = db_pool.get().map_err(|_| DbError::Internal)?;
			let db = Database::new(connection);
			// The slot's info blob is carried over untouched; only the save
			// behind it changed.
			let info = if info.is_empty() { None } else { Some(info.as_slice()) };
			db.tus_set_user_data(&com_id, user_id, slot, data_id, &info, user_id, timestamp, None, None)?;
			if let Err(e) = db.tus_record_data_history(&com_id, user_id, slot, data_id, timestamp) {
				warn!("Failed to record tus data history for operator edit {}: {:?}", data_id, e);
			}
			Ok::<_, DbError>(())
		})
		.await;

		if !matches!(stored, Ok(Ok(()))) {
			return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned()));
		}

		info!("Operator set {} character {} from rank {} to rank {}", request.npid, request.character, previous.rank, request.rank);

		let response = AdminSetRankResponse {
			npid: request.npid,
			character: request.character,
			previous: AdminCharacterRank {
				rank: previous.rank,
				rank_points: previous.rank_points,
			},
			current: AdminCharacterRank {
				rank: request.rank,
				rank_points: request.rank_points.unwrap_or_else(|| {
					game_specific_tus::read_character_ranks(&com_id, &edited)
						.ok()
						.and_then(|ranks| ranks.get(request.character).map(|c| c.rank_points))
						.unwrap_or(0)
				}),
			},
			data_id,
		};
		match serde_json::to_string(&response) {
			Ok(json) => Ok(Response::builder().header("Content-Type", "application/json").body(json).unwrap()),
			Err(_) => Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		}
	}

	fn is_player_online(game_tracker: &Arc<GameTracker>, com_id: &ComId, online_name: &str) -> bool {
		game_tracker.psn_games.read().get(com_id).is_some_and(|game| game.players.read().contains_key(online_name))
	}

	async fn handle_external_user_verify_req(
		req: Request<hyper::body::Incoming>,
		external_user_api_key: &str,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	) -> Result<Response<String>, Infallible> {
		if req.method() != Method::POST {
			return Ok(StatServer::json_response(StatusCode::METHOD_NOT_ALLOWED, "{\"error\":\"method_not_allowed\"}".to_owned()));
		}

		let is_authorized = req
			.headers()
			.get("X-API-Key")
			.and_then(|value| value.to_str().ok())
			.is_some_and(|value| value.len() == external_user_api_key.len() && memcmp::eq(value.as_bytes(), external_user_api_key.as_bytes()));
		if !is_authorized {
			return Ok(StatServer::json_response(StatusCode::FORBIDDEN, "{\"error\":\"forbidden\"}".to_owned()));
		}

		let body = match Limited::new(req.into_body(), EXTERNAL_USER_API_MAX_BODY_SIZE).collect().await {
			Ok(body) => body.to_bytes(),
			Err(_) => return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_request\"}".to_owned())),
		};
		let request: ExternalUserVerifyRequest = match serde_json::from_slice::<ExternalUserVerifyRequest>(&body) {
			Ok(request) if !request.username.is_empty() && !request.password.is_empty() => request,
			_ => return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_request\"}".to_owned())),
		};

		let verification = tokio::task::spawn_blocking(move || {
			let connection = db_pool.get().map_err(|_| DbError::Internal)?;
			Database::new(connection).check_user(&request.username, &request.password, "", false)
		})
		.await;

		match verification {
			Ok(Ok(user)) => {
				let response = ExternalUserVerifyResponse {
					user_id: user.user_id,
					username: user.username,
					online_name: user.online_name,
					avatar_url: user.avatar_url,
					admin: user.admin,
					banned: user.banned,
				};
				Ok(StatServer::json_response(StatusCode::OK, serde_json::to_string(&response).unwrap()))
			}
			Ok(Err(DbError::Empty | DbError::WrongPass)) => Ok(StatServer::json_response(StatusCode::UNAUTHORIZED, "{\"error\":\"invalid_credentials\"}".to_owned())),
			Ok(Err(_)) | Err(_) => Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		}
	}

	/// Entry point for the stat server.
	///
	/// Everything the routing below serves is meant to be readable by a page
	/// in a browser, so the answers carry a permissive cross origin header.
	/// Only the reading is opened up: a preflight is answered for GET alone
	/// and does not allow `X-API-Key`, so the endpoints that take the operator
	/// key stay reachable from a backend and not from someone else's page.
	async fn handle_stat_server_req(
		req: Request<hyper::body::Incoming>,
		path: &str,
		cache_life: u32,
		game_tracker: Arc<GameTracker>,
		score_cache: Arc<ScoresCache>,
		json_cache: Arc<JsonCache>,
		room_manager: Arc<RwLock<RoomManager>>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
		external_user_api_key: Option<String>,
	) -> Result<Response<String>, Infallible> {
		if req.method() == Method::OPTIONS {
			return Ok(Response::builder()
				.status(StatusCode::NO_CONTENT)
				.header("Access-Control-Allow-Origin", "*")
				.header("Access-Control-Allow-Methods", "GET, OPTIONS")
				.header("Access-Control-Allow-Headers", "Content-Type")
				.header("Access-Control-Max-Age", "86400")
				.body(String::new())
				.unwrap());
		}

		let mut response = StatServer::route_stat_server_req(req, path, cache_life, game_tracker, score_cache, json_cache, room_manager, db_pool, external_user_api_key).await?;
		response.headers_mut().insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
		Ok(response)
	}

	async fn route_stat_server_req(
		req: Request<hyper::body::Incoming>,
		path: &str,
		cache_life: u32,
		game_tracker: Arc<GameTracker>,
		score_cache: Arc<ScoresCache>,
		json_cache: Arc<JsonCache>,
		room_manager: Arc<RwLock<RoomManager>>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
		external_user_api_key: Option<String>,
	) -> Result<Response<String>, Infallible> {
		let req_path = req.uri().path();
		let external_user_verify_path = format!("{}/external/users/verify", path);
		if req_path == external_user_verify_path {
			if let Some(external_user_api_key) = external_user_api_key {
				return StatServer::handle_external_user_verify_req(req, &external_user_api_key, db_pool).await;
			}
			return Ok(Response::builder().status(StatusCode::NOT_FOUND).body("".to_owned()).unwrap());
		}

		let admin_set_rank_path = format!("{}/admin/character-rank", path);
		if req_path == admin_set_rank_path {
			if let Some(external_user_api_key) = external_user_api_key {
				return StatServer::handle_admin_set_rank_req(req, &external_user_api_key, game_tracker, db_pool).await;
			}
			return Ok(Response::builder().status(StatusCode::NOT_FOUND).body("".to_owned()).unwrap());
		}

		if req.method() != Method::GET {
			return Ok(Response::new("".to_owned()));
		}

		let usage_path = format!("{}/usage", path);
		let score_prefix = format!("{}/score/", path);
		let rooms_prefix = format!("{}/rooms/", path);
		let matches_prefix = format!("{}/matches/", path);
		let player_matches_prefix = format!("{}/players/", path);
		let leaderboard_prefix = format!("{}/leaderboard/", path);

		if req_path == usage_path {
			let include_ips = StatServer::is_operator(&req, &external_user_api_key);
			return StatServer::handle_usage_req(cache_life, &game_tracker, &json_cache, include_ips);
		}

		// /leaderboard/<com_id>[?limit=n][&slot=n] : every account by rank
		if let Some(rest) = req_path.strip_prefix(&leaderboard_prefix) {
			if rest.len() == COMMUNICATION_ID_SIZE {
				let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
				com_id.copy_from_slice(rest.as_bytes());
				let query = req.uri().query();
				// A whole population is a reasonable thing to ask for here,
				// unlike a match list, so the ceiling is higher.
				let limit = StatServer::parse_limit_with(query, 100, 10_000);
				let slot = StatServer::query_param(query, "slot").and_then(|v| v.parse::<i32>().ok()).unwrap_or(1);
				return StatServer::handle_leaderboard_req(&com_id, rest, slot, limit, cache_life, &json_cache, db_pool).await;
			}
		}

		// /matches/<com_id>[?limit=n] : the most recent finished matches
		if let Some(rest) = req_path.strip_prefix(&matches_prefix) {
			if rest.len() == COMMUNICATION_ID_SIZE {
				let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
				com_id.copy_from_slice(rest.as_bytes());
				let limit = StatServer::parse_limit(req.uri().query());
				return StatServer::handle_recent_matches_req(&com_id, limit, db_pool);
			}
		}

		// /players/<npid>/matches[?limit=n] : one account's match history
		if let Some(rest) = req_path.strip_prefix(&player_matches_prefix) {
			if let Some(npid) = rest.strip_suffix("/matches") {
				let limit = StatServer::parse_limit(req.uri().query());
				return StatServer::handle_player_matches_req(npid, limit, db_pool);
			}

			// /players/<npid>/ranks?com_id=...[&slot=n] : per character ranks
			if let Some(npid) = rest.strip_suffix("/ranks") {
				let query = req.uri().query();
				let Some(com_id_str) = StatServer::query_param(query, "com_id") else {
					return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"com_id_required\"}".to_owned()));
				};
				let slot = StatServer::query_param(query, "slot").and_then(|v| v.parse::<i32>().ok()).unwrap_or(1);
				return StatServer::handle_player_ranks_req(npid, com_id_str, slot, db_pool).await;
			}
		}

		if let Some(com_id_str) = req_path.strip_prefix(&rooms_prefix) {
			if com_id_str.len() == COMMUNICATION_ID_SIZE {
				let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
				com_id.copy_from_slice(com_id_str.as_bytes());
				return StatServer::handle_rooms_req(&room_manager, &com_id);
			}
		}

		if let Some(rest) = req_path.strip_prefix(&score_prefix) {
			let parts: Vec<&str> = rest.splitn(2, '/').collect();
			let com_id_str = parts[0];
			if com_id_str.len() == COMMUNICATION_ID_SIZE {
				let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
				com_id.copy_from_slice(com_id_str.as_bytes());

				if parts.len() == 2 {
					if let Ok(table_id) = parts[1].parse::<u32>() {
						return StatServer::handle_table_score_req(cache_life, &score_cache, &json_cache, &com_id, table_id);
					}
				} else {
					return StatServer::handle_com_id_score_req(cache_life, &score_cache, &json_cache, &com_id);
				}
			}
		}

		Ok(Response::new("".to_owned()))
	}

	/// The usage summary.
	///
	/// `include_ips` fills in each player's address, which the tracker holds
	/// alongside their name. That is not something the game shows anyone, and
	/// this endpoint has always been unauthenticated, so it is off unless the
	/// caller presents the operator API key; the names are reported either
	/// way, with a null in place of each address.
	fn game_tracker_to_json(game_tracker: &Arc<GameTracker>, include_ips: bool) -> String {
		let psn_games: Vec<(String, i64, Vec<String>)> = game_tracker
			.psn_games
			.read()
			.iter()
			.filter_map(|(name, game_info)| {
				let num_users = game_info.num_users.load(Ordering::SeqCst);
				if num_users != 0 {
					Some((com_id_to_string(name), num_users, game_info.name_hints.read().iter().cloned().collect()))
				} else {
					None
				}
			})
			.collect();

		let ticket_games: Vec<(String, i64)> = game_tracker
			.ticket_games
			.read()
			.iter()
			.filter_map(|(name, num_users)| {
				let num_users = num_users.load(Ordering::SeqCst);
				if num_users != 0 { Some((name.clone(), num_users)) } else { None }
			})
			.collect();

		let mut res = String::from("{\n");
		let _ = write!(res, "    \"num_users\" : {}", game_tracker.num_users.load(Ordering::SeqCst));

		// The game id doesn't need to be sanitized as it is composed only of alphanumerical ascii chars(checked before being passed to game tracker)
		let add_games_with_hints = |string: &mut String, section_name: &str, v: &Vec<(String, i64, Vec<String>)>| {
			if !v.is_empty() {
				let _ = writeln!(string, ",\n    \"{}\": {{", section_name);

				for (index, (name, num, name_hints)) in v.iter().enumerate() {
					let _ = write!(string, "        \"{}\": [{}", name, num);
					for hint in name_hints {
						let _ = write!(string, ", \"{}\"", sanitize_for_json(hint));
					}
					let _ = write!(string, "]");
					*string += if index != (v.len() - 1) { ",\n" } else { "\n" };
				}

				*string += "    }"
			}
		};

		let add_games = |string: &mut String, section_name: &str, v: &Vec<(String, i64)>| {
			if !v.is_empty() {
				let _ = writeln!(string, ",\n    \"{}\": {{", section_name);

				for (index, (name, num)) in v.iter().enumerate() {
					let _ = write!(string, "        \"{}\": {}", name, num);
					*string += if index != (v.len() - 1) { ",\n" } else { "\n" };
				}

				*string += "    }"
			}
		};

		add_games_with_hints(&mut res, "psn_games", &psn_games);
		add_games(&mut res, "ticket_games", &ticket_games);

		let psn_games_read = game_tracker.psn_games.read();
		let has_players = psn_games_read.values().any(|g| !g.players.read().is_empty());

		if has_players {
			let _ = writeln!(res, ",\n    \"players_id\": {{");
			let entries: Vec<_> = psn_games_read.iter().filter(|(_, g)| !g.players.read().is_empty()).collect();

			for (index, (com_id, game_info)) in entries.iter().enumerate() {
				let com_id_str = com_id_to_string(com_id);
				let _ = writeln!(res, "        \"{}\": {{", com_id_str);
				let players = game_info.players.read();
				for (i, (name, ip)) in players.iter().enumerate() {
					let comma = if i != players.len() - 1 { "," } else { "" };
					if include_ips {
						let _ = write!(res, "            \"{}\": \"{}\"{}", sanitize_for_json(name), ip, comma);
					} else {
						let _ = write!(res, "            \"{}\": null{}", sanitize_for_json(name), comma);
					}
					res += "\n";
				}
				res += if index != entries.len() - 1 { "        },\n" } else { "        }\n" };
			}
			res += "    }";
		}

		res += "\n}";

		res
	}

	fn score_result_to_json(result: &GetScoreResultCache, board_id: u32, table_infos: &DbBoardInfo) -> String {
		let mut res = String::from("{\n");
		let _ = writeln!(res, "    \"board_id\": {},", board_id);
		let _ = writeln!(res, "    \"rank_limit\": {},", table_infos.rank_limit);
		let _ = writeln!(res, "    \"update_mode\": {},", table_infos.update_mode);
		let _ = writeln!(res, "    \"sort_mode\": {},", table_infos.sort_mode);
		let _ = writeln!(res, "    \"upload_num_limit\": {},", table_infos.upload_num_limit);
		let _ = writeln!(res, "    \"upload_size_limit\": {},", table_infos.upload_size_limit);
		let _ = writeln!(res, "    \"total_records\": {},", result.total_records);
		let _ = writeln!(res, "    \"scores\": [");

		for (index, score) in result.scores.iter().enumerate() {
			// NPID is inherently json safe as it is verified at user creation
			let online_name = sanitize_for_json(&score.online_name);
			let comment = result.comments.as_ref().map(|c| sanitize_for_json(&c[index])).unwrap_or_default();
			let game_info = result
				.infos
				.as_ref()
				.map(|g| g[index].iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(""))
				.unwrap_or_default();

			let _ = writeln!(res, "        {{\n");
			let _ = writeln!(res, "            \"rank\": {},", score.rank + 1);
			let _ = writeln!(res, "            \"npid\": \"{}\",", score.npid);
			let _ = writeln!(res, "            \"online_name\": \"{}\",", online_name);
			let _ = writeln!(res, "            \"pcid\": {},", score.pcid);
			let _ = writeln!(res, "            \"score\": {},", score.score);
			let _ = writeln!(res, "            \"has_gamedata\": {},", score.has_gamedata);
			let _ = writeln!(res, "            \"comment\": \"{}\",", comment);
			let _ = writeln!(res, "            \"info\": \"{}\",", game_info);
			let _ = writeln!(res, "            \"timestamp\": {}", score.timestamp);
			let _ = writeln!(res, "        }}");
			if index != result.scores.len() - 1 {
				res += ",\n";
			} else {
				res += "\n";
			}
		}

		res += "    ]\n}";
		res
	}

	/// `limit` query parameter, clamped so a caller cannot ask for everything.
	fn parse_limit(query: Option<&str>) -> u32 {
		StatServer::parse_limit_with(query, 50, 500)
	}

	fn parse_limit_with(query: Option<&str>, default: u32, max: u32) -> u32 {
		StatServer::query_param(query, "limit").and_then(|v| v.parse::<u32>().ok()).unwrap_or(default).clamp(1, max)
	}

	fn handle_recent_matches_req(com_id: &ComId, limit: u32, db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) -> Result<Response<String>, Infallible> {
		let conn = match db_pool.get() {
			Ok(conn) => conn,
			Err(_) => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};
		match Database::new(conn).get_recent_matches(com_id, limit) {
			Ok(matches) => Ok(Response::builder().header("Content-Type", "application/json").body(StatServer::matches_to_json(&matches)).unwrap()),
			Err(_) => Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		}
	}

	fn handle_player_matches_req(npid: &str, limit: u32, db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) -> Result<Response<String>, Infallible> {
		if !Client::is_valid_client_username(npid) {
			return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_npid\"}".to_owned()));
		}
		let conn = match db_pool.get() {
			Ok(conn) => conn,
			Err(_) => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};
		let db = Database::new(conn);
		let user_id = match db.get_user_id(npid) {
			Ok(user_id) => user_id,
			Err(DbError::Empty) => return Ok(StatServer::json_response(StatusCode::NOT_FOUND, "{\"error\":\"not_found\"}".to_owned())),
			Err(_) => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};
		match db.get_matches_for_user(user_id, limit) {
			Ok(matches) => Ok(Response::builder().header("Content-Type", "application/json").body(StatServer::matches_to_json(&matches)).unwrap()),
			Err(_) => Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		}
	}

	/// One account's per character ranks and running record, read out of the
	/// save it last wrote.
	///
	/// Unauthenticated, like the rooms and score endpoints: ranks are what the
	/// game shows to everyone in a lobby anyway.
	async fn handle_player_ranks_req(npid: &str, com_id_str: &str, slot: i32, db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) -> Result<Response<String>, Infallible> {
		if !Client::is_valid_client_username(npid) || com_id_str.len() != COMMUNICATION_ID_SIZE {
			return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"invalid_request\"}".to_owned()));
		}
		let mut com_id: ComId = [0u8; COMMUNICATION_ID_SIZE];
		com_id.copy_from_slice(com_id_str.as_bytes());

		let owned_npid = npid.to_owned();
		let lookup = tokio::task::spawn_blocking(move || {
			let connection = db_pool.get().map_err(|_| DbError::Internal)?;
			let db = Database::new(connection);
			let user_id = db.get_user_id(&owned_npid)?;
			let online_name = db.get_online_name(user_id)?;
			let (status, _) = db.tus_get_user_data(&com_id, user_id, slot)?;
			Ok::<_, DbError>((online_name, status.data_id))
		})
		.await;

		let (online_name, data_id) = match lookup {
			Ok(Ok(found)) => found,
			Ok(Err(DbError::Empty)) => return Ok(StatServer::json_response(StatusCode::NOT_FOUND, "{\"error\":\"not_found\"}".to_owned())),
			_ => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};

		let save = match Client::get_tus_data_file(data_id).await {
			Ok(save) => save,
			Err(_) => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"save_unreadable\"}".to_owned())),
		};

		let characters = match game_specific_tus::read_character_ranks(&com_id, &save) {
			Ok(characters) => characters,
			Err(game_specific_tus::EditError::UnsupportedTitle) => {
				return Ok(StatServer::json_response(StatusCode::BAD_REQUEST, "{\"error\":\"unsupported_title\"}".to_owned()));
			}
			Err(_) => return Ok(StatServer::json_response(StatusCode::CONFLICT, "{\"error\":\"malformed_save\"}".to_owned())),
		};

		let response = PlayerRanksResponse {
			npid: npid.to_owned(),
			online_name,
			com_id: com_id_str.to_owned(),
			slot,
			data_id,
			record: game_specific_tus::account_record(&com_id, &save).map(|(wins, losses)| PlayerRecord { wins, losses }),
			characters: characters
				.into_iter()
				.map(|c| PlayerCharacterRank {
					character: c.character,
					rank: c.rank,
					rank_points: c.rank_points,
				})
				.collect(),
		};
		match serde_json::to_string(&response) {
			Ok(json) => Ok(Response::builder().header("Content-Type", "application/json").body(json).unwrap()),
			Err(_) => Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		}
	}

	/// Every account of a title ordered by its best character's rank.
	///
	/// Unauthenticated, like the other read endpoints: a rank is what the game
	/// shows to everyone in a lobby anyway.
	///
	/// The ranks are not in any table - the title keeps them inside the save -
	/// so this reads every save of the title. That is a few megabytes for a
	/// population in the hundreds, but it is per request, so the result is
	/// cached for `cache_life` seconds like the score tables.
	async fn handle_leaderboard_req(
		com_id: &ComId,
		com_id_str: &str,
		slot: i32,
		limit: u32,
		cache_life: u32,
		json_cache: &Arc<JsonCache>,
		db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	) -> Result<Response<String>, Infallible> {
		let new_timestamp = Client::get_timestamp_seconds();
		let key = (*com_id, slot);

		// Building a board reads one file per account, so unlike the other
		// endpoints this one is never served uncached: a configuration that
		// turns caching off would otherwise let any caller spend the whole
		// population's worth of reads per request.
		const MINIMUM_CACHE_LIFE: u32 = 30;
		let cache_life = cache_life.max(MINIMUM_CACHE_LIFE);

		// The cache holds the whole board; `limit` only trims the reply, so a
		// different limit still hits the same entry. Never held across an
		// await: the save reads below would be holding a blocking lock.
		let fresh = {
			let cache = json_cache.leaderboard_cache.lock();
			cache
				.get(&key)
				.and_then(|cached| (new_timestamp <= cached.timestamp.load(Ordering::SeqCst) + cache_life).then(|| cached.cached_response.lock().body().clone()))
		};
		if let Some(body) = fresh {
			return Ok(StatServer::trim_leaderboard(body, limit));
		}

		let owned_com_id = *com_id;
		let listing = tokio::task::spawn_blocking(move || {
			let connection = db_pool.get().map_err(|_| DbError::Internal)?;
			let db = Database::new(connection);
			let owners = db.tus_list_slot_owners(&owned_com_id, slot)?;
			let tallies = db.get_match_tallies(&owned_com_id)?;
			Ok::<_, DbError>((owners, tallies))
		})
		.await;

		let (owners, tallies) = match listing {
			Ok(Ok(found)) => found,
			_ => return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned())),
		};

		let mut ranked = Vec::with_capacity(owners.len());
		let mut unreadable = 0usize;

		for owner in owners {
			let Ok(save) = Client::get_tus_data_file(owner.data_id).await else {
				unreadable += 1;
				continue;
			};
			let Ok(characters) = game_specific_tus::read_character_ranks(com_id, &save) else {
				unreadable += 1;
				continue;
			};
			let Some(best) = characters.iter().max_by_key(|c| (c.rank, c.rank_points)) else {
				unreadable += 1;
				continue;
			};
			let (wins, losses) = game_specific_tus::account_record(com_id, &save).unwrap_or((0, 0));
			let (server_wins, server_losses) = tallies.get(&owner.user_id).copied().unwrap_or((0, 0));

			ranked.push(LeaderboardEntry {
				position: 0,
				npid: owner.npid,
				online_name: owner.online_name,
				best_rank: best.rank,
				best_rank_points: best.rank_points,
				best_character: best.character,
				record: PlayerRecord { wins, losses },
				server_record: PlayerRecord {
					wins: server_wins,
					losses: server_losses,
				},
			});
		}

		// Ties are broken all the way down to the npid so that two requests
		// that see the same data produce the same board.
		ranked.sort_by(|a, b| {
			b.best_rank
				.cmp(&a.best_rank)
				.then(b.best_rank_points.cmp(&a.best_rank_points))
				.then(b.record.wins.cmp(&a.record.wins))
				.then(a.npid.cmp(&b.npid))
		});
		for (index, entry) in ranked.iter_mut().enumerate() {
			entry.position = index + 1;
		}

		let response = LeaderboardResponse {
			com_id: com_id_str.to_owned(),
			slot,
			ranked_players: ranked.len(),
			unreadable_saves: unreadable,
			entries: ranked,
		};

		let Ok(json) = serde_json::to_string(&response) else {
			return Ok(StatServer::json_response(StatusCode::INTERNAL_SERVER_ERROR, "{\"error\":\"internal_error\"}".to_owned()));
		};

		{
			let mut cache = json_cache.leaderboard_cache.lock();
			let cached = cache.entry(key).or_insert_with(CachedResponse::new);
			*cached.cached_response.lock() = Response::new(json.clone());
			cached.timestamp.store(new_timestamp, Ordering::SeqCst);
		}

		Ok(StatServer::trim_leaderboard(json, limit))
	}

	/// Cuts a cached board down to the entries a request asked for.
	///
	/// The board is cached whole, so this re-reads it rather than keeping a
	/// copy per limit. `ranked_players` stays the size of the whole board.
	fn trim_leaderboard(json: String, limit: u32) -> Response<String> {
		let parsed = serde_json::from_str::<serde_json::Value>(&json);
		let trimmed = match parsed {
			Ok(mut value) => {
				if let Some(entries) = value.get_mut("entries").and_then(|e| e.as_array_mut()) {
					entries.truncate(limit as usize);
				}
				serde_json::to_string(&value).unwrap_or(json)
			}
			Err(_) => json,
		};
		Response::builder().header("Content-Type", "application/json").body(trimmed).unwrap()
	}

	/// A query parameter, without pulling in a query string parser.
	fn query_param<'a>(query: Option<&'a str>, name: &str) -> Option<&'a str> {
		query.and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix(name).and_then(|rest| rest.strip_prefix('='))))
	}

	fn matches_to_json(matches: &[DbMatchRecord]) -> String {
		let mut res = String::from("[\n");
		for (index, m) in matches.iter().enumerate() {
			// NPIDs are validated at account creation, but the sanitizer keeps
			// this honest if that ever changes.
			let _ = writeln!(res, "  {{");
			let _ = writeln!(res, "    \"match_id\": {},", m.match_id);
			let _ = writeln!(res, "    \"room_id\": {},", m.room_id);
			let _ = writeln!(res, "    \"timestamp\": {},", m.timestamp);
			let _ = writeln!(res, "    \"players\": [\"{}\", \"{}\"],", sanitize_for_json(&m.npid_1), sanitize_for_json(&m.npid_2));
			// null means the result was never recovered, not a draw: nothing
			// reports the result of a match directly and the save it is read
			// from may never arrive.
			match &m.winner_npid {
				Some(winner) => {
					let _ = writeln!(res, "    \"winner\": \"{}\"", sanitize_for_json(winner));
				}
				None => {
					let _ = writeln!(res, "    \"winner\": null");
				}
			}
			res += if index != matches.len() - 1 { "  },\n" } else { "  }\n" };
		}
		res += "]";
		res
	}

	fn handle_rooms_req(room_manager: &Arc<RwLock<RoomManager>>, com_id: &ComId) -> Result<Response<String>, Infallible> {
		let json = StatServer::rooms_to_json(room_manager, com_id);
		Ok(Response::builder().header("Content-Type", "application/json").body(json).unwrap())
	}

	fn rooms_to_json(room_manager: &Arc<RwLock<RoomManager>>, com_id: &ComId) -> String {
		let rm = room_manager.read();
		let mut res = String::from("[\n");
		let mut first_room = true;

		for ((c_id, room_id), room) in rm.get_rooms() {
			if c_id != com_id {
				continue;
			}

			if !first_room {
				res += ",\n";
			}
			first_room = false;

			let _ = writeln!(res, "  {{");
			let _ = writeln!(res, "    \"room_id\": {},", room_id);
			let _ = writeln!(res, "    \"world_id\": {},", room.world_id);
			let _ = writeln!(res, "    \"lobby_id\": {},", room.lobby_id);
			let _ = writeln!(res, "    \"max_slot\": {},", room.max_slot);
			let _ = writeln!(res, "    \"cur_member_num\": {},", room.users.len());
			let _ = writeln!(res, "    \"flag_attr\": {},", room.flag_attr);
			let _ = writeln!(res, "    \"has_password\": {},", room.room_password.is_some());
			res += "    \"members\": [\n";

			for (i, user) in room.users.values().enumerate() {
				let comma = if i != room.users.len() - 1 { "," } else { "" };
				let _ = writeln!(
					res,
					"      {{ \"npid\": \"{}\", \"online_name\": \"{}\", \"is_owner\": {} }}{}",
					sanitize_for_json(&user.npid),
					sanitize_for_json(&user.online_name),
					user.flag_attr & 0x80000000 != 0,
					comma
				);
			}

			res += "    ]\n";
			res += "  }";
		}

		res += "\n]";
		res
	}
}
