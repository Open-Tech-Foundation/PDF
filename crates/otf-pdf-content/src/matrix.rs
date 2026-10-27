//! Affine transformation matrices, `[a b c d e f]` (ISO 32000-2 §8.3.4):
//! a point `[x y 1]` times the matrix.

/// An affine transformation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    /// Row 1, column 1.
    pub a: f64,
    /// Row 1, column 2.
    pub b: f64,
    /// Row 2, column 1.
    pub c: f64,
    /// Row 2, column 2.
    pub d: f64,
    /// Translation x.
    pub e: f64,
    /// Translation y.
    pub f: f64,
}

impl Matrix {
    /// The identity.
    pub const IDENTITY: Matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    /// A matrix from its six numbers.
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// A translation.
    pub const fn translate(x: f64, y: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    /// From six operands, if they are all finite numbers.
    pub fn from_slice(v: &[f64]) -> Option<Self> {
        match v {
            [a, b, c, d, e, f] if v.iter().all(|x| x.is_finite()) => {
                Some(Self::new(*a, *b, *c, *d, *e, *f))
            }
            _ => None,
        }
    }

    /// `self` then `other`: points are transformed by `self` first.
    pub fn then(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Transforms a point.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            x * self.a + y * self.c + self.e,
            x * self.b + y * self.d + self.f,
        )
    }

    /// Transforms a vector (no translation).
    pub fn apply_vector(&self, x: f64, y: f64) -> (f64, f64) {
        (x * self.a + y * self.c, x * self.b + y * self.d)
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_order() {
        let scale = Matrix::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        let shift = Matrix::translate(10.0, 0.0);
        // Scale first, then shift.
        assert_eq!(scale.then(&shift).apply(1.0, 1.0), (12.0, 2.0));
        // Shift first, then scale.
        assert_eq!(shift.then(&scale).apply(1.0, 1.0), (22.0, 2.0));
        assert_eq!(Matrix::IDENTITY.then(&shift), shift);
        assert_eq!(scale.apply_vector(1.0, 0.0), (2.0, 0.0));
        assert!(Matrix::from_slice(&[1.0, 0.0, 0.0, 1.0, 0.0]).is_none());
        assert!(Matrix::from_slice(&[f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]).is_none());
    }
}
