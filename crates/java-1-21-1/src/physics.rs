//! Collision-limited player movement. Geometry comes from the current native registry/world.
pub fn step(
    p: [f64; 3],
    horizontal: [f64; 3],
    vertical: f64,
    obstacles: &[[f64; 6]],
) -> ([f64; 3], bool) {
    let mut body = [
        p[0] - 0.3,
        p[1],
        p[2] - 0.3,
        p[0] + 0.3,
        p[1] + 1.8,
        p[2] + 0.3,
    ];
    let mut next = p;
    let mut grounded = false;
    for axis in [1, 0, 2] {
        let wanted = if axis == 1 {
            vertical
        } else {
            horizontal[axis]
        };
        let mut distance = wanted;
        for obstacle in obstacles {
            if (0..3)
                .filter(|a| *a != axis)
                .all(|a| body[a + 3] > obstacle[a] + 1e-7 && body[a] < obstacle[a + 3] - 1e-7)
            {
                if distance > 0. && body[axis + 3] <= obstacle[axis] + 1e-7 {
                    distance = distance.min(obstacle[axis] - body[axis + 3]);
                } else if distance < 0. && body[axis] >= obstacle[axis + 3] - 1e-7 {
                    distance = distance.max(obstacle[axis + 3] - body[axis]);
                }
            }
        }
        if axis == 1 {
            grounded = wanted < 0. && (distance - wanted).abs() > 1e-7;
        }
        next[axis] += distance;
        body[axis] += distance;
        body[axis + 3] += distance;
    }
    (next, grounded)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jump_lands_and_wall_prevents_clipping() {
        let floor = [-4., 0., -4., 4., 1., 4.];
        let wall = [1., 1., -1., 2., 4., 1.];
        let mut p = [0.5, 1., 0.5];
        let mut velocity = 0.42;
        let mut peak = 1.;
        let mut landed = false;
        for _ in 0..40 {
            let (next, ground) = step(p, [0.; 3], velocity, &[floor]);
            p = next;
            peak = f64::max(peak, p[1]);
            if ground {
                landed = true;
                break;
            }
            velocity = (velocity - 0.08) * 0.98;
        }
        assert!(landed && peak > 2. && peak < 2.3);
        assert_eq!(p[1], 1.);
        let (p, ground) = step([0.5, 1., 0.5], [0.5, 0., 0.], -0.08, &[floor, wall]);
        assert!(ground);
        assert!((p[0] - 0.7).abs() < 1e-8);
    }
}
