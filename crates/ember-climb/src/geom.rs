//! Small vector math in world space. Everything is pure, so every number can be tested without
//! the game. Float arithmetic cannot panic.

use std::ops::{Add, Mul, Neg, Sub};

/// A point or a direction in world space, in metres.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    /// East-west.
    pub x: f32,
    /// Up. The game's up is always (0, 1, 0) (`FUN_14023c080`).
    pub y: f32,
    /// North-south.
    pub z: f32,
}

/// World up.
pub const UP: Vec3 = Vec3::new(0.0, 1.0, 0.0);

/// The shortest direction [`Vec3::normalized`] accepts, in metres.
const SHORTEST: f32 = 1e-6;

impl Vec3 {
    /// The vector with these parts.
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// The dot product.
    pub fn dot(self, other: Vec3) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    /// The cross product.
    pub fn cross(self, other: Vec3) -> Vec3 {
        Vec3::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    /// The length.
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// This direction at length 1, or `None` when it is too short (or not finite) to have one.
    pub fn normalized(self) -> Option<Vec3> {
        let length = self.length();
        (length.is_finite() && length > SHORTEST).then(|| self * (1.0 / length))
    }

    /// Are all three parts finite numbers?
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    /// The same direction with its up part removed.
    pub fn flat(self) -> Vec3 {
        Vec3::new(self.x, 0.0, self.z)
    }
}

impl Add for Vec3 {
    type Output = Vec3;

    fn add(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;

    fn sub(self, other: Vec3) -> Vec3 {
        Vec3::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;

    fn mul(self, scale: f32) -> Vec3 {
        Vec3::new(self.x * scale, self.y * scale, self.z * scale)
    }
}

impl Neg for Vec3 {
    type Output = Vec3;

    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// Where world up points after the rotation `[x, y, z, w]`, as the game turns a vector
/// (`FUN_14021ad10`).
pub fn up_of([x, y, z, w]: [f32; 4]) -> Vec3 {
    Vec3::new(
        2.0 * (x * y - w * z),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + w * x),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The game's `FUN_14021ad10`, step for step: `v + 2 * cross(q, cross(q, v) + w * v)`.
    fn game_rotate([x, y, z, w]: [f32; 4], v: Vec3) -> Vec3 {
        let axis = Vec3::new(x, y, z);
        v + axis.cross(axis.cross(v) + v * w) * 2.0
    }

    /// No rotation leaves up alone.
    #[test]
    fn no_rotation_keeps_up() {
        assert_eq!(up_of([0.0, 0.0, 0.0, 1.0]), UP);
    }

    /// A turn around the up axis leaves up alone, whatever the angle.
    #[test]
    fn a_turn_around_up_keeps_up() {
        let half = 0.6f32;
        let up = up_of([0.0, half.sin(), 0.0, half.cos()]);
        assert!((up - UP).length() < 1e-6, "{up:?}");
    }

    /// Up follows the game's own rotation, whatever the rotation.
    #[test]
    fn up_follows_the_games_rotation() {
        for q in [
            [0.1, 0.2, 0.3, 0.927_362],
            [-0.5, 0.5, -0.5, 0.5],
            [0.7, 0.0, 0.1, 0.706_930_6],
            [0.0, -0.3, 0.8, 0.519_615_2],
        ] {
            let difference = up_of(q) - game_rotate(q, UP);
            assert!(difference.length() < 1e-6, "{q:?}");
        }
    }

    /// The cross product of two axes is the third, with the game's right-handed sign.
    #[test]
    fn cross_of_axes_is_the_third_axis() {
        let x = Vec3::new(1.0, 0.0, 0.0);
        let z = Vec3::new(0.0, 0.0, 1.0);
        assert_eq!(UP.cross(x), Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(x.cross(UP), z);
    }

    /// A direction keeps its heading and gets length 1.
    #[test]
    fn normalized_keeps_the_heading() {
        let direction = Vec3::new(3.0, 0.0, 4.0).normalized().unwrap();
        assert!((direction.length() - 1.0).abs() < 1e-6);
        assert!((direction.x - 0.6).abs() < 1e-6);
    }

    /// Nothing can be made of a zero or a broken direction, and that must not divide by zero.
    #[test]
    fn normalized_refuses_what_has_no_direction() {
        assert_eq!(Vec3::default().normalized(), None);
        assert_eq!(Vec3::new(f32::NAN, 0.0, 0.0).normalized(), None);
        assert_eq!(Vec3::new(f32::INFINITY, 0.0, 0.0).normalized(), None);
    }

    /// `is_finite` looks at every part.
    #[test]
    fn is_finite_looks_at_every_part() {
        assert!(UP.is_finite());
        assert!(!Vec3::new(0.0, f32::NAN, 0.0).is_finite());
        assert!(!Vec3::new(0.0, 0.0, f32::INFINITY).is_finite());
    }

    /// `flat` removes only the up part.
    #[test]
    fn flat_removes_only_up() {
        assert_eq!(Vec3::new(1.0, 2.0, 3.0).flat(), Vec3::new(1.0, 0.0, 3.0));
    }
}
