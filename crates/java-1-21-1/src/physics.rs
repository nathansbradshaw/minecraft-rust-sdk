//! Collision-limited player movement. Geometry comes from the current native registry/world.
pub fn step(
    p: [f64; 3],
    horizontal: [f64; 3],
    vertical: f64,
    obstacles: &[[f64; 6]],
) -> ([f64; 3], bool) {
    step_sized(p, horizontal, vertical, obstacles, 0.6, 1.8)
}
pub fn step_sized(
    p: [f64; 3],
    horizontal: [f64; 3],
    vertical: f64,
    obstacles: &[[f64; 6]],
    width: f64,
    height: f64,
) -> ([f64; 3], bool) {
    let mut body = [
        p[0] - width / 2.,
        p[1],
        p[2] - width / 2.,
        p[0] + width / 2.,
        p[1] + height,
        p[2] + width / 2.,
    ];
    let mut next = p;
    let mut grounded = false;
    let axes = if horizontal[0].abs() < horizontal[2].abs() {
        [1, 2, 0]
    } else {
        [1, 0, 2]
    };
    for axis in axes {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Medium {
    Air,
    Water,
    Lava,
}
#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub width: f64,
    pub height: f64,
    pub gravity: f64,
    pub jump_velocity: f64,
    pub step_height: f64,
    pub friction: f64,
    pub speed_factor: f64,
    pub medium: Medium,
    pub water_efficiency: f64,
    pub dolphins_grace: bool,
    pub sprinting: bool,
    pub slow_falling: bool,
    pub levitation: Option<u8>,
    pub no_gravity: bool,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            width: 0.6,
            height: 1.8,
            gravity: 0.08,
            jump_velocity: 0.42,
            step_height: 0.6,
            friction: 0.6,
            speed_factor: 1.,
            medium: Medium::Air,
            water_efficiency: 0.,
            dolphins_grace: false,
            sprinting: false,
            slow_falling: false,
            levitation: None,
            no_gravity: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
}
impl Body {
    /// Client physics only. Native server observations verify submitted effects.
    /// Clearing input never clears gravity or server-provided momentum.
    pub fn tick(&mut self, acceleration: [f64; 3], jump: bool, p: Parameters, boxes: &[[f64; 6]]) {
        let (support, contact) =
            step_sized(self.position, [0.; 3], -0.001, boxes, p.width, p.height);
        self.on_ground = contact && (support[1] - self.position[1]).abs() < 1e-7;
        let friction_ground = self.on_ground;
        if jump {
            match p.medium {
                Medium::Air if self.on_ground => self.velocity[1] = p.jump_velocity,
                Medium::Water | Medium::Lava => self.velocity[1] += 0.04,
                _ => {}
            }
        }
        for (v, a) in self.velocity.iter_mut().zip(acceleration) {
            *v += a;
        }
        let old = self.position;
        let velocity = self.velocity;
        let (mut next, mut ground) =
            step_sized(old, velocity, velocity[1], boxes, p.width, p.height);
        let clipped_horizontal = (next[0] - old[0] - velocity[0]).abs() > 1e-7
            || (next[2] - old[2] - velocity[2]).abs() > 1e-7;
        if clipped_horizontal
            && p.step_height > 0.
            && (self.on_ground || ground)
            && p.medium == Medium::Air
        {
            let (raised, _) = step_sized(old, [0.; 3], p.step_height, boxes, p.width, p.height);
            let rise = raised[1] - old[1];
            let (across, _) = step_sized(raised, velocity, 0., boxes, p.width, p.height);
            let (landed, contact) = step_sized(
                across,
                [0.; 3],
                velocity[1] - rise,
                boxes,
                p.width,
                p.height,
            );
            let progress = |q: [f64; 3]| (q[0] - old[0]).powi(2) + (q[2] - old[2]).powi(2);
            if rise > 0. && progress(landed) > progress(next) + 1e-9 {
                next = landed;
                ground = contact;
            }
        }
        for axis in 0..3 {
            // A legal step raises feet; its vertical offset is not an impulse.
            if (next[axis] - old[axis] - velocity[axis]).abs() > 1e-7 {
                self.velocity[axis] = 0.;
            }
        }
        if velocity[1] <= 0. && !ground {
            let (probe, contact) = step_sized(next, [0.; 3], -0.001, boxes, p.width, p.height);
            ground = contact && (probe[1] - next[1]).abs() < 1e-7;
        }
        self.position = next;
        self.on_ground = ground;
        let gravity = if p.no_gravity {
            0.
        } else if p.slow_falling && self.velocity[1] <= 0. {
            p.gravity.min(0.01)
        } else {
            p.gravity
        };
        match p.medium {
            Medium::Air => {
                let drag = if friction_ground {
                    p.friction * 0.91
                } else {
                    0.91
                };
                self.velocity[0] *= drag * p.speed_factor;
                self.velocity[2] *= drag * p.speed_factor;
                if let Some(level) = p.levitation {
                    self.velocity[1] += (0.05 * (f64::from(level) + 1.) - self.velocity[1]) * 0.2;
                } else {
                    self.velocity[1] -= gravity;
                }
                self.velocity[1] *= 0.98;
            }
            Medium::Water => {
                let efficiency = p.water_efficiency.clamp(0., 1.) * if ground { 1. } else { 0.5 };
                let base = if p.sprinting { 0.9 } else { 0.8 };
                let drag = if p.dolphins_grace {
                    0.96
                } else {
                    base + (0.546 - base) * efficiency
                };
                self.velocity[0] *= drag;
                self.velocity[2] *= drag;
                self.velocity[1] = self.velocity[1] * 0.8 - gravity / 16.;
            }
            Medium::Lava => {
                self.velocity[0] *= 0.5;
                self.velocity[2] *= 0.5;
                self.velocity[1] = self.velocity[1] * 0.8 - gravity / 4.;
            }
        }
        for v in &mut self.velocity {
            if v.abs() < 0.003 {
                *v = 0.;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_release_and_idle_support_removal_continue_ballistic_motion() {
        let floor = [-20., 0., -20., 20., 1., 20.];
        let mut b = Body {
            position: [0.5, 5., 0.5],
            velocity: [0.12, 0.2, 0.],
            on_ground: false,
        };
        let original = b.position;
        for _ in 0..50 {
            b.tick([0.; 3], false, Parameters::default(), &[floor]);
        }
        assert_eq!(b.position[1], 1.);
        assert!(b.on_ground);
        assert!(b.position[0] > original[0]);
        let mut b = Body {
            position: [0.5, 4., 0.5],
            velocity: [0.; 3],
            on_ground: true,
        };
        b.tick([0.; 3], false, Parameters::default(), &[floor]);
        b.tick([0.; 3], false, Parameters::default(), &[floor]);
        assert!(
            b.position[1] < 4. && !b.on_ground,
            "An old on_ground flag cannot cause hovering after support disappears"
        );
    }
    #[test]
    fn native_sized_body_steps_up_and_cannot_jump_through_a_low_ceiling() {
        let floor = [-4., 0., -4., 4., 1., 4.];
        let slab = [1., 1., 0., 2., 1.5, 1.];
        let mut b = Body {
            position: [0.5, 1., 0.5],
            velocity: [0.; 3],
            on_ground: true,
        };
        let mut peak = 1f64;
        for _ in 0..12 {
            b.tick([0.1, 0., 0.], false, Parameters::default(), &[floor, slab]);
            peak = peak.max(b.position[1]);
        }
        assert!(b.position[0] > 1. && peak >= 1.5, "{b:?}; peak={peak}");
        let ceiling = [-4., 3., -4., 4., 4., 4.];
        let mut b = Body {
            position: [0.5, 1., 0.5],
            velocity: [0.; 3],
            on_ground: true,
        };
        b.tick([0.; 3], true, Parameters::default(), &[floor, ceiling]);
        assert!(b.position[1] <= 1.2 + 1e-7);
    }
    #[test]
    fn water_release_sinks_and_repeated_jump_input_can_reach_the_surface() {
        let p = Parameters {
            medium: Medium::Water,
            ..Parameters::default()
        };
        let mut b = Body {
            position: [0., 5., 0.],
            velocity: [0.; 3],
            on_ground: false,
        };
        for _ in 0..20 {
            b.tick([0.; 3], false, p, &[]);
        }
        assert!(b.position[1] < 5.);
        let low = b.position[1];
        for _ in 0..20 {
            b.tick([0.; 3], true, p, &[]);
        }
        assert!(b.position[1] > low);
    }
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
