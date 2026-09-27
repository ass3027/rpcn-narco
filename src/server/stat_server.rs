mod auth;
mod cache;
mod external;
mod response;
mod rooms;
mod router;
mod score;
mod usage;

use std::io;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use crate::server::GameTracker;
use crate::server::Server;
use crate::server::client::TerminateWatch;
use crate::server::room_manager::RoomManager;
use crate::server::score_cache::ScoresCache;
use cache::JsonCache;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use parking_lot::RwLock;
use tokio::net::TcpListener;
use tracing::{info, warn};

// State shared by every stat server request.
struct StatContext {
	path: String,
	cache_life: u32,
	game_tracker: Arc<GameTracker>,
	score_cache: Arc<ScoresCache>,
	json_cache: JsonCache,
	room_manager: Arc<RwLock<RoomManager>>,
	db_pool: r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
	external_user_api_key: Option<String>,
}

pub struct StatServer {
	listener: TcpListener,
	term_watch: TerminateWatch,
	ctx: Arc<StatContext>,
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

			let ctx = StatContext {
				path,
				cache_life,
				game_tracker,
				score_cache: self.score_cache.clone(),
				json_cache: JsonCache::new(),
				room_manager,
				db_pool,
				external_user_api_key,
			};
			let mut stat_server = StatServer {
				listener,
				term_watch,
				ctx: Arc::new(ctx),
			};

			tokio::task::spawn(async move {
				stat_server.server_proc().await;
			});
		}

		Ok(())
	}
}

impl StatServer {
	async fn server_proc(&mut self) {
		if *self.term_watch.recv.borrow_and_update() {
			return;
		}

		'stat_server_loop: loop {
			tokio::select! {
				accept_res = self.listener.accept() => {
					let (stream, peer_addr) = match accept_res {
						Ok(accepted) => accepted,
						Err(e) => {
							warn!("Stat: Error accepting a client: {}", e);
							continue 'stat_server_loop;
						}
					};

					info!("Stat: new client from {}", peer_addr);
					let io = TokioIo::new(stream);
					let ctx = self.ctx.clone();

					tokio::task::spawn(async move {
						if let Err(err) = http1::Builder::new().keep_alive(false).serve_connection(io, service_fn(|req| router::route(req, ctx.clone()))).await {
							warn!("Stat: Error serving connection: {}", err);
						}
					});
				}
				_ = self.term_watch.recv.changed() => {
					break 'stat_server_loop;
				}
			}
		}
		info!("StatServer::server_proc terminating");
	}
}
