//! Client-side refuge preference; observed collision cover is not invulnerability.
use serde_json::Value;

pub(super) fn diver(entity: &Value) -> bool {
    entity["type"] == "minecraft:phantom"
}

/// Require a low solid underside over the entire ordinary player footprint.
/// A single eye ray, leaves without collision, and unknown terrain are insufficient.
pub(super) fn roofed(p: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    [-0.29, 0.0, 0.29].into_iter().all(|dx| {
        [-0.29, 0.0, 0.29].into_iter().all(|dz| {
            obstacles.iter().any(|b| {
                b.iter().all(|n| n.is_finite())
                    && b[3] > b[0]
                    && b[4] > b[1]
                    && b[5] > b[2]
                    && (1.8..=2.5).contains(&(b[1] - p[1]))
                    && p[0] + dx > b[0] + 1e-6
                    && p[0] + dx < b[3] - 1e-6
                    && p[2] + dz > b[2] + 1e-6
                    && p[2] + dz < b[5] - 1e-6
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuge_requires_complete_low_collision_cover() {
        let p = [0.5, 1.0, 0.5];
        assert!(roofed(p, &[[0., 3., 0., 1., 4., 1.]]));
        for roof in [
            vec![],
            vec![[0.4, 3., 0.4, 0.6, 4., 0.6]],
            vec![[0., 5., 0., 1., 6., 1.]],
            vec![[0., 2., 0., 1., 3., 1.]],
            vec![[0., 3., 0., 1., 3., 1.]],
            vec![[0., f64::NAN, 0., 1., 4., 1.]],
        ] {
            assert!(!roofed(p, &roof));
        }
    }
}
