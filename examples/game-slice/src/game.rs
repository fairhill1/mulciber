//! Minimal game state, input snapshot, collision, and win/reset loop.

use glam::Vec2;
use mulciber_platform::{ButtonState, InputEvent, KeyCode};

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

#[derive(Default)]
struct Controls {
    held: [bool; 4],
}

impl Controls {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn direction(&self) -> Vec2 {
        Vec2::new(
            f32::from(self.held[1]) - f32::from(self.held[0]),
            f32::from(self.held[3]) - f32::from(self.held[2]),
        )
        .normalize_or_zero()
    }

    fn set(&mut self, key: KeyCode, held: bool) {
        let index = match key {
            KeyCode::KeyA | KeyCode::ArrowLeft => Some(0),
            KeyCode::KeyD | KeyCode::ArrowRight => Some(1),
            KeyCode::KeyW | KeyCode::ArrowUp => Some(2),
            KeyCode::KeyS | KeyCode::ArrowDown => Some(3),
            _ => None,
        };
        if let Some(index) = index {
            self.held[index] = held;
        }
    }
}

pub struct Game {
    controls: Controls,
    player: Vec2,
    facing: Vec2,
    active_pickups: [bool; PICKUPS.len()],
    collected: usize,
    hits: usize,
    won: bool,
    last_update_seconds: Option<f32>,
    world_seconds: f32,
}

impl Default for Game {
    fn default() -> Self {
        Self {
            controls: Controls::default(),
            player: Vec2::new(-7.0, -7.0),
            facing: Vec2::new(0.0, -1.0),
            active_pickups: [true; PICKUPS.len()],
            collected: 0,
            hits: 0,
            won: false,
            last_update_seconds: None,
            world_seconds: 0.0,
        }
    }
}

impl Game {
    pub fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::FocusChanged { focused: false } => self.controls.clear(),
            InputEvent::Keyboard {
                key, state, repeat, ..
            } => {
                let held = state == ButtonState::Pressed;
                self.controls.set(key, held);
                if key == KeyCode::KeyR && held && !repeat {
                    self.reset();
                    println!("forge run: reset");
                }
            }
            _ => {}
        }
    }

    pub fn update(&mut self, seconds: f32) {
        let delta = self
            .last_update_seconds
            .replace(seconds)
            .map_or(0.0, |previous| (seconds - previous).clamp(0.0, 0.05));
        self.world_seconds += delta;

        let direction = self.controls.direction();
        if direction != Vec2::ZERO {
            self.facing = direction;
            self.move_player(direction * PLAYER_SPEED * delta);
        }
        self.collect_pickups();
        self.check_hazards();
    }

    pub const fn player(&self) -> Vec2 {
        self.player
    }

    pub const fn facing(&self) -> Vec2 {
        self.facing
    }

    pub const fn world_seconds(&self) -> f32 {
        self.world_seconds
    }

    pub fn active_pickups(&self) -> impl Iterator<Item = Vec2> + '_ {
        PICKUPS
            .iter()
            .zip(self.active_pickups)
            .filter_map(|(&position, active)| active.then_some(Vec2::from_array(position)))
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn hazards(&self) -> [Vec2; 4] {
        std::array::from_fn(|index| {
            let phase = index as f32 * std::f32::consts::FRAC_PI_2;
            let angle = self.world_seconds * (0.72 + index as f32 * 0.08) + phase;
            let radius = if index % 2 == 0 { 5.0 } else { 2.8 };
            Vec2::new(angle.cos(), angle.sin()) * radius
        })
    }

    fn move_player(&mut self, movement: Vec2) {
        let candidate = (self.player + movement).clamp(
            Vec2::splat(-ARENA_HALF_EXTENT),
            Vec2::splat(ARENA_HALF_EXTENT),
        );
        let horizontal = Vec2::new(candidate.x, self.player.y);
        if !blocked(horizontal) {
            self.player.x = horizontal.x;
        }
        let vertical = Vec2::new(self.player.x, candidate.y);
        if !blocked(vertical) {
            self.player.y = vertical.y;
        }
    }

    fn collect_pickups(&mut self) {
        for (index, position) in PICKUPS.iter().enumerate() {
            if self.active_pickups[index]
                && self.player.distance_squared(Vec2::from_array(*position))
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
        if self
            .hazards()
            .iter()
            .any(|hazard| self.player.distance_squared(*hazard) < HAZARD_RADIUS * HAZARD_RADIUS)
        {
            self.player = Vec2::new(-7.0, -7.0);
            self.controls.clear();
            self.hits += 1;
            println!("forge run: struck by a sentry (hits: {})", self.hits);
        }
    }

    fn reset(&mut self) {
        let last_update_seconds = self.last_update_seconds;
        *self = Self::default();
        self.last_update_seconds = last_update_seconds;
    }
}

fn blocked(position: Vec2) -> bool {
    OBSTACLES.iter().any(|&obstacle| {
        position.distance_squared(Vec2::from_array(obstacle))
            < (PLAYER_RADIUS + OBSTACLE_RADIUS) * (PLAYER_RADIUS + OBSTACLE_RADIUS)
    })
}

#[cfg(test)]
mod tests {
    use super::{Game, PICKUPS};
    use glam::Vec2;

    #[test]
    fn collecting_a_crystal_removes_it_from_the_render_set() {
        let mut game = Game {
            player: Vec2::from_array(PICKUPS[0]),
            ..Game::default()
        };
        game.update(0.0);
        assert_eq!(game.active_pickups().count(), PICKUPS.len() - 1);
    }

    #[test]
    fn reset_restores_every_crystal() {
        let mut game = Game {
            player: Vec2::from_array(PICKUPS[0]),
            ..Game::default()
        };
        game.update(0.0);
        game.reset();
        assert_eq!(game.active_pickups().count(), PICKUPS.len());
    }
}
