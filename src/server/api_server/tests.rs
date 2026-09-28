use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use parking_lot::RwLock;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tower::ServiceExt;

use super::router;
use super::state::ApiState;
use crate::server::client::{ClientInfo, ClientSharedInfo};
use crate::server::database::{Database, apply_migrations};
use crate::server::room_manager::RoomManager;

const API_KEY: &str = "test-key";
const PASSWORD: &str = "correct-password";

struct Fixture {
	app: Router,
	db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	client_infos: Arc<RwLock<HashMap<i64, ClientSharedInfo>>>,
}

impl Fixture {
	fn new() -> Fixture {
		Fixture::with_api_key(Some(API_KEY))
	}

	fn with_api_key(api_key: Option<&str>) -> Fixture {
		// 인메모리 DB는 연결마다 따로 생기므로 연결을 하나만 둔다
		let db_pool = r2d2::Pool::builder().max_size(1).build(r2d2_sqlite::SqliteConnectionManager::memory()).unwrap();
		apply_migrations(&db_pool.get().unwrap()).unwrap();

		let client_infos = Arc::new(RwLock::new(HashMap::new()));
		let state = Arc::new(ApiState {
			db_pool: db_pool.clone(),
			client_infos: client_infos.clone(),
			room_manager: Arc::new(RwLock::new(RoomManager::new())),
			api_key: api_key.map(str::to_owned),
		});

		Fixture {
			app: router(state),
			db_pool,
			client_infos,
		}
	}

	fn add_user(&self, npid: &str, online_name: &str, admin: bool) -> i64 {
		let db = Database::new(self.db_pool.get().unwrap());
		let email = format!("{}@example.com", npid);
		db.add_user(npid, PASSWORD, online_name, "https://example.com/avatar.png", &email, &email, admin).unwrap();
		db.get_user_id(npid).unwrap()
	}

	fn ban_in_db(&self, user_id: i64) {
		Database::new(self.db_pool.get().unwrap()).ban_user(user_id).unwrap();
	}

	// 로그인 처리(cmd_account)가 client_infos에 넣는 것과 같은 항목을 만든다. 반환값으로 kick 여부를 확인한다
	fn log_in(&self, user_id: i64, npid: &str, online_name: &str, ip: &str) -> Arc<Notify> {
		let client_info = ClientInfo {
			user_id,
			npid: npid.to_owned(),
			online_name: online_name.to_owned(),
			avatar_url: String::new(),
			token: String::new(),
			admin: false,
			stat_agent: false,
			banned: false,
		};
		let kick_notify = Arc::new(Notify::new());
		let (channel, _) = mpsc::channel(1);
		let ip: IpAddr = ip.parse().unwrap();
		let shared_info = ClientSharedInfo::new(HashMap::new(), channel, kick_notify.clone(), &client_info, ip);
		self.client_infos.write().insert(user_id, shared_info);
		kick_notify
	}

	async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
		let res = self.app.clone().oneshot(req).await.unwrap();
		let status = res.status();
		let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
		let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
		(status, body)
	}
}

fn get(path: &str) -> Request<Body> {
	Request::get(path).header("X-API-Key", API_KEY).body(Body::empty()).unwrap()
}

// Content-Type 없이 보낸다. 기존 소비자가 헤더 없이 보내도 받아야 한다
fn post(path: &str, body: Value) -> Request<Body> {
	post_raw(path, body.to_string())
}

fn post_raw(path: &str, body: String) -> Request<Body> {
	Request::post(path).header("X-API-Key", API_KEY).body(Body::from(body)).unwrap()
}

fn admin_request(admin_npid: &str, target: &str) -> Value {
	json!({ "admin_username": admin_npid, "admin_password": PASSWORD, "username": target })
}

fn error(code: &str) -> Value {
	json!({ "error": code })
}

async fn is_kicked(kick_notify: &Notify) -> bool {
	tokio::time::timeout(Duration::from_millis(100), kick_notify.notified()).await.is_ok()
}

// ---------------------------------------------------------------- API 키

#[tokio::test]
async fn keyed_routes_are_hidden_while_no_api_key_is_configured() {
	let fx = Fixture::with_api_key(None);

	let (status, body) = fx.send(get("/admin/sessions")).await;

	assert_eq!(status, StatusCode::NOT_FOUND);
	assert_eq!(body, Value::Null);
}

#[tokio::test]
async fn keyed_routes_reject_a_missing_or_wrong_api_key() {
	let fx = Fixture::new();
	let missing = Request::get("/admin/sessions").body(Body::empty()).unwrap();
	let wrong = Request::get("/admin/sessions").header("X-API-Key", "test-kez").body(Body::empty()).unwrap();
	let prefix = Request::get("/admin/sessions").header("X-API-Key", "test").body(Body::empty()).unwrap();

	for req in [missing, wrong, prefix] {
		assert_eq!(fx.send(req).await, (StatusCode::FORBIDDEN, error("forbidden")));
	}
}

#[tokio::test]
async fn rooms_are_public() {
	let fx = Fixture::new();
	let req = Request::get("/rooms/NPWR02973_00").body(Body::empty()).unwrap();

	assert_eq!(fx.send(req).await, (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn rooms_reject_a_com_id_of_the_wrong_length() {
	let fx = Fixture::new();

	assert_eq!(fx.send(get("/rooms/NPWR02973")).await, (StatusCode::BAD_REQUEST, error("invalid_request")));
}

#[tokio::test]
async fn wrong_method_answers_method_not_allowed() {
	let fx = Fixture::new();

	assert_eq!(fx.send(get("/admin/users/ban")).await, (StatusCode::METHOD_NOT_ALLOWED, error("method_not_allowed")));
	assert_eq!(fx.send(post("/admin/sessions", json!({}))).await, (StatusCode::METHOD_NOT_ALLOWED, error("method_not_allowed")));
}

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn sessions_list_every_logged_in_user_sorted_by_online_name() {
	let fx = Fixture::new();
	fx.log_in(1, "zed", "Zed", "203.0.113.9");
	fx.log_in(2, "alice", "Alice", "2001:db8::1");

	let (status, body) = fx.send(get("/admin/sessions")).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		body,
		json!({ "sessions": [
			{ "online_name": "Alice", "npid": "alice", "ip": "2001:db8::1" },
			{ "online_name": "Zed", "npid": "zed", "ip": "203.0.113.9" },
		]})
	);
}

// online_name은 DB에서 중복될 수 있다. 두 계정이 서로 덮어쓰면 ban 대상을 잘못 고르게 된다
#[tokio::test]
async fn sessions_keep_users_who_share_an_online_name_apart() {
	let fx = Fixture::new();
	fx.log_in(1, "impostor", "Dup", "198.51.100.7");
	fx.log_in(2, "victim", "Dup", "192.0.2.1");

	let (_, body) = fx.send(get("/admin/sessions")).await;

	assert_eq!(
		body["sessions"],
		json!([
			{ "online_name": "Dup", "npid": "impostor", "ip": "198.51.100.7" },
			{ "online_name": "Dup", "npid": "victim", "ip": "192.0.2.1" },
		])
	);
}

#[tokio::test]
async fn sessions_drop_a_user_once_they_log_out() {
	let fx = Fixture::new();
	fx.log_in(1, "alice", "Alice", "192.0.2.1");
	fx.log_in(2, "bob", "Bob", "192.0.2.2");

	fx.client_infos.write().remove(&1);
	let (_, body) = fx.send(get("/admin/sessions")).await;

	assert_eq!(body["sessions"], json!([{ "online_name": "Bob", "npid": "bob", "ip": "192.0.2.2" }]));
}

// ---------------------------------------------------------------- verify

#[tokio::test]
async fn verify_returns_the_account_for_the_right_password() {
	let fx = Fixture::new();
	let user_id = fx.add_user("alice", "Alice", false);

	let (status, body) = fx.send(post("/external/users/verify", json!({ "username": "alice", "password": PASSWORD }))).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(
		body,
		json!({
			"user_id": user_id, "username": "alice", "online_name": "Alice",
			"avatar_url": "https://example.com/avatar.png", "admin": false, "banned": false,
		})
	);
}

#[tokio::test]
async fn verify_accepts_id_and_pw_as_field_names() {
	let fx = Fixture::new();
	fx.add_user("alice", "Alice", false);

	let (status, body) = fx.send(post("/external/users/verify", json!({ "id": "alice", "pw": PASSWORD }))).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["username"], "alice");
}

// 없는 계정과 틀린 비밀번호를 같은 응답으로 돌려줘야 계정 존재 여부가 드러나지 않는다
#[tokio::test]
async fn verify_answers_a_wrong_password_and_an_unknown_user_alike() {
	let fx = Fixture::new();
	fx.add_user("alice", "Alice", false);

	let wrong_password = fx.send(post("/external/users/verify", json!({ "username": "alice", "password": "nope" }))).await;
	let unknown_user = fx.send(post("/external/users/verify", json!({ "username": "ghost", "password": PASSWORD }))).await;

	assert_eq!(wrong_password, (StatusCode::UNAUTHORIZED, error("invalid_credentials")));
	assert_eq!(unknown_user, wrong_password);
}

#[tokio::test]
async fn malformed_request_bodies_are_rejected() {
	let fx = Fixture::new();
	let oversized = json!({ "username": "a".repeat(super::MAX_BODY_SIZE), "password": "x" }).to_string();
	let bodies = [
		"{not json".to_owned(),
		json!({ "username": "alice" }).to_string(),
		json!({ "username": "", "password": PASSWORD }).to_string(),
		oversized,
	];

	for body in bodies {
		assert_eq!(fx.send(post_raw("/external/users/verify", body)).await, (StatusCode::BAD_REQUEST, error("invalid_request")));
	}
}

// ---------------------------------------------------------------- admin

#[tokio::test]
async fn admin_info_reports_the_user_and_whether_they_are_online() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);
	let user_id = fx.add_user("alice", "Alice", false);
	fx.log_in(user_id, "alice", "Alice", "192.0.2.1");

	let (status, body) = fx.send(post("/admin/users/info", admin_request("root", "alice"))).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(body["user_id"], user_id);
	assert_eq!(body["online_name"], "Alice");
	assert_eq!(body["online"], true);
	assert_eq!(body["banned"], false);
}

#[tokio::test]
async fn admin_routes_refuse_callers_who_are_not_active_admins() {
	let fx = Fixture::new();
	fx.add_user("alice", "Alice", false);
	let fallen_admin = fx.add_user("fallen", "Fallen", true);
	fx.ban_in_db(fallen_admin);

	for caller in ["alice", "fallen"] {
		assert_eq!(fx.send(post("/admin/users/ban", admin_request(caller, "alice"))).await, (StatusCode::FORBIDDEN, error("forbidden")));
	}
}

#[tokio::test]
async fn admin_routes_reject_a_wrong_admin_password() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);
	fx.add_user("alice", "Alice", false);
	let req = json!({ "admin_username": "root", "admin_password": "nope", "username": "alice" });

	assert_eq!(fx.send(post("/admin/users/info", req)).await, (StatusCode::UNAUTHORIZED, error("invalid_credentials")));
}

#[tokio::test]
async fn admin_routes_report_an_unknown_target_user() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);

	assert_eq!(
		fx.send(post("/admin/users/ban", admin_request("root", "ghost"))).await,
		(StatusCode::NOT_FOUND, error("user_not_found"))
	);
}

#[tokio::test]
async fn ban_records_the_ban_and_kicks_an_online_user() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);
	let user_id = fx.add_user("alice", "Alice", false);
	let kick_notify = fx.log_in(user_id, "alice", "Alice", "192.0.2.1");

	let (status, body) = fx.send(post("/admin/users/ban", admin_request("root", "alice"))).await;

	assert_eq!(status, StatusCode::OK);
	assert_eq!(body, json!({ "user_id": user_id, "username": "alice", "banned": true, "kicked": true }));
	assert!(is_kicked(&kick_notify).await);
	let (_, info) = fx.send(post("/admin/users/info", admin_request("root", "alice"))).await;
	assert_eq!(info["banned"], true);
}

#[tokio::test]
async fn ban_of_an_offline_user_records_the_ban_without_a_kick() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);
	fx.add_user("alice", "Alice", false);

	let (_, body) = fx.send(post("/admin/users/ban", admin_request("root", "alice"))).await;

	assert_eq!(body["banned"], true);
	assert_eq!(body["kicked"], false);
}

// 같은 online_name을 쓰는 다른 계정이 접속해 있어도 npid로 지정한 계정만 끊는다
#[tokio::test]
async fn ban_kicks_only_the_named_account_when_online_names_collide() {
	let fx = Fixture::new();
	fx.add_user("root", "Root", true);
	let impostor = fx.add_user("impostor", "Dup", false);
	let victim = fx.add_user("victim", "Dup", false);
	let impostor_kick = fx.log_in(impostor, "impostor", "Dup", "198.51.100.7");
	let victim_kick = fx.log_in(victim, "victim", "Dup", "192.0.2.1");

	fx.send(post("/admin/users/ban", admin_request("root", "impostor"))).await;

	assert!(is_kicked(&impostor_kick).await);
	assert!(!is_kicked(&victim_kick).await);
}
