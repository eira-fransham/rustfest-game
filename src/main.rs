extern crate graphics;
extern crate piston_window;
extern crate cgmath;

use piston_window::*;
use graphics::math::*;
use cgmath::*;

static TEAM_COLORS: &[[f32; 4]] = &[
    [1., 0., 0., 1.],
    [0., 0., 1., 1.],
    [0., 1., 0., 1.],
    [1., 1., 0., 1.],
    [1., 0., 1., 1.],
    [0., 1., 1., 1.],
];
static BLACK: &[f32; 4] = &[0., 0., 0., 1.];

fn team_color(team: Team) -> &'static [f32; 4] {
    TEAM_COLORS.get(team.0 as usize).unwrap_or(BLACK)
}

struct EntityId(usize);
#[derive(Clone, Copy)]
struct Team(u8);

#[derive(PartialEq, Eq)]
enum ControllerType {
    Local,
    Remote { remote_id: usize },
}

#[derive(PartialEq, Eq, Default)]
struct ControllerState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    fire: bool,
}

#[derive(PartialEq, Eq)]
struct Controller {
    state: ControllerState,
    controller_type: Option<ControllerType>,
}

impl Controller {
    fn update(&mut self, ev: &Event) {
        use self::Button::*;

        if self.controller_type == Some(ControllerType::Local) {
            match ev.button_args() {
                Some(ButtonArgs { state, button, .. }) => {
                    match button {
                        Keyboard(Key::Up) => self.state.up = state == ButtonState::Press,
                        Keyboard(Key::Down) => self.state.down = state == ButtonState::Press,
                        Keyboard(Key::Left) => self.state.left = state == ButtonState::Press,
                        Keyboard(Key::Right) => self.state.right = state == ButtonState::Press,
                        Keyboard(Key::Space) => self.state.fire = state == ButtonState::Press,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn is_firing(&self) -> bool {
        self.state.fire
    }

    fn desired_rotation(&self) -> f64 {
        (self.state.left as i8 - self.state.right as i8) as f64
    }

    fn desired_thrust(&self) -> f64 {
        (self.state.up as i8 - self.state.down as i8) as f64
    }

    fn local() -> Self {
        Controller {
            state: Default::default(),
            controller_type: Some(ControllerType::Local),
        }
    }

    fn remote(id: usize) -> Self {
        Controller {
            state: Default::default(),
            controller_type: Some(ControllerType::Remote { remote_id: id }),
        }
    }

    fn slave() -> Self {
        Controller {
            state: Default::default(),
            controller_type: None,
        }
    }
}

#[derive(PartialEq)]
enum EntityType {
    Player {
        controller: Controller,
        last_fire: f64,
    },
    Bullet { born_at: f64 },
}

struct GameEntity {
    entity_type: EntityType,
    owning_team: Team,
    color: &'static [f32; 4],
    max_velocity: f64,
    position: Vector2<f64>,
    velocity: Vector2<f64>,
    acceleration: Vector2<f64>,
    rotation: Rad<f64>,
    polygon: &'static [[f64; 2]],
    scale: f64,
}

impl GameEntity {
    fn with_position(mut self, new_pos: Vector2<f64>) -> Self {
        self.position = new_pos;
        self
    }

    fn wrapping_transformations(&self) -> [Matrix2d; 5] {
        fn with_trans(e: &GameEntity, (x, y): (f64, f64)) -> Matrix2d {
            [[1., 0., 0.], [0., 1., 0.]]
                .trans(e.position.x, e.position.y)
                .trans(x, y)
                .rot_rad(e.rotation.0)
                .scale(e.scale, e.scale)
        }

        [
            with_trans(self, (0., 0.)),
            with_trans(self, (2., 0.)),
            with_trans(self, (-2., 0.)),
            with_trans(self, (0., -2.)),
            with_trans(self, (0., 2.)),
        ]
    }

    fn handle_edges(&mut self) {
        if let EntityType::Player { .. } = self.entity_type {
            self.position.x = ((self.position.x + 3.) % 2.) - 1.;
            self.position.y = ((self.position.y + 3.) % 2.) - 1.;
        }
    }

    fn should_die(&self, world_time: f64) -> bool {
        match self.entity_type {
            EntityType::Player { .. } => false,
            EntityType::Bullet { born_at } => world_time > born_at + BULLET_LIFESPAN,
        }
    }
}

fn player(team: Team) -> GameEntity {
    GameEntity {
        entity_type: EntityType::Player {
            controller: Controller::local(),
            last_fire: 0.,
        },
        owning_team: team,
        color: team_color(team),
        max_velocity: PLAYER_MAX_SPEED,
        position: Vector2 { x: 0., y: 0. },
        velocity: Vector2 { x: 0., y: 0. },
        acceleration: Vector2 { x: 0., y: 0. },
        rotation: Rad(0.),
        polygon: &PLAYER_POLY,
        scale: 0.1,
    }
}

fn bullet(player: &GameEntity, world_time: f64) -> GameEntity {
    let fire_norm = Basis2::from_angle(player.rotation).rotate_vector(Vector2::unit_y());
    let fire_vel = fire_norm * BULLET_SPEED;

    GameEntity {
        entity_type: EntityType::Bullet { born_at: world_time },
        owning_team: player.owning_team,
        color: player.color,
        max_velocity: BULLET_SPEED,
        position: player.position + fire_norm * player.scale,
        velocity: fire_vel,
        acceleration: Vector2 { x: 0., y: 0. },
        rotation: Rad(0.),
        polygon: &BULLET_POLY,
        scale: 0.01,
    }
}

static PLAYER_POLY: [[f64; 2]; 4] = [[0., 1.], [1., -1.], [0., -0.5], [-1., -1.]];
static BULLET_POLY: [[f64; 2]; 4] = [[0., 1.], [1., 0.], [0., -1.], [-1., 0.]];

const PLAYER_ROTATION_SPEED: f64 = 4.0;
const PLAYER_THRUST_ACC: f64 = 2.0;
const PLAYER_MAX_SPEED: f64 = 1.0;

const BULLET_SPEED: f64 = 1.2;
const BULLET_LIFESPAN: f64 = 1.;

fn main() {
    let mut entities = vec![player(Team(0))];

    let mut window: PistonWindow = WindowSettings::new("Not Asteroids", [640, 480])
        .exit_on_esc(true)
        .vsync(true)
        .build()
        .unwrap();

    let world_scale = 0.1;
    let mut world_time = 0.;

    while let Some(event) = window.next() {
        if let Event::Loop(Loop::Update(UpdateArgs { dt })) = event {
            world_time += dt
        }

        let mut new = vec![];

        for e in &mut entities {
            let make_bullet = if let EntityType::Player {
                controller: ref mut c,
                last_fire: ref mut last,
            } = e.entity_type
            {
                c.update(&event);

                if let Event::Loop(Loop::Update(UpdateArgs { dt })) = event {
                    e.rotation += Rad(c.desired_rotation() * dt) * PLAYER_ROTATION_SPEED;

                    let rot: Basis2<_> = Rotation2::from_angle(e.rotation);
                    e.acceleration = rot.rotate_vector(
                        Vector2::unit_y() * c.desired_thrust() * PLAYER_THRUST_ACC,
                    );

                    if c.is_firing() && world_time > *last + 0.2 {
                        *last = world_time;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            // Workaround since we don't have NLL
            if make_bullet {
                new.push(bullet(&e, world_time))
            }

            if let Event::Loop(Loop::Update(UpdateArgs { dt })) = event {
                e.velocity += e.acceleration * dt;
                e.position += e.velocity * dt;

                if e.velocity.magnitude() > e.max_velocity {
                    e.velocity = e.velocity.normalize() * e.max_velocity;
                }

                e.handle_edges();
            }
        }

        window.draw_2d(&event, |context, graphics| {
            clear([0., 0., 0., 1.], graphics);

            for e in &entities {
                for trans in e.wrapping_transformations().iter() {
                    polygon(e.color.clone(), e.polygon, trans.clone(), graphics);
                }
            }
        });

        entities.retain(|e| !e.should_die(world_time));

        entities.extend(new);
    }
}
