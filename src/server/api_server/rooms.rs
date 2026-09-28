use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use super::error::ApiError;
use super::state::ApiState;
use crate::server::client::ComId;
use crate::server::room_manager::Room;

#[derive(Serialize)]
pub struct RoomJson {
	room_id: u64,
	world_id: u32,
	lobby_id: u64,
	max_slot: u16,
	cur_member_num: usize,
	flag_attr: u32,
	has_password: bool,
	members: Vec<MemberJson>,
}

#[derive(Serialize)]
struct MemberJson {
	npid: String,
	online_name: String,
	is_owner: bool,
}

// 방은 실시간 상태라 캐시하지 않는다
pub async fn list(State(state): State<Arc<ApiState>>, Path(com_id): Path<String>) -> Result<Json<Vec<RoomJson>>, ApiError> {
	let com_id: ComId = com_id.as_bytes().try_into().map_err(|_| ApiError::InvalidRequest)?;

	let rooms = state
		.room_manager
		.read()
		.get_rooms()
		.iter()
		.filter(|((c_id, _), _)| *c_id == com_id)
		.map(|((_, room_id), room)| room_json(*room_id, room))
		.collect();

	Ok(Json(rooms))
}

fn room_json(room_id: u64, room: &Room) -> RoomJson {
	RoomJson {
		room_id,
		world_id: room.world_id,
		lobby_id: room.lobby_id,
		max_slot: room.max_slot,
		cur_member_num: room.users.len(),
		flag_attr: room.flag_attr,
		has_password: room.room_password.is_some(),
		members: room
			.users
			.values()
			.map(|user| MemberJson {
				npid: user.npid.clone(),
				online_name: user.online_name.clone(),
				is_owner: user.flag_attr & 0x80000000 != 0,
			})
			.collect(),
	}
}
