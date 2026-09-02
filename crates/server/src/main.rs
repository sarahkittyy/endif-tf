//! endif matchmaking / account server.
//!
//! One process serves the WebRTC signaling websocket (matchbox full-mesh topology with room limits
//! and an idle timeout, `rooms.rs`: clients connect to `ws://host:port/endif-<room-code>` and are
//! introduced to the other peer in that room; all game traffic then flows peer-to-peer; a socket
//! to `/presence` joins nothing and only counts the client as online), the `/api` account +
//! matchmaking HTTP API (`api.rs`) and the `/health` and `/version` probes.
//! Configuration comes from the environment (`.env`, see `config.rs`), accounts and matches live
//! in MariaDB (`migrations/`, run on startup).

mod api;
mod auth;
mod config;
mod elo;
mod leaderboard;
mod limits;
mod mail;
mod matches;
mod queue;
mod rooms;

use axum::response::IntoResponse;
use config::Config;
use matchbox_signaling::SignalingServerBuilder;
use matchbox_signaling::topologies::full_mesh::FullMeshState;
use rooms::{EndifMesh, EndifState, Rooms};
use sqlx::mysql::MySqlPoolOptions;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `.env` in the working directory; real environment variables win over it.
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("loaded {}", path.display()),
        Err(e) if e.not_found() => eprintln!("no .env file found; using the environment only"),
        Err(e) => eprintln!("could not read .env: {e}"),
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")))
        .init();

    let cfg = Config::from_env().map_err(|e| format!("configuration: {e}"))?;
    let addr = cfg.bind;
    let max_room_size = cfg.max_room_size;

    info!("connecting to the database");
    let db = MySqlPoolOptions::new()
        .max_connections(8)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&cfg.database_url)
        .await
        .map_err(|e| format!("database ({}): {e}", redact(&cfg.database_url)))?;
    sqlx::migrate!("./migrations").run(&db).await.map_err(|e| format!("migrations: {e}"))?;
    info!("database migrated");

    let rooms = Arc::new(Mutex::new(Rooms::new(max_room_size)));
    let state: api::AppState = Arc::new(api::Inner {
        db: db.clone(),
        jwt: auth::Jwt::new(&cfg.jwt_secret, cfg.jwt_days),
        mailer: mail::Mailer::new(&cfg.mail).map_err(|e| format!("mail: {e}"))?,
        queue: Mutex::new(queue::Queue::default()),
        quick: Mutex::new(queue::Queue::default()),
        limits: limits::Limiter::new(&cfg.limits),
        rooms: rooms.clone(),
    });
    if !cfg.limits.enabled {
        warn!("ENDIF_RATE_LIMITS=false: the API and the signaling handshake are not rate limited");
    }

    // Settles matches whose second report never arrived, voids abandoned ones, drops idle
    // rate-limit buckets and releases room slots whose socket never opened.
    let sweeper = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            if let Err(e) = matches::sweep(&sweeper.db).await {
                error!("match sweep failed: {e:?}");
            }
            sweeper.limits.sweep();
            sweeper.rooms.lock().unwrap().sweep();
        }
    });

    // Drops registrations that were never verified: one DELETE an hour, nothing in between.
    let cleaner = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
        loop {
            tick.tick().await;
            match api::sweep_unverified(&cleaner.db).await {
                Ok(0) => {}
                Ok(n) => info!(n, "dropped unverified accounts older than 30 days"),
                Err(e) => error!("unverified account sweep failed: {e:?}"),
            }
        }
    });

    let (r_request, r_assign) = (rooms.clone(), rooms.clone());
    let protocol = endif_sim::protocol_id();
    let protocol_for_route = protocol.clone();
    let ws_limits = state.clone();

    let server = SignalingServerBuilder::new(addr, EndifMesh, EndifState { mesh: FullMeshState::default(), rooms })
        .on_connection_request(move |meta| {
            let room = meta.path.clone().unwrap_or_default().trim_matches('/').to_string();
            // Handshakes are cheap for us but each one is a task and a socket; a client only needs
            // one per match.
            let who = ws_limits.limits.client_key(Some(meta.origin), &meta.headers);
            ws_limits.limits.check(&format!("ws:{who}"), limits::WS, &format!("{who} ws {room}"), "too many connections from your address; try again in {wait}")?;
            // Clients send the protocol identity of their build as `?v=`; a different simulation
            // or netcode cannot play against this server's generation of clients. 426 tells them
            // apart from a full room (401) and from an unreachable server.
            let client_protocol = meta.query_params.get("v").cloned().unwrap_or_default();
            if client_protocol != protocol {
                warn!(origin = %meta.origin, %room, %client_protocol, "refused: protocol mismatch (server {protocol})");
                return Err((axum::http::StatusCode::UPGRADE_REQUIRED, format!("client protocol {client_protocol} != server {protocol}")).into_response());
            }
            // A presence socket joins no room: it is counted, not seated (`rooms.rs`).
            if room == rooms::PRESENCE_PATH {
                r_request.lock().unwrap().admit_presence(meta.origin);
                return Ok(true);
            }
            // Reserve the slot now, before the upgrade, so two simultaneous joins cannot both get
            // in; `rooms.rs` confirms it once the socket is open and sweeps it if that never happens.
            match r_request.lock().unwrap().admit(meta.origin, &room) {
                Ok(taken) => {
                    info!(origin = %meta.origin, %room, "connection request ({taken}/{max_room_size})");
                    Ok(true)
                }
                Err(taken) => {
                    warn!(origin = %meta.origin, %room, "refused: room is full ({taken}/{max_room_size})");
                    Ok(false)
                }
            }
        })
        .on_id_assignment(move |(socket, id)| match r_assign.lock().unwrap().assign(socket, &id.to_string()) {
            Some(room) => info!(%socket, %id, %room, "assigned peer id"),
            None => warn!(%socket, %id, "assigned peer id for a socket that was never admitted"),
        })
        .cors()
        .mutate_router(move |router| {
            let protocol = protocol_for_route.clone();
            router
                .route("/health", axum::routing::get(|| async { "ok" }))
                // The web client fetches this from the page's origin, which is not the signaling
                // server's; the builder's CORS layer only wraps the routes that existed before
                // `mutate_router`, so the header is set here.
                .route(
                    "/version",
                    axum::routing::get(move || async move { ([(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], protocol.clone()) }),
                )
                .nest("/api", api::router(state))
        })
        .build();

    info!("endif server listening on ws://{addr}/<room-code> and http://{addr}/api  (max room size {max_room_size}, protocol {})", endif_sim::protocol_id());
    if let Err(e) = server.serve().await {
        warn!("server error: {e}");
    }
    Ok(())
}

/// The database URL without its password, for log lines.
fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(s), Some(at)) if at > s => {
            let creds = &url[s + 3..at];
            let user = creds.split(':').next().unwrap_or_default();
            format!("{}{}:***{}", &url[..s + 3], user, &url[at..])
        }
        _ => url.to_string(),
    }
}
