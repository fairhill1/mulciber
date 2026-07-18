//! Game state equivalent to the Mulciber and wgpu Forge Run slices.

use glam::Vec2;

use crate::{InputState, Key};

const ARENA_HALF_EXTENT: f32 = 8.5;
const PLAYER_SPEED: f32 = 5.2;
const PLAYER_RADIUS: f32 = 0.45;
const OBSTACLE_RADIUS: f32 = 0.8;
const PICKUP_RADIUS: f32 = 0.72;
const HAZARD_RADIUS: f32 = 0.8;

pub const OBSTACLES: [[f32; 2]; 12] = [
    [-5.5, -4.0],
    [-2.5, -4.0],
    [2.5, -4.0],
    [5.5, -4.0],
    [-4.0, -0.5],
    [0.0, -0.5],
    [4.0, -0.5],
    [-5.5, 3.5],
    [-2.0, 3.5],
    [2.0, 3.5],
    [5.5, 3.5],
    [0.0, 6.5],
];

pub const PICKUPS: [[f32; 2]; 8] = [
    [-7.0, -1.5],
    [-4.0, -6.5],
    [0.0, -6.5],
    [4.0, -6.5],
    [7.0, 1.0],
    [4.0, 6.5],
    [-4.0, 6.5],
    [-7.0, 5.0],
];

#[derive(Clone, Copy)]
struct SimulationState {
    player: Vec2,
    facing: Vec2,
    world_seconds: f32,
}

impl Default for SimulationState {
    fn default() -> Self {
        Self {
            player: Vec2::new(-7.0, -7.0),
            facing: Vec2::new(0.0, -1.0),
            world_seconds: 0.0,
        }
    }
}

pub(crate) struct RenderState {
    pub(crate) player: Vec2,
    pub(crate) facing: Vec2,
    pub(crate) world_seconds: f32,
    pub(crate) visual_seconds: f32,
}

pub struct Game {
    previous: SimulationState,
    current: SimulationState,
    active_pickups: [bool; PICKUPS.len()],
    collected: usize,
    hits: usize,
    won: bool,
    visual_seconds: f32,
}

impl Default for Game {
    fn default() -> Self {
        let state = SimulationState::default();
        Self {
            previous: state,
            current: state,
            active_pickups: [true; PICKUPS.len()],
            collected: 0,
            hits: 0,
            won: false,
            visual_seconds: 0.0,
        }
    }
}

impl Game {
    pub fn handle_frame_input(&mut self, input: &InputState) {
        if input.key_pressed(Key::R) {
            *self = Self::default();
            println!("forge run: reset");
        }
    }

    pub fn fixed_update(&mut self, input: &InputState, delta_seconds: f32) {
        self.previous = self.current;
        self.current.world_seconds += delta_seconds;
        let direction = direction(input);
        if direction != Vec2::ZERO {
            self.current.facing = direction;
            self.move_player(direction * PLAYER_SPEED * delta_seconds);
        }
        self.collect_pickups();
        self.check_hazards();
    }

    pub fn variable_update(&mut self, delta_seconds: f32) {
        self.visual_seconds += delta_seconds;
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn render_state(&self, interpolation: f64) -> RenderState {
        let interpolation = interpolation as f32;
        let facing = self
            .previous
            .facing
            .lerp(self.current.facing, interpolation)
            .normalize_or_zero();
        RenderState {
            player: self
                .previous
                .player
                .lerp(self.current.player, interpolation),
            facing: if facing == Vec2::ZERO {
                self.current.facing
            } else {
                facing
            },
            world_seconds: self.previous.world_seconds
                + (self.current.world_seconds - self.previous.world_seconds) * interpolation,
            visual_seconds: self.visual_seconds,
        }
    }

    pub fn active_pickups(&self) -> impl Iterator<Item = Vec2> + '_ {
        PICKUPS
            .iter()
            .zip(self.active_pickups)
            .filter_map(|(&position, active)| active.then_some(Vec2::from_array(position)))
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn hazards_at(world_seconds: f32) -> [Vec2; 4] {
        std::array::from_fn(|index| {
            let phase = index as f32 * std::f32::consts::FRAC_PI_2;
            let angle = world_seconds * (0.72 + index as f32 * 0.08) + phase;
            let radius = if index % 2 == 0 { 5.0 } else { 2.8 };
            Vec2::new(angle.cos(), angle.sin()) * radius
        })
    }

    fn move_player(&mut self, movement: Vec2) {
        let candidate = (self.current.player + movement).clamp(
            Vec2::splat(-ARENA_HALF_EXTENT),
            Vec2::splat(ARENA_HALF_EXTENT),
        );
        let horizontal = Vec2::new(candidate.x, self.current.player.y);
        if !blocked(horizontal) {
            self.current.player.x = horizontal.x;
        }
        let vertical = Vec2::new(self.current.player.x, candidate.y);
        if !blocked(vertical) {
            self.current.player.y = vertical.y;
        }
    }

    fn collect_pickups(&mut self) {
        for (index, position) in PICKUPS.iter().enumerate() {
            if self.active_pickups[index]
                && self
                    .current
                    .player
                    .distance_squared(Vec2::from_array(*position))
                    < PICKUP_RADIUS * PICKUP_RADIUS
            {
                self.active_pickups[index] = false;
                self.collected += 1;
                println!("forge run: crystal {}/{}", self.collected, PICKUPS.len());
            }
        }
        if self.collected == PICKUPS.len() && !self.won {
            self.won = true;
            println!("forge run: all crystals recovered — press R to run again");
        }
    }

    fn check_hazards(&mut self) {
        if Self::hazards_at(self.current.world_seconds)
            .iter()
            .any(|hazard| self.current.player.distance_squared(*hazard) < HAZARD_RADIUS.powi(2))
        {
            self.current.player = Vec2::new(-7.0, -7.0);
            self.previous.player = self.current.player;
            self.hits += 1;
            println!("forge run: struck by a sentry (hits: {})", self.hits);
        }
    }
}

fn direction(input: &InputState) -> Vec2 {
    Vec2::new(
        f32::from(input.key_held(Key::D) || input.key_held(Key::Right))
            - f32::from(input.key_held(Key::A) || input.key_held(Key::Left)),
        f32::from(input.key_held(Key::S) || input.key_held(Key::Down))
            - f32::from(input.key_held(Key::W) || input.key_held(Key::Up)),
    )
    .normalize_or_zero()
}

fn blocked(position: Vec2) -> bool {
    OBSTACLES.iter().any(|&obstacle| {
        position.distance_squared(Vec2::from_array(obstacle))
            < (PLAYER_RADIUS + OBSTACLE_RADIUS).powi(2)
    })
}

#[cfg(test)]
mod tests {
    use glam::Vec2;

    use super::{Game, PICKUPS};
    use crate::{InputState, Key};

    #[test]
    fn collecting_a_crystal_removes_it_from_the_render_set() {
        let mut game = Game {
            previous: super::SimulationState {
                player: Vec2::from_array(PICKUPS[0]),
                ..super::SimulationState::default()
            },
            current: super::SimulationState {
                player: Vec2::from_array(PICKUPS[0]),
                ..super::SimulationState::default()
            },
            ..Game::default()
        };
        game.fixed_update(&InputState::default(), 0.0);
        assert_eq!(game.active_pickups().count(), PICKUPS.len() - 1);
    }

    #[test]
    fn reset_restores_every_crystal() {
        let mut game = Game {
            previous: super::SimulationState {
                player: Vec2::from_array(PICKUPS[0]),
                ..super::SimulationState::default()
            },
            current: super::SimulationState {
                player: Vec2::from_array(PICKUPS[0]),
                ..super::SimulationState::default()
            },
            ..Game::default()
        };
        game.fixed_update(&InputState::default(), 0.0);
        let mut input = InputState::default();
        input.key_event(Key::R, true);
        game.handle_frame_input(&input);
        assert_eq!(game.active_pickups().count(), PICKUPS.len());
    }
}
