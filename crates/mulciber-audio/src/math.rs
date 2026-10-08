//! The handful of vector operations the mixer and the room probe need, on plain arrays so the
//! public API does not tie callers to one math crate.

pub(crate) type Vec3 = [f32; 3];

pub(crate) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

/// `a` scaled to unit length, or `fallback` when it is too short or not finite to have a
/// direction.
pub(crate) fn normalize_or(a: Vec3, fallback: Vec3) -> Vec3 {
    let length = length(a);
    if length.is_finite() && length > 1.0e-6 {
        scale(a, 1.0 / length)
    } else {
        fallback
    }
}
