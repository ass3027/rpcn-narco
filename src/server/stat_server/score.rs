use std::fmt::Write;

use crate::server::client::ComId;
use crate::server::database::db_score::DbBoardInfo;
use crate::server::score_cache::{GetScoreResultCache, ScoresCache};
use hyper::{Response, StatusCode};

use super::StatContext;
use super::response::{empty_response, json_response, sanitize_for_json};

pub(super) fn handle_com_id_score_req(ctx: &StatContext, com_id: &ComId) -> Response<String> {
	ctx.json_cache
		.com_id_score(ctx.cache_life, com_id, || json_response(StatusCode::OK, com_id_score_to_json(&ctx.score_cache, com_id)))
}

pub(super) fn handle_table_score_req(ctx: &StatContext, com_id: &ComId, table_id: u32) -> Response<String> {
	let Some(table) = ctx.score_cache.get_table(com_id, table_id) else {
		return empty_response();
	};
	let table_infos = table.read().table_info.clone();

	ctx.json_cache.table_score(ctx.cache_life, com_id, table_id, || {
		json_response(StatusCode::OK, table_score_to_json(&ctx.score_cache, com_id, table_id, &table_infos))
	})
}

fn com_id_score_to_json(score_cache: &ScoresCache, com_id: &ComId) -> String {
	let mut tables = score_cache.get_all_tables(com_id);
	if tables.is_empty() {
		return "[]".to_owned();
	}

	tables.sort_by_key(|(board_id, _)| *board_id);

	let mut res = String::from("[\n");
	for (index, (board_id, table)) in tables.iter().enumerate() {
		let table_infos = table.read().table_info.clone();
		res += &table_score_to_json(score_cache, com_id, *board_id, &table_infos);
		res += if index != tables.len() - 1 { ",\n" } else { "\n" };
	}
	res += "]";
	res
}

fn table_score_to_json(score_cache: &ScoresCache, com_id: &ComId, board_id: u32, table_infos: &DbBoardInfo) -> String {
	let result = score_cache.get_score_range(com_id, board_id, 1, table_infos.rank_limit, true, true);
	score_result_to_json(&result, board_id, table_infos)
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
		res += if index != result.scores.len() - 1 { ",\n" } else { "\n" };
	}

	res += "    ]\n}";
	res
}
