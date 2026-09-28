use std::net::IpAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use super::state::ApiState;

#[derive(Serialize)]
pub struct Session {
	online_name: String,
	npid: String,
	ip: IpAddr,
}

#[derive(Serialize)]
pub struct SessionsResponse {
	sessions: Vec<Session>,
}

// 게임에서 보이는 online_name으로 찾고, 같은 이름이 여럿이면 npid로 구분해 ban한다
pub async fn list(State(state): State<Arc<ApiState>>) -> Json<SessionsResponse> {
	let mut sessions: Vec<Session> = state
		.client_infos
		.read()
		.values()
		.map(|info| Session {
			online_name: info.online_name.clone(),
			npid: info.npid.clone(),
			ip: info.ip,
		})
		.collect();
	sessions.sort_by(|a, b| (&a.online_name, &a.npid).cmp(&(&b.online_name, &b.npid)));

	Json(SessionsResponse { sessions })
}
