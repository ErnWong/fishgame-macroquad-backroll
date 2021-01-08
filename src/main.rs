use macroquad::prelude::*;

use macroquad_particles as particles;
use macroquad_profiler as profiler;
use macroquad_tiled as tiled;

use macroquad::telemetry;
use nanoserde::DeBin;
use particles::EmittersCache;
use physics_platformer::{Actor, World as CollisionWorld};
use std::collections::HashMap;

mod nakama;

mod consts {
    pub const GRAVITY: f32 = 900.0;
    pub const JUMP_SPEED: f32 = 250.0;
    pub const RUN_SPEED: f32 = 150.0;
    pub const PLAYER_SPRITE: u32 = 120;
    pub const BULLET_SPEED: f32 = 300.0;

    pub const NETWORK_FPS: f32 = 15.0;
}

struct Player {
    collider: Actor,
    speed: Vec2,
    facing: bool,
    health: i32,
}

struct Other {
    pos: Vec2,
    facing: bool,
    health: i32,
}

impl Other {
    fn new() -> Other {
        Other {
            pos: vec2(0., 0.),
            facing: true,
            health: 100,
        }
    }
}
struct Bullet {
    pos: Vec2,
    speed: Vec2,
    lived: f32,
    lifetime: f32,
}

bitfield::bitfield! {
    struct PlayerStateBits([u8]);
    impl Debug;
    u32;
    x, set_x: 9, 0;
    y, set_y: 19, 10;
    facing, set_facing: 20;
    shooting, set_shooting: 21;
}

#[test]
fn test_bitfield() {
    let mut bits = PlayerStateBits([0; 3]);

    bits.set_x(345);
    bits.set_y(567);
    bits.set_facing(true);
    bits.set_shooting(false);

    assert_eq!(bits.x(), 345);
    assert_eq!(bits.y(), 567);
    assert_eq!(bits.facing(), true);
    assert_eq!(bits.shooting(), false);
    assert_eq!(std::mem::size_of_val(&bits), 3);
}

mod message {
    use nanoserde::{DeBin, SerBin};

    #[derive(Debug, Clone, SerBin, DeBin, PartialEq)]
    pub struct Move(pub [u8; 3]);
    impl Move {
        pub const OPCODE: i32 = 1;
    }

    #[derive(Debug, Clone, SerBin, DeBin, PartialEq)]
    pub struct SelfDamage(pub u8);
    impl SelfDamage {
        pub const OPCODE: i32 = 2;
    }

    #[derive(Debug, Clone, SerBin, DeBin, PartialEq)]
    pub struct Died;
    impl Died {
        pub const OPCODE: i32 = 3;
    }
}

struct NetworkCache {
    sent_health: i32,
    sent_position: [u8; 3],
    last_send_time: f64,
}

impl NetworkCache {
    fn flush(&mut self) {
        self.sent_health = 100;
        self.sent_position = [0; 3];
        self.last_send_time = 0.0;
    }
}

struct World {
    explosions: EmittersCache,
    collision_world: CollisionWorld,
    tiled_map: tiled::Map,
    player: Player,
    others: HashMap<String, Other>,
    bullets: Vec<Bullet>,
    network_cache: NetworkCache,
}

impl World {
    async fn new() -> World {
        let mut collision_world = CollisionWorld::new();

        let tileset = load_texture("assets/tileset.png").await;
        set_texture_filter(tileset, FilterMode::Nearest);

        let tiled_map_json = load_string("assets/map.json").await.unwrap();
        let tiled_map = tiled::load_map(&tiled_map_json, &[("tileset.png", tileset)], &[]).unwrap();

        let mut static_colliders = vec![];
        for (_x, _y, tile) in tiled_map.tiles("main layer", None) {
            static_colliders.push(tile.is_some());
        }

        collision_world.add_static_tiled_layer(static_colliders, 8., 8., 40, 1);

        let spawner_pos = {
            let objects = &tiled_map.layers["logic"].objects;
            let macroquad_tiled::Object::Rect {
                world_x, world_y, ..
            } = objects[rand::gen_range(0, objects.len()) as usize];

            vec2(world_x, world_y)
        };

        let explosions =
            EmittersCache::new(nanoserde::DeJson::deserialize_json(EXPLOSION_FX).unwrap());

        World {
            explosions,
            player: Player {
                collider: collision_world.add_actor(spawner_pos, 8, 8),
                speed: vec2(0., 0.),
                facing: true,
                health: 100,
            },
            tiled_map,
            collision_world,
            bullets: vec![],
            others: HashMap::new(),
            network_cache: NetworkCache {
                sent_position: [0; 3],
                last_send_time: 0.0,
                sent_health: 100,
            },
        }
    }

    pub fn sync_state(&mut self) {
        {
            let shooting = is_key_pressed(KeyCode::LeftControl);
            let network_frame =
                get_time() - self.network_cache.last_send_time > (1. / consts::NETWORK_FPS) as f64;

            if shooting || network_frame {
                self.network_cache.last_send_time = get_time();

                let pos = self.collision_world.actor_pos(self.player.collider);
                let mut state = PlayerStateBits([0; 3]);

                state.set_x(pos.x as u32);
                state.set_y(pos.y as u32);
                state.set_facing(self.player.facing);
                state.set_shooting(shooting);

                if self.network_cache.sent_position != state.0 {
                    self.network_cache.sent_position = state.0;
                    nakama::send_bin(message::Move::OPCODE, &message::Move(state.0));
                }

                if self.network_cache.sent_health != self.player.health {
                    if self.player.health < 0 {
                        self.network_cache.sent_health = 100;
                        self.player.health = 100;
                        nakama::send_bin(message::Died::OPCODE, &message::Died);
                    } else {
                        nakama::send_bin(
                            message::SelfDamage::OPCODE,
                            &message::SelfDamage(self.player.health as u8),
                        );
                        self.network_cache.sent_health = self.player.health;
                    }
                }
            }
        }

        while let Some(event) = nakama::events() {
            match event {
                nakama::Event::Leave(leaver) => {
                    self.others.remove(&leaver);
                }
                nakama::Event::Join(joined) => {
                    self.network_cache.flush();
                    self.others.insert(joined, Other::new());
                }
            }
        }

        while let Some(msg) = nakama::try_recv() {
            if let Some(other) = self.others.get_mut(&msg.user_id) {
                match msg.opcode as i32 {
                    message::Move::OPCODE => {
                        let message::Move(data) = DeBin::deserialize_bin(&msg.data).unwrap();
                        let state = PlayerStateBits(data);

                        let facing = state.facing();
                        let shooting = state.shooting();
                        let pos = vec2(state.x() as f32, state.y() as f32);

                        other.pos = pos;
                        other.facing = facing;
                        if shooting {
                            self.spawn_bullet(pos, facing);
                        }
                    }
                    message::SelfDamage::OPCODE => {
                        let message::SelfDamage(health) =
                            DeBin::deserialize_bin(&msg.data).unwrap();

                        other.health = health as i32;
                    }
                    message::Died::OPCODE => {}
                    opcode => {
                        warn!("Unknown opcode: {}", opcode);
                    }
                }
            }
        }
    }

    fn spawn_bullet(&mut self, pos: Vec2, facing: bool) {
        let dir = if facing {
            vec2(1.0, 0.0)
        } else {
            vec2(-1.0, 0.0)
        };
        self.bullets.push(Bullet {
            pos: pos + vec2(4.0, 4.0) + dir * 8.0,
            speed: dir * consts::BULLET_SPEED,
            lived: 0.0,
            lifetime: 0.7,
        })
    }

    pub fn draw(&mut self) {
        for _ in 0..1 {
            self.tiled_map
                .draw_tiles("main layer", Rect::new(0.0, 0.0, 320.0, 152.0), None);
        }

        // draw player
        {
            let pos = self.collision_world.actor_pos(self.player.collider);

            if self.player.speed.x < 0.0 {
                self.player.facing = false;
            }
            if self.player.speed.x > 0.0 {
                self.player.facing = true;
            }
            draw_rectangle(pos.x as f32 - 4.0, pos.y as f32 - 5.0, 16.0, 2.0, RED);
            draw_rectangle(
                pos.x as f32 - 4.0,
                pos.y as f32 - 5.0,
                self.player.health as f32 / 100.0 * 16.0,
                2.0,
                GREEN,
            );

            if self.player.facing {
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

            if is_key_pressed(KeyCode::LeftControl) {
                let pos = self.collision_world.actor_pos(self.player.collider);

                self.spawn_bullet(pos, self.player.facing);
            }
        }

        // draw other others
        for (
            other_id,
            Other {
                pos: Vec2 { x, y },
                facing,
                health,
                ..
            },
        ) in self.others.values().enumerate()
        {
            draw_text_ex(
                &format!("player {}", other_id),
                *x as f32 - 4.0,
                *y as f32 - 6.0,
                TextParams {
                    font_size: 30,
                    font_scale: 0.15,
                    ..Default::default()
                },
            );
            draw_rectangle(*x as f32 - 4.0, *y as f32 - 5.0, 16.0, 1.5, RED);
            draw_rectangle(
                *x as f32 - 4.0,
                *y as f32 - 5.0,
                *health as f32 / 100.0 * 16.0,
                1.5,
                GREEN,
            );

            if *facing {
                self.tiled_map.spr(
                    "tileset",
                    consts::PLAYER_SPRITE,
                    Rect::new(*x as f32, *y as f32, 8.0, 8.0),
                );
            } else {
                self.tiled_map.spr(
                    "tileset",
                    consts::PLAYER_SPRITE,
                    Rect::new(*x as f32 + 8.0, *y as f32, -8.0, 8.0),
                );
            }
        }

        for bullet in &self.bullets {
            draw_circle(
                bullet.pos.x,
                bullet.pos.y,
                1.,
                Color::new(1.0, 1.0, 0.8, 1.0),
            );
        }

        {
            let _z = telemetry::ZoneGuard::new("draw particles");
            self.explosions.draw();
        }
    }

    pub fn update(&mut self) {
        let player_pos = self.collision_world.actor_pos(self.player.collider);

        let on_ground = self
            .collision_world
            .collide_check(self.player.collider, player_pos + vec2(0., 1.));

        if on_ground == false {
            self.player.speed.y += consts::GRAVITY * get_frame_time();
        }

        if is_key_down(KeyCode::Right) {
            self.player.speed.x = consts::RUN_SPEED;
        } else if is_key_down(KeyCode::Left) {
            self.player.speed.x = -consts::RUN_SPEED;
        } else {
            self.player.speed.x = 0.;
        }

        if is_key_pressed(KeyCode::Space) {
            if on_ground {
                self.player.speed.y = -consts::JUMP_SPEED;
            }
        }

        self.collision_world
            .move_h(self.player.collider, self.player.speed.x * get_frame_time());
        if !self
            .collision_world
            .move_v(self.player.collider, self.player.speed.y * get_frame_time())
        {
            self.player.speed.y = 0.0;
        }

        {
            let _z = telemetry::ZoneGuard::new("update bullets");

            for bullet in &mut self.bullets {
                bullet.pos += bullet.speed * get_frame_time();
                bullet.lived += get_frame_time();
            }
            let explosions = &mut self.explosions;
            let collision_world = &mut self.collision_world;
            let player = &mut self.player;
            let others = &mut self.others;

            self.bullets.retain(|bullet| {
                let self_damaged =
                    Rect::new(player_pos.x, player_pos.y, 8., 8.).contains(bullet.pos);

                if self_damaged {
                    player.health -= 5;
                }

                if collision_world.solid_at(bullet.pos)
                    || others.values().any(|other| {
                        Rect::new(other.pos.x, other.pos.y, 8.0, 8.0).contains(bullet.pos)
                    })
                    || self_damaged
                {
                    explosions.spawn(bullet.pos);
                    return false;
                }
                bullet.lived < bullet.lifetime
            });
        }
    }
}

pub const EXPLOSION_FX: &'static str = r#"{"local_coords":false,"emission_shape":{"Point":[]},"one_shot":true,"lifetime":0.15,"lifetime_randomness":0,"explosiveness":0.65,"amount":41,"shape":{"Circle":{"subdivisions":10}},"emitting":false,"initial_direction":{"x":0,"y":-1},"initial_direction_spread":6.2831855,"initial_velocity":30,"initial_velocity_randomness":0.2,"linear_accel":0,"size":1.5000002,"size_randomness":0.4,"blend_mode":{"Alpha":[]},"colors_curve":{"start":{"r":0.8200004,"g":1,"b":0.31818175,"a":1},"mid":{"r":0.71000004,"g":0.36210018,"b":0,"a":1},"end":{"r":0.02,"g":0,"b":0.000000007152557,"a":1}},"gravity":{"x":0,"y":0},"post_processing":{}}
"#;

#[macroquad::main("Platformer")]
async fn main() {
    #[cfg(target_arch = "wasm32")]
    {
        while nakama::connected() == false {
            clear_background(BLACK);
            draw_text(
                &format!(
                    "Connecting {}",
                    ".".repeat(((get_time() * 2.0) as usize) % 4)
                ),
                screen_width() / 2.0 - 100.0,
                screen_height() / 2.0,
                40.,
                WHITE,
            );

            next_frame().await;
        }
    }

    rand::srand(get_time() as u64);

    let camera = Camera2D::from_display_rect(Rect::new(0.0, 0.0, 320.0, 152.0));

    let mut world = World::new().await;

    loop {
        world.sync_state();

        clear_background(BLACK);

        set_camera(camera);

        world.draw();
        world.update();

        set_default_camera();

        profiler::profiler(profiler::ProfilerParams {
            fps_counter_pos: vec2(50.0, 20.0),
        });

        next_frame().await;
    }
}
