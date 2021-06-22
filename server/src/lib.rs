use macroquad::prelude::*;
use macroquad_tiled as tiled;

use ::rand::seq::SliceRandom;
use nanoserde::DeBin;
use std::cell::Cell;
use std::sync::{Arc, RwLock};
use std::time::Duration;

struct Player {
    port: u16,
    x: u16,
    y: u8,
}

#[derive(Default)]
struct Lobby {
    players: Vec<Option<Player>>,
    started: bool,
}

impl Lobby {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Default)]
struct ClientState {
    index: usize,
    started: Cell<bool>,
    lobby: Arc<RwLock<Lobby>>,
}

#[derive(Default)]
struct CurrentLobby {
    lobby: Arc<RwLock<Lobby>>,
}

impl CurrentLobby {
    pub fn new() -> Self {
        Self::default()
    }
}

pub async fn lobby_main() {
    let current_lobby = Arc::new(RwLock::new(CurrentLobby::new()));

    let tileset = load_texture("client/assets/tileset.png").await.unwrap();
    tileset.set_filter(FilterMode::Nearest);
    let tiled_map_json = load_string("client/assets/map.json").await.unwrap();
    let tiled_map = tiled::load_map(&tiled_map_json, &[("tileset.png", tileset)], &[]).unwrap();
    let spawners = &tiled_map.layers["logic"].objects;
    let spawner_positions: RwLock<Vec<Vec2>> = RwLock::new(
        spawners
            .iter()
            .map(|spawner| vec2(spawner.world_x, spawner.world_y))
            .collect(),
    );

    {
        let current_lobby = current_lobby.clone();
        std::thread::spawn(move || {
            quad_net::quad_socket::server::listen(
                "0.0.0.0:8090",
                "0.0.0.0:8091",
                quad_net::quad_socket::server::Settings {
                    on_message: {
                        let current_lobby = current_lobby.clone();
                        move |mut _out, state: &mut ClientState, msg| {
                            let shared::Join(port) = DeBin::deserialize_bin(&msg).unwrap();
                            let spawner_positions = spawner_positions.read().unwrap();
                            let spawn_position =
                                spawner_positions.choose(&mut ::rand::thread_rng()).unwrap();
                            let player = Player {
                                port,
                                x: spawn_position.x as u16,
                                y: spawn_position.y as u8,
                            };
                            let lobby = &current_lobby.read().unwrap().lobby;
                            let mut lobby_write = lobby.write().unwrap();
                            state.index = lobby_write.players.len();
                            state.lobby = lobby.clone();
                            lobby_write.players.push(Some(player));
                            info!(
                                "Player {} joined the lobby (from port: {})",
                                state.index, port
                            );
                        }
                    },
                    on_timer: {
                        move |out, state| {
                            let lobby_read = state.lobby.read().unwrap();
                            if lobby_read.started {
                                info!("Player {} starting", state.index);
                                let players = lobby_read
                                    .players
                                    .iter()
                                    .filter_map(|possible_player| {
                                        possible_player
                                            .as_ref()
                                            .map(|player| (player.port, (player.x, player.y)))
                                    })
                                    .collect();
                                out.send_bin(&shared::Start(players)).unwrap();
                                state.started.set(true);
                                out.disconnect();
                            }
                        }
                    },
                    on_disconnect: {
                        move |state| {
                            if !state.started.get() {
                                info!("Player {} left the lobby", state.index);
                                state.lobby.write().unwrap().players[state.index] = None;
                            }
                        }
                    },
                    timer: Some(Duration::from_millis(1000 / 30)),
                    _marker: std::marker::PhantomData,
                },
            );
        });
    }

    loop {
        if is_key_pressed(KeyCode::Enter) {
            info!("Starting game...");
            let lobby = &mut current_lobby.write().unwrap().lobby;
            {
                let mut lobby_write = lobby.write().unwrap();
                lobby_write.started = true;
            }
            *lobby = Arc::new(RwLock::new(Lobby::new()));
        }
        next_frame().await;
    }
}
