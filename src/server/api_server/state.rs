use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::server::client::ClientSharedInfo;
use crate::server::room_manager::RoomManager;

pub struct ApiState {
	pub db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	pub client_infos: Arc<RwLock<HashMap<i64, ClientSharedInfo>>>,
	pub room_manager: Arc<RwLock<RoomManager>>,
	// None이면 키가 필요한 라우트는 모두 404
	pub api_key: Option<String>,
}
