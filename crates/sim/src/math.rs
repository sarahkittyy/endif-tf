//! Float math that mirrors Source's `mathlib` closely enough to be a faithful port,
//! while staying bit-for-bit deterministic across x86_64, aarch64 and wasm32.
//!
//! Determinism rules for everything in the `sim` crate:
//! * only `f32`/`f64` `+ - * /` and `sqrt` (all IEEE-754 correctly rounded on every target),
//! * transcendental functions come from the pure-Rust `libm` crate, never from `std`,
//! * never use `mul_add` (FMA), `powf`, `exp`, `ln`, or `std` trig,
//! * no `HashMap` iteration, no pointer-address ordering, no wall-clock time.

use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::ops::{Add, AddAssign, Div, Mul, MulAssign, Neg, Sub, SubAssign};

/// `M_PI_F` from Source: `(float)M_PI`.
pub const PI_F: f32 = std::f64::consts::PI as f32;

/// `DEG2RAD(x)` from Source: `(float)(x) * (float)(M_PI_F / 180.f)`.
#[inline]
pub fn deg2rad(x: f32) -> f32 {
    x * (PI_F / 180.0f32)
}

#[inline]
pub fn sinf(x: f32) -> f32 {
    libm::sinf(x)
}

#[inline]
pub fn cosf(x: f32) -> f32 {
    libm::cosf(x)
}

#[inline]
pub fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

/// IEEE sqrt is correctly rounded on all supported targets, so `std` is fine here.
#[inline]
pub fn sqrtf(x: f32) -> f32 {
    x.sqrt()
}

#[inline]
pub fn fabsf(x: f32) -> f32 {
    x.abs()
}

/// Source `clamp` template.
#[inline]
pub fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[inline]
pub fn fmin(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

#[inline]
pub fn fmax(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// `RemapValClamped` from mathlib.h.
#[inline]
pub fn remap_val_clamped(val: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    if a == b {
        return if val >= b { d } else { c };
    }
    let mut cval = (val - a) / (b - a);
    cval = clamp(cval, 0.0, 1.0);
    c + (d - c) * cval
}

/// `SimpleSpline` from mathlib.h: ease-in/ease-out.
#[inline]
pub fn simple_spline(value: f32) -> f32 {
    let value_squared = value * value;
    3.0 * value_squared - 2.0 * value_squared * value
}

/// `SimpleSplineRemapValClamped` from mathlib.h.
#[inline]
pub fn simple_spline_remap_val_clamped(val: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    if a == b {
        return if val >= b { d } else { c };
    }
    let mut cval = (val - a) / (b - a);
    cval = clamp(cval, 0.0, 1.0);
    c + (d - c) * simple_spline(cval)
}

/// A 3-component float vector with Source semantics (Z is up).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Hash for Vec3 {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.x.to_bits().hash(state);
        self.y.to_bits().hash(state);
        self.z.to_bits().hash(state);
    }
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }

    #[inline]
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    /// `CrossProduct(a, b)`.
    #[inline]
    pub fn cross(self, b: Vec3) -> Vec3 {
        Vec3 {
            x: self.y * b.z - self.z * b.y,
            y: self.z * b.x - self.x * b.z,
            z: self.x * b.y - self.y * b.x,
        }
    }

    #[inline]
    pub fn length_sqr(self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// `VectorLength` / `Vector::Length()`.
    #[inline]
    pub fn length(self) -> f32 {
        sqrtf(self.x * self.x + self.y * self.y + self.z * self.z)
    }

    /// `Vector::Length2D()`.
    #[inline]
    pub fn length_2d(self) -> f32 {
        sqrtf(self.x * self.x + self.y * self.y)
    }

    #[inline]
    pub fn length_2d_sqr(self) -> f32 {
        self.x * self.x + self.y * self.y
    }

    /// `VectorNormalize( Vector& v )`: normalizes in place and returns the previous length.
    /// Mirrors the PC implementation: `l = Length(); if (l != 0) v /= l; else v = (0,0,1)`,
    /// where `operator/=` multiplies by the reciprocal.
    #[inline]
    pub fn normalize_in_place(&mut self) -> f32 {
        let l = self.length();
        if l != 0.0 {
            let oofl = 1.0 / l;
            self.x *= oofl;
            self.y *= oofl;
            self.z *= oofl;
        } else {
            self.x = 0.0;
            self.y = 0.0;
            self.z = 1.0;
        }
        l
    }

    #[inline]
    pub fn normalized(mut self) -> Vec3 {
        self.normalize_in_place();
        self
    }

    /// `VectorMA(start, scale, dir)` = start + scale * dir.
    #[inline]
    pub fn ma(self, scale: f32, dir: Vec3) -> Vec3 {
        Vec3 {
            x: self.x + scale * dir.x,
            y: self.y + scale * dir.y,
            z: self.z + scale * dir.z,
        }
    }

    #[inline]
    pub fn dist_to(self, o: Vec3) -> f32 {
        (self - o).length()
    }

    #[inline]
    pub fn is_zero(self) -> bool {
        self.x == 0.0 && self.y == 0.0 && self.z == 0.0
    }

    #[inline]
    pub fn get(self, i: usize) -> f32 {
        match i {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: f32) {
        match i {
            0 => self.x = v,
            1 => self.y = v,
            _ => self.z = v,
        }
    }

    #[inline]
    pub fn with_z(self, z: f32) -> Vec3 {
        Vec3 { x: self.x, y: self.y, z }
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    #[inline]
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl AddAssign for Vec3 {
    #[inline]
    fn add_assign(&mut self, o: Vec3) {
        self.x += o.x;
        self.y += o.y;
        self.z += o.z;
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    #[inline]
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl SubAssign for Vec3 {
    #[inline]
    fn sub_assign(&mut self, o: Vec3) {
        self.x -= o.x;
        self.y -= o.y;
        self.z -= o.z;
    }
}
impl Mul<f32> for Vec3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Mul<Vec3> for f32 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        Vec3::new(v.x * self, v.y * self, v.z * self)
    }
}
impl MulAssign<f32> for Vec3 {
    #[inline]
    fn mul_assign(&mut self, s: f32) {
        self.x *= s;
        self.y *= s;
        self.z *= s;
    }
}
impl Div<f32> for Vec3 {
    type Output = Vec3;
    /// Source `operator/`: multiplies by the reciprocal.
    #[inline]
    fn div(self, s: f32) -> Vec3 {
        let oofl = 1.0 / s;
        Vec3::new(self.x * oofl, self.y * oofl, self.z * oofl)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    #[inline]
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// Euler angles in degrees, Source order: pitch (x), yaw (y), roll (z).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct QAngle {
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}

impl Hash for QAngle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pitch.to_bits().hash(state);
        self.yaw.to_bits().hash(state);
        self.roll.to_bits().hash(state);
    }
}

impl QAngle {
    #[inline]
    pub const fn new(pitch: f32, yaw: f32, roll: f32) -> Self {
        QAngle { pitch, yaw, roll }
    }
}

/// `AngleVectors( const QAngle &angles, Vector *forward, Vector *right, Vector *up )`.
pub fn angle_vectors(angles: QAngle) -> (Vec3, Vec3, Vec3) {
    let (sy, cy) = (sinf(deg2rad(angles.yaw)), cosf(deg2rad(angles.yaw)));
    let (sp, cp) = (sinf(deg2rad(angles.pitch)), cosf(deg2rad(angles.pitch)));
    let (sr, cr) = (sinf(deg2rad(angles.roll)), cosf(deg2rad(angles.roll)));

    let forward = Vec3::new(cp * cy, cp * sy, -sp);
    let right = Vec3::new(
        -1.0 * sr * sp * cy + -1.0 * cr * -sy,
        -1.0 * sr * sp * sy + -1.0 * cr * cy,
        -1.0 * sr * cp,
    );
    let up = Vec3::new(
        cr * sp * cy + -sr * -sy,
        cr * sp * sy + -sr * cy,
        cr * cp,
    );
    (forward, right, up)
}

/// `VectorAngles( const Vector& forward, QAngle &angles )`.
pub fn vector_angles(forward: Vec3) -> QAngle {
    let yaw: f32;
    let pitch: f32;
    if forward.y == 0.0 && forward.x == 0.0 {
        yaw = 0.0;
        pitch = if forward.z > 0.0 { 270.0 } else { 90.0 };
    } else {
        // `atan2(float, float) * 180 / M_PI`: float atan2, then double arithmetic, stored to float.
        let mut y = (atan2f(forward.y, forward.x) as f64 * 180.0 / std::f64::consts::PI) as f32;
        if y < 0.0 {
            y += 360.0;
        }
        yaw = y;
        let tmp = sqrtf(forward.x * forward.x + forward.y * forward.y);
        let mut p = (atan2f(-forward.z, tmp) as f64 * 180.0 / std::f64::consts::PI) as f32;
        if p < 0.0 {
            p += 360.0;
        }
        pitch = p;
    }
    QAngle::new(pitch, yaw, 0.0)
}

/// Wrap an angle to (-180, 180].
pub fn normalize_yaw(mut yaw: f32) -> f32 {
    while yaw > 180.0 {
        yaw -= 360.0;
    }
    while yaw <= -180.0 {
        yaw += 360.0;
    }
    yaw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_vectors_forward_is_unit() {
        let (f, r, u) = angle_vectors(QAngle::new(-30.0, 45.0, 0.0));
        assert!((f.length() - 1.0).abs() < 1e-5);
        assert!((r.length() - 1.0).abs() < 1e-5);
        assert!((u.length() - 1.0).abs() < 1e-5);
        assert!(f.z > 0.0, "negative pitch looks up in Source");
    }

    #[test]
    fn vector_angles_roundtrip() {
        let a = QAngle::new(20.0, 135.0, 0.0);
        let (f, _, _) = angle_vectors(a);
        let b = vector_angles(f);
        assert!((b.pitch - 20.0).abs() < 1e-3);
        assert!((b.yaw - 135.0).abs() < 1e-3);
    }

    #[test]
    fn normalize_source_semantics() {
        let mut v = Vec3::ZERO;
        assert_eq!(v.normalize_in_place(), 0.0);
        assert_eq!(v, Vec3::new(0.0, 0.0, 1.0));
    }
}
