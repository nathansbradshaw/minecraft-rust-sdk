//! Local ranged turn preference when actual recent motion meets a known wall.
use super::*;

pub(super) struct Turn {
    radial: [f64; 2],
    sense: f64,
    enemies: Vec<[f64; 3]>,
}
impl Turn {
    pub fn find(
        o: &Observation,
        recent: &[[f64; 3]],
        obstacles: &[[f64; 6]],
        enemies: Vec<[f64; 3]>,
    ) -> Option<Self> {
        if o.player["on_ground"] != true || enemies.is_empty() {
            return None;
        }
        let start = point(&o.player["position"])?;
        let previous = recent.iter().rev().find(|p| {
            (p[1] - start[1]).abs() < 0.6
                && (0.75..=6.).contains(&(start[0] - p[0]).hypot(start[2] - p[2]))
        })?;
        let mut heading = [start[0] - previous[0], start[2] - previous[2]];
        let length = heading[0].hypot(heading[1]);
        heading.iter_mut().for_each(|v| *v /= length);
        // Missing cells do not establish a boundary. Require an observed solid
        // intersecting continuation of this character's actual recent motion.
        if !(1..=8).any(|n| {
            let d = f64::from(n) / 4.;
            let p = [
                start[0] + heading[0] * d,
                start[1],
                start[2] + heading[1] * d,
            ];
            obstacles.iter().any(|b| intersects(p, b))
        }) {
            return None;
        }
        let enemy = enemies.iter().min_by(|a, b| {
            (start[0] - a[0])
                .hypot(start[2] - a[2])
                .total_cmp(&(start[0] - b[0]).hypot(start[2] - b[2]))
        })?;
        let mut radial = [start[0] - enemy[0], start[2] - enemy[2]];
        let distance = radial[0].hypot(radial[1]);
        if distance < 6. {
            return None;
        }
        radial.iter_mut().for_each(|v| *v /= distance);
        let cross = radial[0] * heading[1] - radial[1] * heading[0];
        if cross.abs() < 0.2 {
            return None;
        }
        Some(Self {
            radial,
            sense: cross.signum(),
            enemies,
        })
    }
    pub fn lateral(&self, start: [f64; 3], p: [f64; 3]) -> f64 {
        self.sense * (self.radial[0] * (p[2] - start[2]) - self.radial[1] * (p[0] - start[0]))
    }
    pub fn allows(&self, start: [f64; 3], p: [f64; 3]) -> bool {
        let d = [p[0] - start[0], p[2] - start[2]];
        let length = d[0] * d[0] + d[1] * d[1];
        self.enemies.iter().all(|e| {
            let t = if length > 0. {
                ((e[0] - start[0]) * d[0] + (e[2] - start[2]) * d[1]) / length
            } else {
                0.
            }
            .clamp(0., 1.);
            (start[0] + t * d[0] - e[0]).hypot(start[2] + t * d[1] - e[2]) >= 4.5
        })
    }
}
