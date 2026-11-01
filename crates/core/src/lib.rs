//! Version-independent contracts. Registry names and advertised capabilities are runtime data.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: u32 = 1;
pub const ADAPTER_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    Revoked,
    Conflict,
    SessionRequired,
    ServerFull,
    UnsupportedVersion,
    UnsupportedCapability,
    PermissionDenied,
    UncertainOutcome,
    NotFound,
    NotReady,
    RateLimited,
    Internal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}
impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}
pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerInfo {
    pub api_version: u32,
    pub instance_id: String,
    pub minecraft_version: String,
    pub platform: String,
    pub adapter_version: String,
    pub ready: bool,
    pub capabilities: Vec<String>,
    pub limits: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub name: String,
}
/// The key is returned once. Debug intentionally hides it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub bot_id: String,
    pub player_uuid: String,
    pub key: String,
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("bot_id", &self.bot_id)
            .field("key", &"[redacted]")
            .finish()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub adapter_version: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    pub request_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hand {
    Main,
    Off,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigStage {
    Start,
    Finish,
    Cancel,
}

/// Closed player action vocabulary. No raw packets, actor IDs, console or host operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Move {
        target: Position,
        on_ground: bool,
        /// Request a jumping trajectory rather than a grounded walk.
        #[serde(default)]
        jump: bool,
    },
    Look {
        yaw: f32,
        pitch: f32,
    },
    Jump,
    Sprint {
        enabled: bool,
    },
    Sneak {
        enabled: bool,
    },
    Dig {
        block: BlockPos,
        face: u8,
        stage: DigStage,
    },
    Place {
        block: BlockPos,
        face: u8,
        hand: Hand,
        hit: [f32; 3],
    },
    UseItem {
        hand: Hand,
    },
    UseBlock {
        block: BlockPos,
        face: u8,
        hand: Hand,
        hit: [f32; 3],
    },
    Interact {
        entity: i32,
        hand: Hand,
    },
    Attack {
        entity: i32,
    },
    SelectSlot {
        slot: u8,
    },
    ClickSlot {
        menu_id: i32,
        state_id: i32,
        slot: i16,
        button: u8,
        mode: u8,
    },
    CloseMenu {
        menu_id: i32,
    },
    Craft {
        recipe: String,
        menu_id: i32,
        make_all: bool,
    },
    Chat {
        message: String,
    },
    Command {
        command: String,
    },
    Respawn,
    /// Only operator-installed adapters may implement a namespaced capability.
    Extension {
        capability: String,
        input: Value,
    },
}
impl Action {
    pub fn capability(&self) -> &str {
        match self {
            Self::Move { .. }
            | Self::Look { .. }
            | Self::Jump
            | Self::Sprint { .. }
            | Self::Sneak { .. } => "minecraft:movement",
            Self::Dig { .. } => "minecraft:mining",
            Self::Place { .. } => "minecraft:placement",
            Self::UseBlock { .. } => "minecraft:block_use",
            Self::UseItem { .. } => "minecraft:item_use",
            Self::Interact { .. } | Self::Attack { .. } => "minecraft:entities",
            Self::SelectSlot { .. } | Self::ClickSlot { .. } | Self::CloseMenu { .. } => {
                "minecraft:inventory"
            }
            Self::Craft { .. } => "minecraft:crafting",
            Self::Chat { .. } => "minecraft:chat",
            Self::Command { .. } => "minecraft:commands",
            Self::Respawn => "minecraft:respawn",
            Self::Extension { capability, .. } => capability,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let valid = match self {
            Self::Move { target, .. } => [target.x, target.y, target.z]
                .iter()
                .all(|x| x.is_finite() && x.abs() <= 30_000_000.),
            Self::Look { yaw, pitch } => yaw.is_finite() && pitch.is_finite() && pitch.abs() <= 90.,
            Self::Dig { face, .. } => *face <= 5,
            Self::Place { face, hit, .. } | Self::UseBlock { face, hit, .. } => {
                *face <= 5 && hit.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            }
            Self::SelectSlot { slot } => *slot < 9,
            Self::ClickSlot {
                menu_id,
                state_id,
                button,
                mode,
                ..
            } => *menu_id >= 0 && *state_id >= 0 && *button <= 8 && *mode <= 6,
            Self::CloseMenu { menu_id } | Self::Craft { menu_id, .. } => *menu_id >= 0,
            Self::Chat { message } => {
                !message.is_empty()
                    && message.len() <= 256
                    && !message.starts_with('/')
                    && !message.chars().any(char::is_control)
            }
            Self::Command { command } => {
                !command.is_empty()
                    && command.len() <= 256
                    && !command.starts_with('/')
                    && !command.chars().any(char::is_control)
            }
            Self::Extension { capability, input } => {
                registry_id(capability) && input.to_string().len() <= 8192
            }
            Self::Attack { entity } | Self::Interact { entity, .. } => *entity >= 0,
            _ => true,
        };
        if !valid {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid player action",
            ));
        }
        if let Self::Craft { recipe, .. } = self
            && !registry_id(recipe)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Use a runtime recipe registry ID",
            ));
        }
        Ok(())
    }
}
pub fn registry_id(s: &str) -> bool {
    let Some((namespace, path)) = s.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && s.len() <= 256
        && namespace
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
        && path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_/.-".contains(&b))
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub action_id: String,
    pub action: Action,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Prepared,
    Dispatched,
    Observed,
    Rejected,
    Uncertain,
    Reconciled,
    Imported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub action_id: String,
    pub outcome: Outcome,
    pub adapter_version: String,
    pub evidence: Value,
    pub error: Option<ApiError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub revision: String,
    pub player: Value,
    pub world: Value,
    pub menu: Value,
}

/// Trusted, length-bounded JSON-line process interface, selected by the operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterRequest {
    pub protocol: u32,
    pub instance_id: String,
    pub bot_id: String,
    pub player_uuid: String,
    pub player_name: String,
    pub operation: AdapterOperation,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdapterOperation {
    Discover,
    Connect,
    Disconnect,
    Observe,
    Action { request: ActionRequest },
    Reconcile { action_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AdapterResponse {
    Info(ServerInfo),
    Connected,
    Disconnected,
    Observation(Observation),
    Receipt(Receipt),
    Error(ApiError),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_forged_actor_and_privileged_operations() {
        for value in [
            r#"{"kind":"chat","message":"hello","actor":"victim"}"#,
            r#"{"kind":"operator_bot_reset"}"#,
            r#"{"kind":"raw_packet","packet":12}"#,
        ] {
            assert!(serde_json::from_str::<Action>(value).is_err());
        }
        assert!(
            Action::Chat {
                message: "/op intruder".into()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn runtime_registry_names_and_bounds() {
        assert!(registry_id("some_mod:a_new_machine"));
        assert!(!registry_id("../../host"));
        assert!(
            Action::Move {
                target: Position {
                    x: f64::NAN,
                    y: 0.,
                    z: 0.
                },
                on_ground: false,
                jump: false,
            }
            .validate()
            .is_err()
        );
    }
}
