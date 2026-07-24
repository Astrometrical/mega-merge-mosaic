//! Minimal dense linear algebra for the photometric solve.
//!
//! One routine: LU with partial pivoting solving `A x = b` for the small
//! normal-equation systems (2N×2N, N ≤ a few hundred panels). No external
//! linear-algebra dependency by design.

use crate::{Error, Result};

/// Solve `A x = b`. `a` is row-major n×n; both `a` and `b` are clobbered
/// (reduced in place). Errors on a singular or numerically singular system.
pub fn solve_dense(a: &mut [f64], b: &mut [f64], n: usize) -> Result<Vec<f64>> {
    assert_eq!(a.len(), n * n, "matrix must be n×n");
    assert_eq!(b.len(), n, "rhs must be length n");

    // Scale-relative singularity threshold.
    let anorm = a.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
    let tiny = if anorm > 0.0 { anorm * 1e-13 } else { f64::MIN_POSITIVE };

    for k in 0..n {
        // Partial pivot: largest |a[r][k]| for r ≥ k.
        let mut p = k;
        let mut pmax = a[k * n + k].abs();
        for r in k + 1..n {
            let v = a[r * n + k].abs();
            if v > pmax {
                pmax = v;
                p = r;
            }
        }
        if !pmax.is_finite() || pmax < tiny {
            return Err(Error::format(
                "solve_dense",
                format!("singular system (pivot {pmax:.3e} at column {k}, n={n})"),
            ));
        }
        if p != k {
            for c in k..n {
                a.swap(k * n + c, p * n + c);
            }
            b.swap(k, p);
        }
        let pivot = a[k * n + k];
        for r in k + 1..n {
            let m = a[r * n + k] / pivot;
            if m == 0.0 {
                continue;
            }
            a[r * n + k] = 0.0;
            for c in k + 1..n {
                a[r * n + c] -= m * a[k * n + c];
            }
            b[r] -= m * b[k];
        }
    }

    // Back substitution.
    let mut x = vec![0.0f64; n];
    for k in (0..n).rev() {
        let mut s = b[k];
        for c in k + 1..n {
            s -= a[k * n + c] * x[c];
        }
        x[k] = s / a[k * n + k];
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_known_3x3_system() {
        // 2x + y − z = 8; −3x − y + 2z = −11; −2x + y + 2z = −3 → (2, 3, −1).
        let mut a = vec![2.0, 1.0, -1.0, -3.0, -1.0, 2.0, -2.0, 1.0, 2.0];
        let mut b = vec![8.0, -11.0, -3.0];
        let x = solve_dense(&mut a, &mut b, 3).unwrap();
        assert!((x[0] - 2.0).abs() < 1e-12, "x = {x:?}");
        assert!((x[1] - 3.0).abs() < 1e-12, "x = {x:?}");
        assert!((x[2] + 1.0).abs() < 1e-12, "x = {x:?}");
    }

    #[test]
    fn pivots_past_zero_diagonal() {
        // Requires a row swap: a[0][0] = 0.
        let mut a = vec![0.0, 1.0, 1.0, 0.0];
        let mut b = vec![3.0, 4.0];
        let x = solve_dense(&mut a, &mut b, 2).unwrap();
        assert_eq!(x, vec![4.0, 3.0]);
    }

    #[test]
    fn errors_on_singular_matrix() {
        // Second row is 2× the first.
        let mut a = vec![1.0, 2.0, 2.0, 4.0];
        let mut b = vec![1.0, 2.0];
        assert!(solve_dense(&mut a, &mut b, 2).is_err());
    }

    #[test]
    fn solves_identity_trivially() {
        let mut a = vec![1.0, 0.0, 0.0, 1.0];
        let mut b = vec![5.0, -7.0];
        let x = solve_dense(&mut a, &mut b, 2).unwrap();
        assert_eq!(x, vec![5.0, -7.0]);
    }
}
