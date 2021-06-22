use macroquad::prelude::*;

use macroquad_particles as particles;
//use macroquad_profiler as profiler;
use macroquad_tiled as tiled;

use backroll::{
    command::{Command, Commands},
    Event, P2PSession, Player as BackrollPlayer, PlayerHandle as BackrollPlayerHandle,
};
use backroll_transport_udp::{UdpConnectionConfig, UdpManager};
use bevy_tasks::TaskPool;
use bytemuck::{Pod, Zeroable};
use macroquad::telemetry;
use macroquad_platformer::*;
use ordered_float::OrderedFloat;
use particles::Emitter;
use quad_net::quad_socket::client::QuadSocket;
use std::net::{Ipv4Addr, SocketAddr};

mod consts {
    use super::{Input, KeyCode};
    pub const GRAVITY: f32 = 900.0;
    pub const JUMP_SPEED: f32 = 250.0;
    pub const RUN_SPEED: f32 = 150.0;
    pub const PLAYER_SPRITE: u32 = 120;
    pub const INPUT_MAP: [(Input, KeyCode); 4] = [
        (Input::SHOOT, KeyCode::A),
        (Input::LEFT, KeyCode::Left),
        (Input::RIGHT, KeyCode::Right),
        (Input::JUMP, KeyCode::Space),
    ];
}

#[macro_use]
extern crate bitflags;

struct Player {
    backroll_player_handle: BackrollPlayerHandle,
    collider: Actor,
    speed: Vec2,
    prev_jump_down: bool,
    facing_right: bool,
    shooting: bool,
}

bitflags! {
    #[repr(C)]
    #[derive(Zeroable, Pod)]
    pub struct Input: u8 {
        const SHOOT = 0b1;
        const LEFT = 0b10;
        const RIGHT = 0b100;
        const JUMP = 0b1000;
    }
}

impl Input {
    pub fn current() -> Input {
        let mut current_inputs = Input::empty();
        for (input, key_code) in &consts::INPUT_MAP {
            if is_key_down(*key_code) {
                current_inputs.insert(*input);
            }
        }
        current_inputs
    }
}

#[derive(Clone, Hash)]
struct PlayerState {
    x: OrderedFloat<f32>,
    y: OrderedFloat<f32>,
    vx: OrderedFloat<f32>,
    vy: OrderedFloat<f32>,
    prev_jump_down: bool,
    facing_right: bool,
    shooting: bool,
}

#[derive(Clone, Hash)]
struct State {
    pub players: Vec<PlayerState>,
}

struct BackrollConfig;

impl backroll::Config for BackrollConfig {
    type Input = Input;
    type State = State;
}

const SHOOTING_FX: &str = r#"
{"local_coords":false,"emission_shape":{"Sphere":{"radius":1}},"one_shot":false,"lifetime":0.4,"lifetime_randomness":0.2,"explosiveness":0,"amount":27,"shape":{"Circle":{"subdivisions":20}},"emitting":true,"initial_direction":{"x":1,"y":0},"initial_direction_spread":0.1,"initial_velocity":337.3,"initial_velocity_randomness":0.3,"linear_accel":0,"size":3.3,"size_randomness":0,"size_curve":{"points":[[0,0.44000006],[0.22,0.72],[0.46,0.84296143],[0.7,1.1229614],[1,0]],"interpolation":{"Linear":[]},"resolution":30},"blend_mode":{"Additive":[]},"colors_curve":{"start":{"r":0.89240015,"g":0.97,"b":0,"a":1},"mid":{"r":1,"g":0.11639989,"b":0.059999943,"a":1},"end":{"r":0.1500001,"g":0.03149999,"b":0,"a":1}},"gravity":{"x":0,"y":0},"post_processing":{}}
"#;

struct Game {
    session: P2PSession<BackrollConfig>,
    local_player: BackrollPlayerHandle,
    world: World,
    camera: Camera2D,
    tiled_map: tiled::Map,
    bullet_emitter: Emitter,
    players: Vec<Player>,
}

impl Game {
    async fn new() -> Self {
        async fn connect(
            server_addr: SocketAddr,
            world: &mut World,
        ) -> (
            P2PSession<BackrollConfig>,
            BackrollPlayerHandle,
            Vec<Player>,
        ) {
            let task_pool = TaskPool::new();

            let local_port = portpicker::pick_unused_port()
                .expect("Ran out of available ports to make connections with");
            let local_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), local_port);
            info!("Local addr: {:?}", local_addr);
            let connection_manager = UdpManager::bind(task_pool.clone(), local_addr).unwrap();

            let mut builder = P2PSession::build();

            info!("Connecting to lobby...");
            let mut socket = QuadSocket::connect(server_addr).unwrap();
            socket.send_bin(&shared::Join(local_port));

            loop {
                if let Some(data) = socket.try_recv() {
                    let shared::Start(players_data) =
                        nanoserde::DeBin::deserialize_bin(&data).unwrap();
                    info!("Starting...");
                    let mut local_player = None;
                    let mut players = Vec::new();
                    for (port, (x, y)) in players_data {
                        let backroll_player_handle = if port == local_port {
                            info!("Adding local player");
                            let backroll_player_handle = builder.add_player(BackrollPlayer::Local);
                            local_player = Some(backroll_player_handle);
                            backroll_player_handle
                        } else {
                            let remote_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port);
                            info!("Adding remote player with addr {:?}", remote_addr);
                            let remote_peer = connection_manager
                                .connect(UdpConnectionConfig::unbounded(remote_addr));
                            let backroll_player_handle =
                                builder.add_player(BackrollPlayer::Remote(remote_peer));
                            backroll_player_handle
                        };
                        players.push(Player {
                            backroll_player_handle,
                            collider: world.add_actor(vec2(x as f32, y as f32), 8, 8),
                            speed: vec2(0., 0.),
                            prev_jump_down: false,
                            facing_right: true,
                            shooting: false,
                        })
                    }
                    let session = builder.start(task_pool).unwrap();
                    break (session, local_player.unwrap(), players);
                }
                next_frame().await;
            }
        }
        let mut bullet_emitter =
            Emitter::new(nanoserde::DeJson::deserialize_json(SHOOTING_FX).unwrap());
        bullet_emitter.config.emitting = false;

        let tileset = load_texture("client/assets/tileset.png").await.unwrap();
        tileset.set_filter(FilterMode::Nearest);

        let tiled_map_json = load_string("client/assets/map.json").await.unwrap();
        let tiled_map = tiled::load_map(&tiled_map_json, &[("tileset.png", tileset)], &[]).unwrap();

        let mut static_colliders = vec![];
        for (_x, _y, tile) in tiled_map.tiles("main layer", None) {
            static_colliders.push(tile.is_some());
        }

        let mut world = World::new();
        world.add_static_tiled_layer(static_colliders, 8., 8., 40, 1);

        let camera = Camera2D::from_display_rect(Rect::new(0.0, 0.0, 320.0, 152.0));

        let (session, local_player, players) =
            connect("0.0.0.0:8090".parse().unwrap(), &mut world).await;

        Self {
            session,
            local_player,
            bullet_emitter,
            tiled_map,
            world,
            camera,
            players,
        }
    }

    async fn update(&mut self) {
        telemetry::begin_zone("Main loop");

        telemetry::begin_zone("pre flush");
        self.run_commands(self.session.poll());
        telemetry::end_zone();

        if self.session.is_synchronized() {
            telemetry::begin_zone("local input");
            self.session
                .add_local_input(self.local_player, Input::current())
                .expect("Inputs should be added at the right time");
            telemetry::end_zone();

            telemetry::begin_zone("advance frame");
            self.run_commands(self.session.advance_frame());
            telemetry::end_zone();

            self.draw();
        }

        //profiler::profiler(profiler::ProfilerParams {
        //    fps_counter_pos: vec2(50.0, 20.0),
        //});

        telemetry::end_zone();

        next_frame().await;
    }

    fn run_commands(&mut self, commands: Commands<BackrollConfig>) {
        for command in commands {
            match command {
                Command::Save(save_state) => {
                    save_state.save(State {
                        players: self
                            .players
                            .iter()
                            .map(|player| {
                                let pos = self.world.actor_pos(player.collider);
                                PlayerState {
                                    x: OrderedFloat(pos.x),
                                    y: OrderedFloat(pos.y),
                                    vx: OrderedFloat(player.speed.x),
                                    vy: OrderedFloat(player.speed.y),
                                    prev_jump_down: player.prev_jump_down,
                                    facing_right: player.facing_right,
                                    shooting: player.shooting,
                                }
                            })
                            .collect(),
                    });
                }
                Command::Load(load_state) => {
                    for (player_state, player) in load_state
                        .load()
                        .players
                        .iter()
                        .zip(self.players.iter_mut())
                    {
                        *player = Player {
                            backroll_player_handle: player.backroll_player_handle,
                            collider: player.collider,
                            speed: Vec2::new(player_state.vx.0, player_state.vy.0),
                            prev_jump_down: player_state.prev_jump_down,
                            facing_right: player_state.facing_right,
                            shooting: player_state.shooting,
                        };
                        self.world.set_actor_position(
                            player.collider,
                            Vec2::new(player_state.x.0, player_state.y.0),
                        );
                    }
                }
                Command::AdvanceFrame(input) => {
                    for player in &mut self.players {
                        let pos = self.world.actor_pos(player.collider);
                        let on_ground = self
                            .world
                            .collide_check(player.collider, pos + vec2(0., 1.));
                        {
                            let player_input = input
                                .get(player.backroll_player_handle)
                                .expect("Player should still be valid"); // TODO: Is player valid even after disconnect?

                            if player_input.contains(Input::RIGHT) {
                                player.speed.x = consts::RUN_SPEED;
                            } else if player_input.contains(Input::LEFT) {
                                player.speed.x = -consts::RUN_SPEED;
                            } else {
                                player.speed.x = 0.;
                            }
                            if player_input.contains(Input::JUMP) && !player.prev_jump_down {
                                if on_ground {
                                    player.speed.y = -consts::JUMP_SPEED;
                                }
                            }
                            player.prev_jump_down = player_input.contains(Input::JUMP);
                        }

                        if player.speed.x < 0.0 {
                            player.facing_right = false;
                        }
                        if player.speed.x > 0.0 {
                            player.facing_right = true;
                        }

                        if on_ground == false {
                            player.speed.y += consts::GRAVITY * get_frame_time();
                        }

                        self.world
                            .move_h(player.collider, player.speed.x * get_frame_time());
                        if !self
                            .world
                            .move_v(player.collider, player.speed.y * get_frame_time())
                        {
                            player.speed.y = 0.0;
                        }
                    }
                }
                Command::Event(Event::Connected(player_handle)) => {
                    info!("Remote player connected: {:?}", player_handle);
                }
                Command::Event(Event::Synchronizing {
                    player,
                    count,
                    total,
                }) => {
                    debug!(
                        "Remote player sync: {:?}, count: {}, total: {}",
                        player, count, total
                    );
                }
                Command::Event(Event::Synchronized(player)) => {
                    info!("Remote player synced: {:?}", player);
                }
                Command::Event(Event::Running) => {
                    info!("P2PSession is all synchronized and is ready to run");
                }
                Command::Event(Event::Disconnected(player)) => {
                    info!("Remote player disconnected: {:?}", player);
                }
                Command::Event(Event::TimeSync { frames_ahead }) => {
                    debug!("Received stall request: {}", frames_ahead);
                }
                Command::Event(Event::ConnectionInterrupted {
                    player,
                    disconnect_timeout,
                }) => {
                    info!(
                        "Remote player interrupted: {:?}, timeout: {:?}",
                        player, disconnect_timeout
                    );
                }
                Command::Event(Event::ConnectionResumed(player)) => {
                    info!("Remote player resumed: {:?}", player);
                }
            }
        }
    }

    fn draw(&mut self) {
        telemetry::begin_zone("draw world");
        clear_background(BLACK);

        set_camera(&self.camera);

        for _ in 0..1 {
            self.tiled_map
                .draw_tiles("main layer", Rect::new(0.0, 0.0, 320.0, 152.0), None);
        }

        for player in &self.players {
            let pos = self.world.actor_pos(player.collider);

            if player.backroll_player_handle.0 != self.local_player.0 {
                draw_text_ex(
                    &format!("player {}", player.backroll_player_handle.0),
                    pos.x - 4.0,
                    pos.y - 6.0,
                    TextParams {
                        font_size: 30,
                        font_scale: 0.15,
                        ..Default::default()
                    },
                );
            }

            if player.facing_right {
                self.tiled_map.spr(
                    "tileset",
                    consts::PLAYER_SPRITE,
                    Rect::new(pos.x, pos.y, 8.0, 8.0),
                );
            } else {
                self.tiled_map.spr(
                    "tileset",
                    consts::PLAYER_SPRITE,
                    Rect::new(pos.x + 8.0, pos.y, -8.0, 8.0),
                );
            }

            if player.shooting {
                if player.facing_right {
                    self.bullet_emitter.config.initial_direction = vec2(1.0, 0.0);
                } else {
                    self.bullet_emitter.config.initial_direction = vec2(-1.0, 0.0);
                }
                self.bullet_emitter.emit(
                    vec2(pos.x, pos.y) + vec2(8.0 * player.facing_right as u8 as f32, 4.0),
                    1,
                );
            }
        }

        telemetry::end_zone();

        telemetry::begin_zone("draw particles");
        self.bullet_emitter.draw(vec2(0., 0.));
        telemetry::end_zone();

        set_default_camera();
    }
}

#[macroquad::main("Platformer")]
async fn main() {
    let mut game = Game::new().await;

    loop {
        game.update().await;
    }
}
