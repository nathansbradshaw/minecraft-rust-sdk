//! Versioned native fluid volumes for bounded local input prediction.
use super::*;
use minecraft_sdk_java_1_21_1::physics::Medium;

struct Volume {
    bounds: [f64; 6],
    medium: Option<Medium>,
    still: bool,
}
pub(super) struct Volumes(Vec<Volume>);
impl Volumes {
    pub(super) fn new(rows: &[Value]) -> AnyResult<Option<Self>> {
        if rows.iter().all(|r| r.get("fluid").is_none()) {
            // Older matching installations retain their original admission.
            return Ok(None);
        }
        let mut volumes = Vec::new();
        for row in rows {
            let f = &row["fluid"];
            if f["format"] != "java767-fluid-v1" || !f["empty"].is_boolean() {
                return Err("Complete versioned native fluid facts required".into());
            }
            if f["empty"] == true {
                continue;
            }
            let p: [f64; 3] =
                std::array::from_fn(|i| row["position"][i].as_f64().unwrap_or(f64::NAN));
            let height = f["height"]
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0. && *v <= 1.)
                .ok_or("Invalid native fluid height")?;
            if p.iter().any(|v| !v.is_finite()) {
                return Err("Invalid native fluid position".into());
            }
            let medium = match (f["water"].as_bool(), f["lava"].as_bool()) {
                (Some(true), Some(false)) => Some(Medium::Water),
                (Some(false), Some(true)) => Some(Medium::Lava),
                (Some(false), Some(false)) => None,
                _ => return Err("Invalid native fluid tags".into()),
            };
            let still = f["flow_known"] == true
                && f["flow"].as_array().is_some_and(|v| {
                    v.len() == 3
                        && v.iter()
                            .all(|n| n.as_f64().is_some_and(|n| n.is_finite() && n.abs() < 1e-9))
                });
            volumes.push(Volume {
                bounds: [p[0], p[1], p[2], p[0] + 1., p[1] + height, p[2] + 1.],
                medium,
                still,
            });
        }
        Ok(Some(Self(volumes)))
    }
    pub(super) fn medium(&self, p: [f64; 3], width: f64, height: f64) -> AnyResult<Medium> {
        let lo = [
            p[0] - width / 2. + 0.001,
            p[1] + 0.001,
            p[2] - width / 2. + 0.001,
        ];
        let hi = [
            p[0] + width / 2. - 0.001,
            p[1] + height - 0.001,
            p[2] + width / 2. - 0.001,
        ];
        let mut medium = Medium::Air;
        for v in &self.0 {
            if (0..3).all(|i| hi[i] > v.bounds[i] && lo[i] < v.bounds[i + 3]) {
                if !v.still {
                    return Err("Flowing or unobserved fluid currents require a compatible movement adapter".into());
                }
                let next = v
                    .medium
                    .ok_or("Unknown fluid physics requires a compatible movement adapter")?;
                if next == Medium::Lava {
                    return Err(
                        "Lava transitions are not supported by this local route predictor".into(),
                    );
                }
                medium = next;
            }
        }
        Ok(medium)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn water() -> Value {
        json!({"position":[0,0,0],"fluid":{"format":"java767-fluid-v1","empty":false,"water":true,"lava":false,"height":0.75,"flow_known":true,"flow":[0,0,0]}})
    }
    #[test]
    fn height_and_body_overlap_determine_medium_without_registry_name_guesses() {
        let v = Volumes::new(&[water()]).unwrap().unwrap();
        assert_eq!(v.medium([0.5, 0.8, 0.5], 0.6, 1.8).unwrap(), Medium::Air);
        assert_eq!(v.medium([0.5, 0.7, 0.5], 0.6, 1.8).unwrap(), Medium::Water);
        assert_eq!(v.medium([1.5, 0.7, 0.5], 0.6, 1.8).unwrap(), Medium::Air);
    }
    #[test]
    fn unknown_partial_current_and_lava_facts_fail_closed_only_when_contacted() {
        assert!(Volumes::new(&[water(), json!({"position":[1,0,0]})]).is_err());
        for (field, value) in [
            ("flow", json!([0.2, 0, 0])),
            ("flow_known", json!(false)),
            ("water", json!(false)),
            ("format", json!("future")),
            ("height", json!(2.)),
        ] {
            let mut row = water();
            row["fluid"][field] = value;
            if let Ok(Some(v)) = Volumes::new(&[row]) {
                assert!(v.medium([0.5, 0.5, 0.5], 0.6, 1.8).is_err());
                assert_eq!(v.medium([2.5, 0.5, 0.5], 0.6, 1.8).unwrap(), Medium::Air);
            }
        }
    }
}
