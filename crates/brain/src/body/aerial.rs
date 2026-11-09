//! Client-side refuge preference; observed collision cover is not invulnerability.
use serde_json::Value;

pub(super) fn diver(entity: &Value) -> bool {
    entity["type"] == "minecraft:phantom"
}

/// Require a low solid underside over the entire ordinary player footprint.
/// A single eye ray, leaves without collision, and unknown terrain are insufficient.
pub(super) fn roofed(p: [f64; 3], obstacles: &[[f64; 6]]) -> bool {
    covered_footprint(p, obstacles, 0.)
}

/// Conservative extra horizontal coverage, capped at one block. This is a
/// preference for observed interior, not a proof that a flyer cannot enter.
pub(super) fn margin(p: [f64; 3], obstacles: &[[f64; 6]]) -> f64 {
    let mut margin = 0.;
    for extra in [0.25, 0.5, 0.75, 1.] {
        if !covered_footprint(p, obstacles, extra) {
            break;
        }
        margin = extra;
    }
    margin
}

fn covered_footprint(p: [f64; 3], obstacles: &[[f64; 6]], extra: f64) -> bool {
    let radius = 0.3 + extra;
    let bounds = [p[0] - radius, p[2] - radius, p[0] + radius, p[2] + radius];
    let mut roofs = Vec::new();
    for b in obstacles {
        if !b.iter().all(|n| n.is_finite()) || b[4] <= b[1] || !(1.8..=2.5).contains(&(b[1] - p[1]))
        {
            continue;
        }
        let r = [
            b[0].max(bounds[0]),
            b[2].max(bounds[1]),
            b[3].min(bounds[2]),
            b[5].min(bounds[3]),
        ];
        if r[2] > r[0] && r[3] > r[1] {
            if roofs.len() == 32 {
                return false; // Too complex to establish cover within this policy budget.
            }
            roofs.push(r);
        }
    }
    if roofs.is_empty() {
        return false;
    }
    let mut xs = vec![bounds[0], bounds[2]];
    let mut zs = vec![bounds[1], bounds[3]];
    for r in &roofs {
        xs.extend([r[0], r[2]]);
        zs.extend([r[1], r[3]]);
    }
    xs.sort_by(f64::total_cmp);
    zs.sort_by(f64::total_cmp);
    xs.dedup();
    zs.dedup();
    // All rectangle boundaries partition the footprint into constant-coverage
    // cells. Checking each cell establishes the union, including small gaps
    // that a fixed set of head/corner samples would miss.
    xs.windows(2).all(|x| {
        zs.windows(2).all(|z| {
            let q = [(x[0] + x[1]) / 2., (z[0] + z[1]) / 2.];
            roofs
                .iter()
                .any(|r| q[0] >= r[0] && q[0] <= r[2] && q[1] >= r[1] && q[1] <= r[3])
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
    #[test]
    fn joined_roof_pieces_cover_the_footprint_but_sampled_patches_do_not() {
        let p = [0.5, 1.0, 0.5];
        assert!(roofed(
            p,
            &[[0., 3., 0., 0.5, 4., 1.], [0.5, 3., 0., 1., 4., 1.]]
        ));
        let patches: Vec<_> = [0.21, 0.5, 0.79]
            .into_iter()
            .flat_map(|x| {
                [0.21, 0.5, 0.79]
                    .into_iter()
                    .map(move |z| [x - 0.02, 3., z - 0.02, x + 0.02, 4., z + 0.02])
            })
            .collect();
        assert!(
            !roofed(p, &patches),
            "nine covered samples do not establish a continuous roof"
        );
        assert!(
            !roofed(p, &vec![[0., 3., 0., 1., 4., 1.]; 33]),
            "complex unbounded geometry is not proof of refuge"
        );
    }
    #[test]
    fn interior_margin_requires_observed_continuous_cover() {
        let roof = [[-1., 3., -1., 2., 4., 2.]];
        assert_eq!(margin([0.5, 1., 0.5], &roof), 1.);
        assert_eq!(margin([-0.5, 1., -0.5], &roof), 0.);
        assert_eq!(margin([0.5, 1., 0.5], &[]), 0.);
        let gap = [[-1., 3., -1., 0.99, 4., 2.], [1.01, 3., -1., 2., 4., 2.]];
        assert_eq!(margin([0.5, 1., 0.5], &gap), 0.);
    }
}
