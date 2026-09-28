// 포크 전용 HTTP API. upstream stat 서버와 병합 충돌이 나지 않도록 별도 포트·별도 설정으로 띄운다
mod admin;
mod auth;
mod error;
mod external;
mod extract;
mod rooms;
mod sessions;
mod state;
#[cfg(test)]
mod tests;

use std::io;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::server::Server;
use crate::server::client::TerminateWatch;
use error::ApiError;
use state::ApiState;

const MAX_BODY_SIZE: usize = 4096;

impl Server {
	pub async fn start_api_server(&self, term_watch: TerminateWatch) -> io::Result<()> {
		let (bind_addr, api_key);
		{
			let config = self.config.read();
			bind_addr = config.get_api_server_binds().clone();
			api_key = Some(config.get_api_server_api_key()).filter(|key| !key.is_empty()).map(str::to_owned);
		}

		let Some((host, port)) = bind_addr else {
			return Ok(());
		};

		let str_addr = format!("{}:{}", host, port);
		let listener = TcpListener::bind(&str_addr)
			.await
			.map_err(|e| io::Error::new(e.kind(), format!("Api: error binding to <{}>: {}", &str_addr, e)))?;

		info!("API server now waiting for connections on {}", str_addr);

		let state = Arc::new(ApiState {
			db_pool: self.db_pool.clone(),
			client_infos: self.client_infos.clone(),
			room_manager: self.room_manager.clone(),
			api_key,
		});

		tokio::task::spawn(async move {
			if let Err(e) = axum::serve(listener, router(state)).with_graceful_shutdown(terminated(term_watch)).await {
				warn!("Api: server error: {}", e);
			}
			info!("ApiServer terminating");
		});

		Ok(())
	}
}

fn router(state: Arc<ApiState>) -> Router {
	let keyed = Router::new()
		.route("/admin/sessions", get(sessions::list))
		.route("/external/users/verify", post(external::verify))
		.route("/admin/users/info", post(admin::user_info))
		.route("/admin/users/ban", post(admin::ban))
		.route_layer(middleware::from_fn_with_state(state.clone(), auth::require_api_key));

	Router::new()
		.route("/rooms/{com_id}", get(rooms::list))
		.merge(keyed)
		.method_not_allowed_fallback(|| async { ApiError::MethodNotAllowed })
		.layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
		.with_state(state)
}

async fn terminated(mut term_watch: TerminateWatch) {
	let _ = term_watch.recv.wait_for(|terminated| *terminated).await;
}
