use std::fmt::Write;
use std::sync::atomic::Ordering;

use crate::server::GameTracker;
use crate::server::client::com_id_to_string;
use hyper::{Response, StatusCode};

use super::StatContext;
use super::response::{json_response, sanitize_for_json};

pub(super) fn handle_usage_req(ctx: &StatContext) -> Response<String> {
	ctx.json_cache.usage(ctx.cache_life, || json_response(StatusCode::OK, game_tracker_to_json(&ctx.game_tracker)))
}

fn game_tracker_to_json(game_tracker: &GameTracker) -> String {
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
				let _ = write!(res, "            \"{}\": \"{}\"{}", sanitize_for_json(name), ip, comma);
				res += "\n";
			}
			res += if index != entries.len() - 1 { "        },\n" } else { "        }\n" };
		}
		res += "    }";
	}

	res += "\n}";

	res
}
