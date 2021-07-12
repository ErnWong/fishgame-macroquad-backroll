use macroquad::prelude::*;

use macroquad_particles as particles;
//use macroquad_profiler as profiler;
use macroquad_tiled as tiled;

use backroll::{
    command::{Command, Commands},
    BackrollError, Event, P2PSession, Player as BackrollPlayer,
    PlayerHandle as BackrollPlayerHandle,
};
use backroll_transport_udp::{UdpConnectionConfig, UdpManager};
use bevy_tasks::TaskPool;
use bytemuck::{Pod, Zeroable};
use macroquad::telemetry;
use macroquad_platformer::{Actor, World as CollisionWorld};
use ordered_float::OrderedFloat;
use particles::EmittersCache;
use quad_net::quad_socket::client::QuadSocket;
use std::net::{Ipv4Addr, SocketAddr};

mod consts {
    use super::{Input, KeyCode};
    pub const TIMESTEP: f32 = 1.0 / 60.0;
    pub const GRAVITY: f32 = 900.0;
    pub const JUMP_SPEED: f32 = 250.0;
    pub const RUN_SPEED: f32 = 150.0;
    pub const PLAYER_SPRITE: u32 = 120;
    pub const BULLET_SPEED: f32 = 300.0;
    pub const BULLET_INTERVAL_TICKS: u32 = 10;
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
    health: i32,
    gun_clock: u32,
}

struct Bullet {
    pos: Vec2,
    speed: Vec2,
    lived: f32,
    lifetime: f32,
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
    health: i32,
    gun_clock: u32,
}

#[derive(Clone, Hash)]
struct BulletState {
    x: OrderedFloat<f32>,
    y: OrderedFloat<f32>,
    vx: OrderedFloat<f32>,
    vy: OrderedFloat<f32>,
    lived: OrderedFloat<f32>,
    lifetime: OrderedFloat<f32>,
}

#[derive(Clone, Hash)]
struct State {
    pub players: Vec<PlayerState>,
    pub bullets: Vec<BulletState>,
}

struct BackrollConfig;

impl backroll::Config for BackrollConfig {
    type Input = Input;
    type State = State;
}

pub const EXPLOSION_FX: &'static str = r#"{"local_coords":false,"emission_shape":{"Point":[]},"one_shot":true,"lifetime":0.15,"lifetime_randomness":0,"explosiveness":0.65,"amount":41,"shape":{"Circle":{"subdivisions":10}},"emitting":false,"initial_direction":{"x":0,"y":-1},"initial_direction_spread":6.2831855,"initial_velocity":30,"initial_velocity_randomness":0.2,"linear_accel":0,"size":1.5000002,"size_randomness":0.4,"blend_mode":{"Alpha":[]},"colors_curve":{"start":{"r":0.8200004,"g":1,"b":0.31818175,"a":1},"mid":{"r":0.71000004,"g":0.36210018,"b":0,"a":1},"end":{"r":0.02,"g":0,"b":0.000000007152557,"a":1}},"gravity":{"x":0,"y":0},"post_processing":{}}
"#;

struct Game {
    _connection_manager: UdpManager,
    session: P2PSession<BackrollConfig>,
    local_player: BackrollPlayerHandle,
    explosions: EmittersCache,
    collision_world: CollisionWorld,
    camera: Camera2D,
    tiled_map: tiled::Map,
    players: Vec<Player>,
    bullets: Vec<Bullet>,
    frames_to_stall: u8,
}

impl Game {
    async fn new() -> Self {
        async fn connect(
            server_addr: SocketAddr,
            collision_world: &mut CollisionWorld,
        ) -> (
            P2PSession<BackrollConfig>,
            BackrollPlayerHandle,
            Vec<Player>,
            UdpManager,
        ) {
            let task_pool = TaskPool::new();

            let local_port = portpicker::pick_unused_port()
                .expect("Ran out of available ports to make connections with");
            let local_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local_port);
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
                            let remote_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
                            info!("Adding remote player with addr {:?}", remote_addr);
                            let remote_peer = connection_manager
                                .connect(UdpConnectionConfig::unbounded(remote_addr));
                            let backroll_player_handle =
                                builder.add_player(BackrollPlayer::Remote(remote_peer));
                            backroll_player_handle
                        };
                        players.push(Player {
                            backroll_player_handle,
                            collider: collision_world.add_actor(vec2(x as f32, y as f32), 8, 8),
                            speed: vec2(0., 0.),
                            prev_jump_down: false,
                            facing_right: true,
                            health: 100,
                            gun_clock: 0,
                        })
                    }
                    let session = builder.start(task_pool).unwrap();
                    break (session, local_player.unwrap(), players, connection_manager);
                }
                next_frame().await;
            }
        }
        let explosions =
            EmittersCache::new(nanoserde::DeJson::deserialize_json(EXPLOSION_FX).unwrap());

        let tileset = load_texture("client/assets/tileset.png").await.unwrap();
        tileset.set_filter(FilterMode::Nearest);

        let tiled_map_json = load_string("client/assets/map.json").await.unwrap();
        let tiled_map = tiled::load_map(&tiled_map_json, &[("tileset.png", tileset)], &[]).unwrap();

        let mut static_colliders = vec![];
        for (_x, _y, tile) in tiled_map.tiles("main layer", None) {
            static_colliders.push(tile.is_some());
        }

        let mut collision_world = CollisionWorld::new();
        collision_world.add_static_tiled_layer(static_colliders, 8., 8., 40, 1);

        let camera = Camera2D::from_display_rect(Rect::new(0.0, 0.0, 320.0, 152.0));

        let (session, local_player, players, connection_manager) =
            connect("0.0.0.0:8090".parse().unwrap(), &mut collision_world).await;

        Self {
            _connection_manager: connection_manager,
            session,
            local_player,
            explosions,
            tiled_map,
            collision_world,
            camera,
            players,
            bullets: vec![],
            frames_to_stall: 0,
        }
    }

    fn update(&mut self) {
        telemetry::begin_zone("Main loop");

        telemetry::begin_zone("pre flush");
        self.run_commands(self.session.poll());
        telemetry::end_zone();

        if self.frames_to_stall > 0 {
            self.frames_to_stall -= 1;
        } else if self.session.is_synchronized() {
            telemetry::begin_zone("local input");
            match self
                .session
                .add_local_input(self.local_player, Input::current())
            {
                Ok(()) => {
                    telemetry::begin_zone("advance frame");
                    self.run_commands(self.session.advance_frame());
                    telemetry::end_zone();
                }
                Err(BackrollError::ReachedPredictionBarrier) => {
                    warn!("Prediction barrier reached. Stalling.");
                }
                Err(err) => {
                    panic!("Error in adding local input: ({:?}) {}", err, err);
                }
            }
            telemetry::end_zone();
        }

        //profiler::profiler(profiler::ProfilerParams {
        //    fps_counter_pos: vec2(50.0, 20.0),
        //});

        telemetry::end_zone();
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
                                let pos = self.collision_world.actor_pos(player.collider);
                                PlayerState {
                                    x: OrderedFloat(pos.x),
                                    y: OrderedFloat(pos.y),
                                    vx: OrderedFloat(player.speed.x),
                                    vy: OrderedFloat(player.speed.y),
                                    prev_jump_down: player.prev_jump_down,
                                    facing_right: player.facing_right,
                                    health: player.health,
                                    gun_clock: player.gun_clock,
                                }
                            })
                            .collect(),
                        bullets: self
                            .bullets
                            .iter()
                            .map(|bullet| BulletState {
                                x: OrderedFloat(bullet.pos.x),
                                y: OrderedFloat(bullet.pos.y),
                                vx: OrderedFloat(bullet.speed.x),
                                vy: OrderedFloat(bullet.speed.y),
                                lived: OrderedFloat(bullet.lived),
                                lifetime: OrderedFloat(bullet.lifetime),
                            })
                            .collect(),
                    });
                }
                Command::Load(load_state) => {
                    let state = load_state.load();
                    for (player_state, player) in state.players.iter().zip(self.players.iter_mut())
                    {
                        *player = Player {
                            backroll_player_handle: player.backroll_player_handle,
                            collider: player.collider,
                            speed: Vec2::new(player_state.vx.0, player_state.vy.0),
                            prev_jump_down: player_state.prev_jump_down,
                            facing_right: player_state.facing_right,
                            health: player_state.health,
                            gun_clock: player_state.gun_clock,
                        };
                        self.collision_world.set_actor_position(
                            player.collider,
                            Vec2::new(player_state.x.0, player_state.y.0),
                        );
                    }
                    for (bullet_state, bullet) in state.bullets.iter().zip(self.bullets.iter_mut())
                    {
                        *bullet = Bullet {
                            pos: vec2(bullet_state.x.0, bullet_state.y.0),
                            speed: vec2(bullet_state.vx.0, bullet_state.vy.0),
                            lived: bullet_state.lived.0,
                            lifetime: bullet_state.lifetime.0,
                        }
                    }
                }
                Command::AdvanceFrame(input) => {
                    for player in &mut self.players {
                        let pos = self.collision_world.actor_pos(player.collider);
                        let on_ground = self
                            .collision_world
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
                            if player_input.contains(Input::SHOOT) {
                                if player.gun_clock == 0 {
                                    let dir = if player.facing_right {
                                        vec2(1.0, 0.0)
                                    } else {
                                        vec2(-1.0, 0.0)
                                    };
                                    self.bullets.push(Bullet {
                                        pos: pos + vec2(4.0, 4.0) + dir * 8.0,
                                        speed: dir * consts::BULLET_SPEED,
                                        lived: 0.0,
                                        lifetime: 0.7,
                                    });
                                }
                                player.gun_clock += 1;
                                player.gun_clock %= consts::BULLET_INTERVAL_TICKS;
                            } else {
                                player.gun_clock = 0;
                            }
                        }

                        if player.speed.x < 0.0 {
                            player.facing_right = false;
                        }
                        if player.speed.x > 0.0 {
                            player.facing_right = true;
                        }

                        if on_ground == false {
                            player.speed.y += consts::GRAVITY * consts::TIMESTEP;
                        }

                        self.collision_world
                            .move_h(player.collider, player.speed.x * consts::TIMESTEP);
                        if !self
                            .collision_world
                            .move_v(player.collider, player.speed.y * consts::TIMESTEP)
                        {
                            player.speed.y = 0.0;
                        }

                        // HACK: clear the position remainders that are not saved in the backroll state.
                        self.collision_world.set_actor_position(
                            player.collider,
                            self.collision_world.actor_pos(player.collider),
                        );
                    }

                    {
                        let _z = telemetry::ZoneGuard::new("update bullets");

                        for bullet in &mut self.bullets {
                            bullet.pos += bullet.speed * consts::TIMESTEP;
                            bullet.lived += consts::TIMESTEP;
                        }
                        let explosions = &mut self.explosions;
                        let collision_world = &mut self.collision_world;
                        let players = &mut self.players;

                        self.bullets.retain(|bullet| {
                            if collision_world.solid_at(bullet.pos) {
                                explosions.spawn(bullet.pos);
                                return false;
                            }
                            for player in players.iter_mut() {
                                let player_pos = collision_world.actor_pos(player.collider);
                                if Rect::new(player_pos.x, player_pos.y, 8.0, 8.0)
                                    .contains(bullet.pos)
                                {
                                    player.health -= 5;
                                    explosions.spawn(bullet.pos);
                                    return false;
                                }
                            }
                            bullet.lived < bullet.lifetime
                        });
                    }

                    self.players.retain(|player| player.health > 0);
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
                    self.frames_to_stall = frames_ahead;
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
            let pos = self.collision_world.actor_pos(player.collider);

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

            draw_rectangle(pos.x as f32 - 4.0, pos.y as f32 - 5.0, 16.0, 2.0, RED);
            draw_rectangle(
                pos.x as f32 - 4.0,
                pos.y as f32 - 5.0,
                player.health.clamp(0, 100) as f32 / 100.0 * 16.0,
                2.0,
                GREEN,
            );

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
        }

        telemetry::end_zone();

        for bullet in &self.bullets {
            draw_circle(
                bullet.pos.x,
                bullet.pos.y,
                1.0,
                Color::new(1.0, 1.0, 0.8, 1.0),
            );
        }
        {
            let _z = telemetry::ZoneGuard::new("draw particles");
            self.explosions.draw();
        }

        set_default_camera();
    }
}

#[macroquad::main("Platformer")]
async fn main() {
    let mut game = Game::new().await;

    let mut seconds_behind = 0.0;

    loop {
        seconds_behind += get_frame_time();

        while seconds_behind > 0.0 {
            seconds_behind -= consts::TIMESTEP;
            game.update();
        }

        game.draw();
        next_frame().await;
    }
}
