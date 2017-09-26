#[macro_use]
extern crate serde_derive;

extern crate smallvec;
extern crate serde;
extern crate rmp;
extern crate rmp_serde;
extern crate graphics;
extern crate piston_window;
extern crate cgmath;
extern crate itertools;
extern crate getopts;
extern crate rusttype;
extern crate rand;
extern crate mio;

use smallvec::SmallVec;
use piston_window::*;
use graphics::math::*;
use cgmath::*;
use itertools::*;

use std::net::ToSocketAddrs;
use std::collections::{HashMap, HashSet};
use std::ops::{Add, Sub, Mul};
use std::sync::mpsc::{self, Sender, Receiver};

const DEFAULT_SERVER_LOCATION: &str = "127.0.0.1:55555";
const DEJAVU_SANS_MONO: &[u8] = include_bytes!("/usr/share/fonts/TTF/DejaVuSansMono.ttf");

fn lerp<T: Add<Output = T> + Sub<Output = T> + Mul<Output = T> + Copy>(a: T, b: T, bias: T) -> T {
    a + (b - a) * bias
}

const TEAM_COLORS: &[[f32; 4]] = &[
    [1., 0., 0., 1.],
    [0., 0., 1., 1.],
    [0., 1., 0., 1.],
    [1., 1., 0., 1.],
    [0., 1., 1., 1.],
    [1., 0., 1., 1.],
    [1., 0.25, 0.5, 1.],
    [0.55, 0.15, 0.9, 1.],
    [1., 0.5, 0.25, 1.],
    [0.27, 0.5, 0.7, 1.],
];

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
struct Team(u8);

struct IdGen(u32);

impl IdGen {
    fn next(&mut self) -> u32 {
        let out = self.0;
        self.0 += 1;
        out
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub enum GameKey {
    Up,
    Down,
    Left,
    Right,
    Fire,
}

mod packets {
    #![allow(dead_code)]

    use std::net::SocketAddr;
    use std::io;
    use mio::net::{TcpStream, UdpSocket};
    use smallvec::SmallVec;
    use super::{ControllerState, Drawable};

    pub trait PrioSend<Out> {
        fn send_high(&mut self, val: &Out) -> Result<(), io::Error>;
        fn send_low(&mut self, val: &Out) -> Result<(), io::Error>;
    }

    impl<Out, A: Sender<Out>, B> PrioSend<Out> for (A, B) {
        fn send_high(&mut self, val: &Out) -> Result<(), io::Error> {
            self.0.send(val)
        }
        fn send_low(&mut self, val: &Out) -> Result<(), io::Error> {
            self.0.send(val)
        }
    }

    impl<Out: ::serde::Serialize> PrioSend<Out> for TcpStream {
        fn send_high(&mut self, val: &Out) -> Result<(), io::Error> {
            self.send(val)
        }

        fn send_low(&mut self, val: &Out) -> Result<(), io::Error> {
            use rmp_serde::encode::{self, Error};
            use rmp::encode::ValueWriteError;

            encode::write(self, val).map_err(|e| match e {
                Error::InvalidValueWrite(ValueWriteError::InvalidMarkerWrite(e)) |
                Error::InvalidValueWrite(ValueWriteError::InvalidDataWrite(e)) => e,
                _ => io::Error::from(io::ErrorKind::Other),
            })
        }
    }

    pub struct PrioSocket {
        high: TcpStream,
        low: NoncedUdpStream,
    }

    impl PrioSocket {
        pub fn connect(addr: &SocketAddr) -> io::Result<Self> {
            Self::from_tcp(TcpStream::connect(addr)?)
        }

        pub fn from_tcp(stream: TcpStream) -> io::Result<Self> {
            let _ = stream.set_nodelay(true);
            let low = NoncedUdpStream::connect(&stream.local_addr()?, &stream.peer_addr()?)?;
            Ok(PrioSocket {
                high: stream,
                low: low,
            })
        }
    }

    impl<Out: ::serde::Serialize> PrioSend<Out> for PrioSocket {
        fn send_high(&mut self, val: &Out) -> Result<(), io::Error> {
            self.high.send(val)
        }
        fn send_low(&mut self, val: &Out) -> Result<(), io::Error> {
            self.low.send(val)
        }
    }

    impl<In: for<'a> ::serde::Deserialize<'a>> Receiver<In> for PrioSocket {
        fn try_recv(&mut self) -> Result<In, io::Error> {
            self.high.try_recv().or_else(|_| self.low.try_recv())
        }
    }

    pub trait Sender<T> {
        fn send(&mut self, val: &T) -> Result<(), io::Error>;
    }

    pub trait Receiver<T> {
        fn try_recv(&mut self) -> Result<T, io::Error>;
    }

    pub trait SendRecv<In, Out>: PrioSend<Out> + Receiver<In> {}

    impl<In, Out, T: PrioSend<Out> + Receiver<In>> SendRecv<In, Out> for T {}

    impl<T, A: Sender<T>, B> Sender<T> for (A, B) {
        fn send(&mut self, val: &T) -> Result<(), io::Error> {
            self.0.send(val)
        }
    }

    impl<T, A, B: Receiver<T>> Receiver<T> for (A, B) {
        fn try_recv(&mut self) -> Result<T, io::Error> {
            self.1.try_recv()
        }
    }

    impl<T: Clone> Sender<T> for ::std::sync::mpsc::Sender<T> {
        fn send(&mut self, val: &T) -> Result<(), io::Error> {
            ::std::sync::mpsc::Sender::send(&*self, val.clone())
                .map_err(|_| ::std::io::Error::from(::std::io::ErrorKind::Other))
        }
    }

    // TODO: Make this act like a `TcpStream` that silently turns out-of-order packets into dropped
    //       packets.
    struct NoncedUdpStream {
        stream: UdpSocket,
        out_nonce: u32,
        in_nonce: Option<u32>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct NoncedMessage<T>(u32, T);

    impl NoncedUdpStream {
        fn connect(local: &SocketAddr, remote: &SocketAddr) -> io::Result<Self> {
            let backing_socket = UdpSocket::bind(local)?;
            backing_socket.connect(*remote)?;

            Ok(NoncedUdpStream {
                stream: backing_socket,
                in_nonce: None,
                out_nonce: 0,
            })
        }
    }

    impl<T: ::serde::Serialize> Sender<T> for NoncedUdpStream {
        fn send(&mut self, val: &T) -> Result<(), io::Error> {
            use rmp_serde::encode::{self, Error};
            use rmp::encode::ValueWriteError;

            let mut buffer: SmallVec<[u8; 1024]> = SmallVec::new();
            let nonce = self.out_nonce;
            self.out_nonce += 1;

            let to_encode: NoncedMessage<&T> = NoncedMessage(nonce, val);
            encode::write(&mut buffer, &to_encode).map_err(|e| {
                println!("{:?}", e);
                match e {
                    Error::InvalidValueWrite(ValueWriteError::InvalidMarkerWrite(e)) |
                    Error::InvalidValueWrite(ValueWriteError::InvalidDataWrite(e)) => e,
                    _ => io::Error::from(io::ErrorKind::Other),
                }
            })?;

            let mut to_write: &[u8] = &buffer;
            self.stream.send(&buffer).map(|_| ())
        }
    }

    impl<T: ::serde::Serialize> Sender<T> for TcpStream {
        fn send(&mut self, val: &T) -> Result<(), io::Error> {
            use rmp_serde::encode::{self, Error};
            use rmp::encode::ValueWriteError;

            loop {
                let out = encode::write(self, val).map_err(|e| match e {
                    Error::InvalidValueWrite(ValueWriteError::InvalidMarkerWrite(e)) |
                    Error::InvalidValueWrite(ValueWriteError::InvalidDataWrite(e)) => e,
                    _ => io::Error::from(io::ErrorKind::Other),
                });

                if let &Err(ref e) = &out {
                    if e.kind() == io::ErrorKind::WouldBlock {
                        continue;
                    }
                }

                return out;
            }
        }
    }

    impl<T> Receiver<T> for ::std::sync::mpsc::Receiver<T> {
        fn try_recv(&mut self) -> Result<T, ::std::io::Error> {
            ::std::sync::mpsc::Receiver::try_recv(&*self).map_err(|e| {
                use std::sync::mpsc::TryRecvError;

                match e {
                    TryRecvError::Disconnected => ::std::io::Error::from(
                        ::std::io::ErrorKind::Other,
                    ),
                    TryRecvError::Empty => ::std::io::Error::from(::std::io::ErrorKind::WouldBlock),
                }
            })
        }
    }

    impl<T: for<'a> ::serde::Deserialize<'a>> Receiver<T> for ::mio::net::TcpStream {
        fn try_recv(&mut self) -> Result<T, ::std::io::Error> {
            use rmp_serde::decode::Error;

            ::rmp_serde::from_read(self).map_err(|e| match e {
                Error::InvalidMarkerRead(e) |
                Error::InvalidDataRead(e) => e,
                _ => ::std::io::Error::from(::std::io::ErrorKind::Other),
            })
        }
    }

    impl<T: for<'a> ::serde::Deserialize<'a>> Receiver<T> for NoncedUdpStream {
        fn try_recv(&mut self) -> Result<T, ::std::io::Error> {
            use rmp_serde::decode::Error;
            let mut buffer: [u8; 1024] = [0; 1024];

            let num_read = self.stream.recv(&mut buffer)?;

            let nonced: NoncedMessage<T> = ::rmp_serde::from_slice(&buffer[..num_read]).map_err(|e| {
                println!("{:?}", e);
                match e {
                    Error::InvalidMarkerRead(e) |
                    Error::InvalidDataRead(e) => e,
                    _ => ::std::io::Error::from(::std::io::ErrorKind::Other),
                }
            })?;

            if self.in_nonce.map(|nonce| nonced.0 <= nonce).unwrap_or(
                false,
            )
            {
                return Err(::std::io::Error::from(::std::io::ErrorKind::Other));
            }

            self.in_nonce = Some(nonced.0);

            Ok(nonced.1)
        }
    }

    #[derive(Clone, Serialize, Deserialize)]
    pub enum ClientMessage {
        Handshake(ClientHandshake),
        ControllerUpdate(u32, ControllerState),
    }

    #[derive(Clone, Serialize, Deserialize)]
    pub struct ClientHandshake {
        pub requested_players: u32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub enum ServerMessage {
        Handshake(ServerHandshake),
        Tick(Tick),
        AddRemove(AddRemove),
        UpdateScore(UpdateScore),
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ServerHandshake {
        pub objects: SmallVec<[(u32, (Drawable, ObjectInfo)); 128]>,
        pub scores: SmallVec<[u32; 8]>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Tick {
        pub time: f64,
        pub info: SmallVec<[(u32, ObjectInfo); 128]>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct AddRemove {
        pub new: SmallVec<[NewObject; 4]>,
        pub deleted: SmallVec<[u32; 4]>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct NewObject {
        pub id: u32,
        pub time: f64,
        pub drawable: Drawable,
        pub initial_info: ObjectInfo,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DeletedObject {
        pub id: u32,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UpdateScore {
        pub scores: SmallVec<[u32; 8]>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ObjectInfo {
        pub position: (f64, f64),
        pub velocity: (f64, f64),
        pub rotation: f64,
        pub rotation_speed: f64,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ControllerState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    fire: bool,
}

impl ControllerState {
    fn update(&mut self, key: GameKey, state: bool) {
        use GameKey::*;

        match key {
            Up => self.up = state,
            Down => self.down = state,
            Left => self.left = state,
            Right => self.right = state,
            Fire => self.fire = state,
        }
    }

    fn to_game_key(&self, ev: &ButtonArgs) -> Option<(bool, GameKey)> {
        use self::Button::*;

        let &ButtonArgs { state, button, .. } = ev;
        let is_down = state == ButtonState::Press;
        let key = match button {
            Keyboard(Key::W) => {
                if self.up == is_down {
                    return None;
                }

                GameKey::Up
            }
            Keyboard(Key::S) => {
                if self.down == is_down {
                    return None;
                }

                GameKey::Down
            }
            Keyboard(Key::A) => {
                if self.left == is_down {
                    return None;
                }

                GameKey::Left
            }
            Keyboard(Key::D) => {
                if self.right == is_down {
                    return None;
                }

                GameKey::Right
            }
            Keyboard(Key::Space) => {
                if self.fire == is_down {
                    return None;
                }

                GameKey::Fire
            }
            _ => return None,
        };

        let out = (is_down, key);

        Some(out)
    }

    fn is_firing(&self) -> bool {
        self.fire
    }

    fn desired_rotation(&self) -> f64 {
        (self.left as i8 - self.right as i8) as f64
    }

    fn desired_thrust(&self) -> f64 {
        (self.up as i8 - self.down as i8) as f64
    }
}

struct Bullet {
    shared: GameEntity,
}

#[derive(Clone)]
struct Player {
    controller: ControllerState,
    last_fire: f64,
    acceleration: Vector2<f64>,
    shared: GameEntity,
}

#[derive(Debug, Clone)]
struct GameEntity {
    entity_id: u32,
    owning_team: Team,
    position: Point2<f64>,
    velocity: Vector2<f64>,
    rotation: Rad<f64>,
}

impl Bullet {
    fn should_die(&self) -> bool {
        let p = self.shared.position;
        p.x < -1. || p.x > 1. || p.y < -1. || p.y > 1.
    }
}

impl Player {
    fn handle_edges(&mut self) {
        self.shared.position = wrap(self.shared.position)
    }
}

#[derive(Hash, Debug, Copy, Clone, PartialEq, Eq)]
enum EntityId {
    Player(usize),
    Bullet(usize),
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
enum GamePoly {
    Player,
    Bullet,
    Asteroid(usize),
}

impl From<GamePoly> for &'static [[f64; 2]] {
    fn from(poly: GamePoly) -> Self {
        use GamePoly::*;

        match poly {
            Player => &PLAYER_POLY,
            Bullet => &BULLET_POLY,
            Asteroid(id) => ASTEROIDS[id],
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
enum GameColor {
    Team(u8),
    Black,
    White,
}

impl From<GameColor> for [f32; 4] {
    fn from(color: GameColor) -> Self {
        use GameColor::*;

        match color {
            Team(team) => TEAM_COLORS[team as usize].clone(),
            Black => [0., 0., 0., 1.],
            White => [1.; 4],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawable {
    should_wrap: bool,
    color: GameColor,
    poly: GamePoly,
    scale: f64,
}

#[derive(Debug)]
struct ClientDrawable {
    position: Vector2<f64>,
    velocity: Vector2<f64>,
    rotation: Rad<f64>,
    rotation_speed: Rad<f64>,
    drawable: Drawable,
}

impl ClientDrawable {
    fn transformations(&self) -> SmallVec<[Matrix2d; 3]> {
        fn transformation_with_offset(
            position: Vector2<f64>,
            rotation: Rad<f64>,
            (x, y): (f64, f64),
        ) -> Matrix2d {
            [[1., 0., 0.], [0., 1., 0.]]
                .trans(position.x + x, position.y + y)
                .rot_rad(rotation.0)
        }

        let mut out = SmallVec::new();
        out.push(transformation_with_offset(
            self.position,
            self.rotation,
            (0., 0.),
        ));

        if self.drawable.should_wrap {
            const BOUND: f64 = 1.0 - PLAYER_SCALE;

            if self.position.x < -BOUND {
                out.push(transformation_with_offset(
                    self.position,
                    self.rotation,
                    (2., 0.),
                ));
            } else if self.position.x > BOUND {
                out.push(transformation_with_offset(
                    self.position,
                    self.rotation,
                    (-2., 0.),
                ));
            }

            if self.position.y < -BOUND {
                out.push(transformation_with_offset(
                    self.position,
                    self.rotation,
                    (0., 2.),
                ));
            } else if self.position.y > BOUND {
                out.push(transformation_with_offset(
                    self.position,
                    self.rotation,
                    (0., -2.),
                ));
            }
        }

        out
    }
}

trait Entity {
    fn drawable(&self) -> Drawable;

    fn radius(&self) -> f64;

    fn collides_with<T: Entity>(&self, other: &T) -> bool {
        self.entity().owning_team != other.entity().owning_team &&
            (self.entity().position - other.entity().position).magnitude2() <
                self.radius() * self.radius()
    }

    fn entity(&self) -> &GameEntity;
    fn entity_mut(&mut self) -> &mut GameEntity;

    #[inline(always)]
    fn max_speed() -> Option<f64> {
        None
    }

    #[inline(always)]
    fn acceleration(&self) -> Option<Vector2<f64>> {
        None
    }
}

impl Entity for Player {
    fn drawable(&self) -> Drawable {
        Drawable {
            should_wrap: true,
            color: GameColor::Team(self.shared.owning_team.0),
            poly: GamePoly::Player,
            scale: PLAYER_SCALE,
        }
    }

    // This is actually smaller than the radius of the actual bounding circle of the player, but we
    // prefer to give leeway to make it seem less bullshit
    fn radius(&self) -> f64 {
        PLAYER_SCALE * 1.5
    }

    fn max_speed() -> Option<f64> {
        Some(PLAYER_MAX_SPEED)
    }

    #[inline(always)]
    fn acceleration(&self) -> Option<Vector2<f64>> {
        Some(self.acceleration)
    }

    #[inline(always)]
    fn entity(&self) -> &GameEntity {
        &self.shared
    }

    #[inline(always)]
    fn entity_mut(&mut self) -> &mut GameEntity {
        &mut self.shared
    }
}

impl Entity for Bullet {
    fn drawable(&self) -> Drawable {
        Drawable {
            should_wrap: false,
            color: GameColor::Team(self.shared.owning_team.0),
            poly: GamePoly::Bullet,
            scale: BULLET_SCALE,
        }
    }

    fn radius(&self) -> f64 {
        2. * BULLET_SCALE
    }

    #[inline(always)]
    fn entity(&self) -> &GameEntity {
        &self.shared
    }

    #[inline(always)]
    fn entity_mut(&mut self) -> &mut GameEntity {
        &mut self.shared
    }
}

fn player(team: Team, id: u32) -> Player {
    Player {
        controller: Default::default(),
        last_fire: 0.,
        acceleration: Vector2 { x: 0., y: 0. },
        shared: GameEntity {
            entity_id: id,
            owning_team: team,
            position: Point2 { x: 0., y: 0. },
            velocity: Vector2 { x: 0., y: 0. },
            rotation: Rad(0.),
        },
    }
}

fn bullet(id: u32, player: &GameEntity) -> Bullet {
    let fire_norm = Basis2::from_angle(player.rotation).rotate_vector(Vector2::unit_y());
    let fire_vel = fire_norm * BULLET_SPEED;

    Bullet {
        shared: GameEntity {
            entity_id: id,
            owning_team: player.owning_team,
            position: wrap(player.position + fire_norm * PLAYER_SCALE),
            velocity: fire_vel,
            rotation: Rad(0.),
        },
    }
}

fn wrap(p: Point2<f64>) -> Point2<f64> {
    Point2 {
        x: ((p.x + 3.) % 2.) - 1.,
        y: ((p.y + 3.) % 2.) - 1.,
    }
}

static PLAYER_POLY: [[f64; 2]; 4] = [[0., 1.], [1., -1.], [0., -0.5], [-1., -1.]];
static BULLET_POLY: [[f64; 2]; 4] = [[0., 1.], [1., 0.], [0., -1.], [-1., 0.]];
static ASTEROIDS: &[&[[f64; 2]]] = &[
    &[
        [0.11, 1.24],
        [1.0, 0.96],
        [1.35, -0.17],
        [0.64, -0.64],
        [0.09, -1.05],
        [-0.7, -0.54],
        [-0.89, -0.11],
        [-0.53, 0.54],
    ],
    &[
        [0.02, 0.78],
        [0.93, 0.86],
        [1.15, -0.04],
        [0.7, -0.71],
        [0.19, -1.24],
        [-0.95, -0.85],
        [-1.06, -0.05],
        [-0.91, 0.71],
    ],
    &[
        [-0.04, 0.96],
        [0.57, 0.67],
        [0.88, 0.06],
        [0.83, -1.09],
        [0.12, -0.81],
        [-0.54, -0.54],
        [-0.61, 0.04],
        [-0.59, 0.78],
    ],
    &[
        [0.06, 1.02],
        [0.54, 0.45],
        [1.3, 0.04],
        [0.71, -0.79],
        [0.08, -1.39],
        [-0.71, -0.77],
        [-0.82, 0.05],
        [-0.89, 0.84],
    ],
    &[
        [0.15, 1.04],
        [0.71, 0.53],
        [0.92, 0.05],
        [0.65, -0.88],
        [-0.08, -1.07],
        [-0.8, -0.69],
        [-1.12, 0.01],
        [-1.02, 0.75],
    ],
];

const PLAYER_DEAD_TIME: f64 = 5.0;
const PLAYER_FIRE_INTERVAL: f64 = 0.2;
const PLAYER_ROTATION_SPEED: f64 = 5.0;
const PLAYER_THRUST_ACC: f64 = 2.0;
const PLAYER_MAX_SPEED: f64 = 0.6;
const PLAYER_SCALE: f64 = 0.05;

const BULLET_SPEED: f64 = 1.2;
const BULLET_SCALE: f64 = 0.02;

fn update_physics<E: Entity>(e: &mut E, dt: f64) {
    if let Some(acc) = e.acceleration() {
        e.entity_mut().velocity += acc * dt;
    }

    let ent = e.entity_mut();
    ent.position += ent.velocity * dt;

    if let Some(max) = E::max_speed() {
        if ent.velocity.magnitude() > max {
            ent.velocity = ent.velocity.normalize() * max;
        }
    }
}

fn render<G: Graphics>(e: &ClientDrawable, graphics: &mut G) {
    let iter = e.transformations();

    let Drawable { color, poly, scale, .. } = e.drawable;

    for trans in &iter {
        polygon(
            color.into(),
            poly.into(),
            trans.scale(scale, scale),
            graphics,
        );
    }
}

trait RetainTools<T> {
    fn retain_pair_return<F: FnMut(usize, &mut T) -> bool>(&mut self, f: F) -> &mut [T];
    fn retain_index<F: FnMut(usize) -> bool>(&mut self, mut f: F) -> &mut [T] {
        self.retain_pair_return(|i, _| f(i))
    }
    fn retain_return<F: FnMut(&mut T) -> bool>(&mut self, mut f: F) -> &mut [T] {
        self.retain_pair_return(|_, v| f(v))
    }
}

impl<T> RetainTools<T> for Vec<T> {
    fn retain_pair_return<F: FnMut(usize, &mut T) -> bool>(&mut self, mut f: F) -> &mut [T] {
        use std::slice;

        let len = self.len();
        let mut del = 0;

        {
            let v = &mut **self;

            for i in 0..len {
                if !f(i, &mut v[i]) {
                    del += 1;
                } else if del > 0 {
                    v.swap(i - del, i);
                }
            }
        }

        unsafe {
            self.set_len(len - del);

            let raw = self.as_mut_ptr();

            slice::from_raw_parts_mut(raw.offset((len - del) as _), del)
        }
    }
}

trait FilterUnzip<A, B>: Sized {
    fn filter_unzip<FromA, FromB>(self) -> (FromA, FromB)
    where
        FromA: Default + Extend<A>,
        FromB: Default + Extend<B>;
}

impl<A, B, I: Sized + Iterator<Item = (Option<A>, Option<B>)>> FilterUnzip<A, B> for I {
    fn filter_unzip<FromA, FromB>(self) -> (FromA, FromB)
    where
        FromA: Default + Extend<A>,
        FromB: Default + Extend<B>,
    {
        let mut ts: FromA = Default::default();
        let mut us: FromB = Default::default();

        for (t, u) in self {
            ts.extend(t);
            us.extend(u);
        }

        (ts, us)
    }
}

fn make_server<C, T>(location: T, num_teams: usize, client: C) -> !
where
    C: Into<Option<(Sender<packets::ServerMessage>, Receiver<packets::ClientMessage>)>>,
    T: ToSocketAddrs,
{
    use mio::net::TcpListener;
    use std::time::{Instant, Duration};

    type Remote = Box<packets::SendRecv<packets::ClientMessage, packets::ServerMessage>>;

    let mut controller_id_gen = IdGen(0);
    let mut pending: Vec<Option<Remote>> = vec![client.into().map(|client| Box::new(client) as _)];

    let mut controllers: HashMap<u32, Remote> = Default::default();

    let mut players: HashMap<(u32, u32), Player> = Default::default();

    let mut live_players: Vec<(u32, u32)> = vec![];
    let mut ent_id_gen = IdGen(0);
    let mut dead_players: Vec<(f64, (u32, u32))> = vec![];
    let mut team_scores = SmallVec::from_vec(vec![0u32; num_teams]);
    let mut bullets = vec![];
    let mut world_time = 0.;
    let mut new_object_messages = SmallVec::new();
    let mut deleted_object_messages = SmallVec::new();

    // The server always runs at a 60fps tickrate. If it slows down, the world slows down. This is
    // just how it will work for now
    let dt = 1. / 60.;

    let listener = TcpListener::bind(&location
        .to_socket_addrs()
        .expect("Invalid host or port")
        .next()
        .expect("No hosts")).expect("Server: couldn't bind to port");

    loop {
        let start = Instant::now();
        world_time += dt;

        {
            let to_spawn =
                dead_players.retain_return(|&mut (time, _)| world_time < time + PLAYER_DEAD_TIME);

            if !to_spawn.is_empty() {
                use rand::{Rng, StdRng};

                let mut rng = StdRng::new().expect("Can't access RNG");

                for &mut (_, ref mut player_id) in to_spawn {
                    let player: Option<Player> = players.get(player_id).cloned();
                    let new_player = if let Some(mut player) = player {
                        player.acceleration = Vector2 { x: 0., y: 0. };
                        player.shared.velocity = Vector2 { x: 0., y: 0. };
                        player.controller = Default::default();

                        'pos_loop: loop {
                            player.shared.position.x = rng.gen_range(-0.9, 0.9);
                            player.shared.position.y = rng.gen_range(-0.9, 0.9);

                            for player_id in &live_players {
                                if players
                                    .get(player_id)
                                    .expect("Player left! (FIXME)")
                                    .collides_with(&player)
                                {
                                    continue 'pos_loop;
                                }
                            }

                            break;
                        }

                        player
                    } else {
                        panic!("Player left! (FIXME)");
                    };

                    new_object_messages.push(packets::NewObject {
                        time: world_time,
                        id: new_player.shared.entity_id,
                        drawable: new_player.drawable(),
                        initial_info: packets::ObjectInfo {
                            position: (new_player.shared.position.x, new_player.shared.position.y),
                            velocity: (new_player.shared.velocity.x, new_player.shared.velocity.y),
                            rotation: new_player.shared.rotation.0,
                            rotation_speed: 0.,
                        },
                    });

                    *players.get_mut(player_id).unwrap() = new_player;

                    live_players.push(*player_id);
                }
            }
        }

        for player_id in &live_players {
            if let Some(player) = players.get_mut(&player_id) {
                {
                    let Player {
                        last_fire: ref mut last,
                        controller: ref mut c,
                        shared: ref mut e,
                        acceleration: ref mut acc,
                    } = *player;

                    e.rotation += Rad(c.desired_rotation() * dt) * PLAYER_ROTATION_SPEED;
                    *acc = Basis2::from_angle(e.rotation).rotate_vector(
                        Vector2::unit_y() * c.desired_thrust() *
                            PLAYER_THRUST_ACC,
                    );

                    if c.is_firing() && world_time > *last + PLAYER_FIRE_INTERVAL {
                        *last = world_time;

                        let new = bullet(ent_id_gen.next(), &e);

                        new_object_messages.push(packets::NewObject {
                            time: world_time,
                            id: new.shared.entity_id,
                            drawable: new.drawable(),
                            initial_info: packets::ObjectInfo {
                                position: (new.shared.position.x, new.shared.position.y),
                                velocity: (new.shared.velocity.x, new.shared.velocity.y),
                                rotation: new.shared.rotation.0,
                                rotation_speed: 0.,
                            },
                        });

                        bullets.push(bullet(ent_id_gen.next(), &e));
                    }
                }

                player.handle_edges();
            }
        }

        struct HitMessage {
            killer: Option<Team>,
            victim_id: EntityId,
        }

        let (killing_teams, dead_entities): (Vec<_>, HashSet<_>) = live_players
            .iter()
            .enumerate()
            .cartesian_product(live_players.iter().enumerate())
            .filter_map(|((ai, a_id), (bi, b_id))| {
                players
                    .get(a_id)
                    .and_then(|a| players.get(b_id).map(|b| (a, b)))
                    .and_then(|(a, b)| if a.shared.entity_id != b.shared.entity_id &&
                        a.collides_with(b)
                    {
                        Some((ai, bi))
                    } else {
                        None
                    })
            })
            .flat_map(|(a, b)| {
                // Hack because fixed-size slices have no ownership-taking iteration methods
                let mut out: SmallVec<[_; 2]> = SmallVec::new();

                out.push(HitMessage {
                    killer: None,
                    victim_id: EntityId::Player(a),
                });
                out.push(HitMessage {
                    killer: None,
                    victim_id: EntityId::Player(b),
                });

                out
            })
            .chain(
                live_players
                    .iter()
                    .enumerate()
                    .filter_map(|(i, id)| players.get(id).map(|p| (i, p)))
                    .cartesian_product(bullets.iter().enumerate())
                    .filter_map(|((pi, p), (bi, b))| if p.collides_with(b) {
                        Some((pi, (bi, b.shared.owning_team)))
                    } else {
                        None
                    })
                    .flat_map(|(a, (b, team))| {
                        // Hack because fixed-size slices have no ownership-taking iteration methods
                        let mut out: SmallVec<[_; 2]> = SmallVec::new();

                        out.push(HitMessage {
                            killer: Some(team),
                            victim_id: EntityId::Player(a),
                        });
                        out.push(HitMessage {
                            killer: None,
                            victim_id: EntityId::Bullet(b),
                        });

                        out
                    }),
            )
            .map(|HitMessage { killer, victim_id }| (killer, Some(victim_id)))
            .filter_unzip();

        for killer in &killing_teams {
            team_scores[killer.0 as usize] += 1;
        }

        if !killing_teams.is_empty() {
            let msg = packets::ServerMessage::UpdateScore(
                packets::UpdateScore { scores: team_scores.clone() },
            );

            for (_, controller) in controllers.iter_mut() {
                controller.send_high(&msg).expect("Send failed");
            }
        }

        if !dead_entities.is_empty() {
            deleted_object_messages.extend(dead_entities.iter().map(|dead| match *dead {
                EntityId::Player(i) => players[&live_players[i]].shared.entity_id,
                EntityId::Bullet(i) => bullets[i].shared.entity_id,
            }));

            let len_before = dead_players.len();
            dead_players.extend(
                live_players
                    .retain_index(|i| !dead_entities.contains(&EntityId::Player(i)))
                    .into_iter()
                    .map(|live| (world_time, *live)),
            );
            let len_after = dead_players.len();

            // This check is basically free and the overwhelming majority-case is that only
            // bullets have died.
            if len_before != len_after {
                dead_players.sort_unstable_by(|&(time_a, _), &(time_b, _)| {
                    time_a.partial_cmp(&time_b).unwrap_or(
                        std::cmp::Ordering::Equal,
                    )
                });
            }

            bullets.retain_index(|i| !dead_entities.contains(&EntityId::Bullet(i)));
        }

        for player_id in &live_players {
            if let Some(player) = players.get_mut(&player_id) {
                update_physics(player, dt);
            }
        }

        for bullet in &mut bullets {
            update_physics(bullet, dt);
        }

        bullets.retain(|e| !e.should_die());

        while let Ok((stream, _)) = listener.accept() {
            let _ = stream.set_nodelay(true);
            pending.push(Some(Box::new(packets::PrioSocket::from_tcp(stream).expect(
                "Couldn't connect back",
            ))));
        }

        for (id, c) in controllers.iter_mut() {
            while let Ok(packets::ClientMessage::ControllerUpdate(local_id, update)) =
                c.try_recv()
            {
                if let Some(val) = players.get_mut(&(*id, local_id)) {
                    val.controller = update;
                }
            }
        }

        let mut changed = false;
        let mut cached_handshake = None;
        for pending in &mut pending {
            if let Some(Ok(packets::ClientMessage::Handshake(handshake))) =
                pending.as_mut().map(|p| p.try_recv())
            {
                changed = true;

                let srv_handshake = if let Some(ref hsk) = cached_handshake {
                    hsk
                } else {
                    cached_handshake = Some(packets::ServerMessage::Handshake(
                        packets::ServerHandshake {
                            objects: live_players
                                .iter()
                                .filter_map(|pid| players.get(pid))
                                .map(|player| {
                                    (player.shared.entity_id, (
                                        player.drawable(),
                                        packets::ObjectInfo {
                                            position: (
                                                player.shared.position.x,
                                                player.shared.position.y,
                                            ),
                                            velocity: (
                                                player.shared.velocity.x,
                                                player.shared.velocity.y,
                                            ),
                                            rotation: player.shared.rotation.0,
                                            rotation_speed: 0.,
                                        },
                                    ))
                                })
                                .chain(bullets.iter().map(|bullet| {
                                    (bullet.shared.entity_id, (
                                        bullet.drawable(),
                                        packets::ObjectInfo {
                                            position: (
                                                bullet.shared.position.x,
                                                bullet.shared.position.y,
                                            ),
                                            velocity: (
                                                bullet.shared.velocity.x,
                                                bullet.shared.velocity.y,
                                            ),
                                            rotation: bullet.shared.rotation.0,
                                            rotation_speed: 0.,
                                        },
                                    ))
                                }))
                                .collect(),
                            scores: team_scores.clone(),
                        },
                    ));

                    cached_handshake.as_ref().unwrap()
                };

                let new_id = controller_id_gen.next();
                let mut pending = pending.take().unwrap();

                pending.send_high(srv_handshake).expect("Send failed");
                controllers.insert(new_id, pending);

                let mut team_sizes = HashMap::new();

                for i in 0..num_teams as u8 {
                    team_sizes.insert(i, 0);
                }

                for (_, player) in &players {
                    *team_sizes.entry(player.shared.owning_team.0).or_insert(0) += 1;
                }

                for i in 0..handshake.requested_players {
                    let smallest_team = team_sizes
                        .iter()
                        .min_by_key(|&(_, size)| size)
                        .map(|(t, _)| Team(*t))
                        .unwrap_or(Team(0));

                    *team_sizes.entry(smallest_team.0).or_insert(0) += 1;

                    players.insert((new_id, i), player(smallest_team, ent_id_gen.next()));
                    dead_players.push((std::f64::NEG_INFINITY, (new_id, i)));
                }
            }
        }

        if changed {
            pending.retain(|f| f.is_some());
        }

        if !live_players.is_empty() {
            use std::mem;

            let add_remove_msg = {
                let msg = packets::AddRemove {
                    new: mem::replace(&mut new_object_messages, Default::default()),
                    deleted: mem::replace(&mut deleted_object_messages, Default::default()),
                };

                if msg.new.is_empty() && msg.deleted.is_empty() {
                    None
                } else {
                    Some(packets::ServerMessage::AddRemove(msg))
                }
            };

            let tick_msg = packets::ServerMessage::Tick(packets::Tick {
                time: world_time,
                info: live_players
                    .iter()
                    .filter_map(|pid| players.get(pid))
                    .map(|player| {
                        (
                            player.shared.entity_id,
                            packets::ObjectInfo {
                                position: (player.shared.position.x, player.shared.position.y),
                                velocity: (player.shared.velocity.x, player.shared.velocity.y),
                                rotation: player.shared.rotation.0,
                                rotation_speed: 0.,
                            },
                        )
                    })
                    .chain(bullets.iter().map(|bullet| {
                        (
                            bullet.shared.entity_id,
                            packets::ObjectInfo {
                                position: (bullet.shared.position.x, bullet.shared.position.y),
                                velocity: (bullet.shared.velocity.x, bullet.shared.velocity.y),
                                rotation: bullet.shared.rotation.0,
                                rotation_speed: 0.,
                            },
                        )
                    }))
                    .collect(),
            });

            for (_, controller) in controllers.iter_mut() {
                controller.send_low(&tick_msg).expect("Send failed");

                if let Some(ref msg) = add_remove_msg {
                    controller.send_high(msg).expect("Send failed");
                }
            }
        }

        if let Some(remaining_time) =
            Duration::new(0, (1_000_000_000. * dt) as _).checked_sub(Instant::now() - start)
        {
            std::thread::sleep(remaining_time);
        }
    }
}

fn make_remote_client<T: ToSocketAddrs>(num_local_players: usize, remote: T) {
    use mio::net::TcpStream;

    let remote_addr = remote
        .to_socket_addrs()
        .ok()
        .and_then(|mut i| i.next())
        .expect("Remote address incorrectly specified");
    let remote = packets::PrioSocket::connect(&remote_addr).expect("Couldn't reach remote server");
    make_client(num_local_players, remote);
}

fn make_client<
    T: packets::PrioSend<packets::ClientMessage>
        + packets::Receiver<packets::ServerMessage>,
>(
    num_local_players: usize,
    mut remote: T,
) {
    let mut window: PistonWindow = WindowSettings::new("Not Asteroids", [640, 640])
        .exit_on_esc(true)
        .vsync(true)
        .build()
        .unwrap();

    let mut glyphs = Glyphs::from_font(
        rusttype::FontCollection::from_bytes(DEJAVU_SANS_MONO)
            .into_font()
            .expect("Parsing DejaVu Sans Mono failed"),
        window.factory.clone(),
        TextureSettings::new(),
    );

    let mut controllers: Vec<ControllerState> = vec![Default::default(); num_local_players];
    let mut entities: HashMap<u32, ClientDrawable> = HashMap::new();
    let mut team_scores: SmallVec<[u32; 8]> = SmallVec::new();

    remote
        .send_high(&packets::ClientMessage::Handshake(
            packets::ClientHandshake {
                requested_players: num_local_players as _,
            },
        ))
        .expect("Couldn't send");

    let mut new: SmallVec<[packets::NewObject; 4]> = SmallVec::new();

    while let Some(event) = window.next() {
        if let Some(UpdateArgs { dt }) = event.update_args() {
            for (_, ent) in &mut entities {
                ent.position += ent.velocity * dt;
                ent.rotation += ent.rotation_speed * dt;
            }

            for (id, state) in controllers.iter().enumerate() {
                let _ = remote.send_low(&packets::ClientMessage::ControllerUpdate(
                    id as _,
                    state.clone(),
                ));
            }
        } else if let Some(button_args) = event.button_args() {
            for controller in controllers.iter_mut() {
                if let Some((is_down, game_key)) = controller.to_game_key(&button_args) {
                    controller.update(game_key, is_down);
                }
            }
        } else if let Some(RenderArgs { width, height, .. }) = event.render_args() {
            window.draw_2d(&event, |c, graphics| {
                clear([0., 0., 0., 1.], graphics);

                for (_, drawable) in &entities {
                    render(drawable, graphics);
                }

                let to_text_space = |x, y| {
                    (
                        (x + 1.) * (width as f64 / 2.),
                        (y + 1.) * (height as f64 / 2.),
                    )
                };

                let min_text_x = -0.93;
                let max_text_x = 0.4;
                let text_y = -0.9;
                for (i, (score, color)) in team_scores.iter().zip(TEAM_COLORS.iter()).enumerate() {
                    let (x, y) = to_text_space(
                        lerp(min_text_x, max_text_x, i as f64 / team_scores.len() as f64),
                        text_y,
                    );

                    let trans = c.transform.trans(x, y);

                    text::Text::new_color(color.clone(), 32).draw(
                        &format!("{}", score),
                        &mut glyphs,
                        &c.draw_state,
                        trans,
                        graphics,
                    );
                }
            });
        }

        let mut tick: Option<packets::Tick> = None;
        let mut deleted: SmallVec<[u32; 4]> = SmallVec::new();

        while let Ok(msg) = remote.try_recv() {
            use packets::ServerMessage::*;

            match msg {
                Handshake(msg) => {
                    for (id, (vis, info)) in msg.objects {
                        entities.insert(
                            id,
                            ClientDrawable {
                                position: Vector2 {
                                    x: info.position.0,
                                    y: info.position.1,
                                },
                                velocity: Vector2 {
                                    x: info.velocity.0,
                                    y: info.velocity.1,
                                },
                                rotation: Rad(info.rotation),
                                rotation_speed: Rad(info.rotation_speed),
                                drawable: vis,
                            },
                        );
                    }

                    team_scores = msg.scores;
                }
                AddRemove(msg) => {
                    new.extend(msg.new);
                    deleted.extend(msg.deleted);
                }
                Tick(msg) => {
                    tick = Some(msg);
                }
                UpdateScore(msg) => team_scores = msg.scores,
            }
        }

        for id in deleted {
            entities.remove(&id);
        }

        if let Some(msg) = tick {
            for (id, info) in msg.info {
                if let Some(ent) = entities.get_mut(&id) {
                    ent.position.x = info.position.0;
                    ent.position.y = info.position.1;
                    ent.velocity.x = info.velocity.0;
                    ent.velocity.y = info.velocity.1;
                    ent.rotation = Rad(info.rotation);
                    ent.rotation_speed = Rad(info.rotation_speed);
                }
            }

            for new in new.drain() {
                let pos = Vector2 {
                    x: new.initial_info.position.0,
                    y: new.initial_info.position.1,
                };
                let vel = Vector2 {
                    x: new.initial_info.velocity.0,
                    y: new.initial_info.velocity.1,
                };

                entities.insert(
                    new.id,
                    ClientDrawable {
                        position: pos + vel * (msg.time - new.time),
                        velocity: vel,
                        rotation: Rad(new.initial_info.rotation),
                        rotation_speed: Rad(new.initial_info.rotation_speed),
                        drawable: new.drawable,
                    },
                );
            }
        }
    }
}

fn main() {
    use std::env;
    use getopts::Options;

    let args = env::args().collect::<Vec<_>>();
    let mut options = Options::new();
    options
        .optflag("d", "dedicated", "Start the server in dedicated mode")
        .optopt("s", "serve", "Start a listen server", "SERVER_LOCATION")
        .optopt(
            "c",
            "connect",
            "Connect to a remote server",
            "REMOTE_SERVER",
        )
        .optopt("t", "teams", "Number of teams (for the server)", "TEAMS")
        .optopt(
            "p",
            "players",
            "Number of players on this client (currently ignored and set to 1)",
            "PLAYERS",
        );

    let matches = options.parse(&args[1..]).expect(
        "Can't parse CLI arguments",
    );

    let remote_server = matches.opt_str("connect");

    let num_teams = matches
        .opt_str("teams")
        .map(|teams| teams.parse().expect("Invalid value for teams"))
        .unwrap_or(2);

    remote_server
        .map(|srv| make_remote_client(1, srv))
        .unwrap_or_else(|| {
            let client = if matches.opt_present("dedicated") {
                None
            } else {
                let (to_server, from_client) = mpsc::channel();
                let (to_client, from_server) = mpsc::channel();
                std::thread::spawn(|| make_client(1, (to_server, from_server)));
                Some((to_client, from_client))
            };

            make_server(
                matches
                    .opt_str("serve")
                    .as_ref()
                    .map(|s| s as &str)
                    .unwrap_or(DEFAULT_SERVER_LOCATION),
                num_teams,
                client,
            );
        });
}
