use std::fmt::Write;

use crate::server::client::ComId;
use crate::server::room_manager::RoomManager;
use hyper::{Response, StatusCode};

use super::StatContext;
use super::response::{json_response, sanitize_for_json};

// Rooms are live state, so this route is never cached.
pub(super) fn handle_rooms_req(ctx: &StatContext, com_id: &ComId) -> Response<String> {
	json_response(StatusCode::OK, rooms_to_json(&ctx.room_manager.read(), com_id))
}

fn rooms_to_json(rm: &RoomManager, com_id: &ComId) -> String {
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
