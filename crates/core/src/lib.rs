//! Version-independent contracts. Registry names and advertised capabilities are runtime data.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const API_VERSION: u32 = 1;
pub const ADAPTER_VERSION: u32 = 2;

/// Native arrival contract in blocks, shared by adapters and navigation clients.
/// This says nothing about dispatch, identity or receipt authenticity.
pub const MOVEMENT_TARGET_TOLERANCE: f64 = 0.15;
pub fn movement_target_reached(actual: [f64; 3], target: [f64; 3]) -> bool {
    actual.into_iter().chain(target).all(f64::is_finite)
        && (0..3).map(|i| (actual[i] - target[i]).powi(2)).sum::<f64>()
            < MOVEMENT_TARGET_TOLERANCE.powi(2)
}

/// Stable click-intent codes. The version adapter owns protocol encoding.
pub mod click_mode {
    pub const PICKUP: u8 = 0;
    pub const QUICK_MOVE: u8 = 1;
    pub const SWAP: u8 = 2;
    pub const CLONE: u8 = 3;
    pub const THROW: u8 = 4;
    pub const QUICK_CRAFT: u8 = 5;
    pub const PICKUP_ALL: u8 = 6;
}
fn deserialize_click_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u8, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Code(u8),
        Name(String),
    }
    let code = match Input::deserialize(deserializer)? {
        Input::Code(code) => Some(code),
        Input::Name(name) => match name.as_str() {
            "pickup" => Some(click_mode::PICKUP),
            "quick_move" => Some(click_mode::QUICK_MOVE),
            "swap" => Some(click_mode::SWAP),
            "clone" => Some(click_mode::CLONE),
            "throw" => Some(click_mode::THROW),
            "quick_craft" => Some(click_mode::QUICK_CRAFT),
            "pickup_all" => Some(click_mode::PICKUP_ALL),
            _ => None,
        },
    };
    code.filter(|code| *code <= 6).ok_or_else(|| {
        serde::de::Error::custom("Expected a click code0..6 or a supported snake_case mode name")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    Revoked,
    Conflict,
    /// Container-local arbiter refused before forwarding any gameplay request.
    LocalPreempted,
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
    /// A bounded waypoint control; inspect its durable action ID while running.
    StartMove {
        target: Position,
        #[serde(default)]
        jump: bool,
        max_ticks: u16,
    },
    /// Cross a supported, level-ground waypoint without braking to rest.
    /// Passage and released input require native evidence; passive momentum
    /// continues after release. Discover the adapter capability before use.
    StartMoveThrough {
        target: Position,
        max_ticks: u16,
    },
    CancelControl {
        action_id: String,
    },
    MotionFrame {
        target: Position,
    },
    DropSelected {
        entire_stack: bool,
    },
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
    /// Hold item use for a finite lease; completion and release are observed separately.
    StartUseItem {
        hand: Hand,
        max_ticks: u16,
    },
    CancelUseItem {
        action_id: String,
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
        #[serde(deserialize_with = "deserialize_click_mode")]
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
            Self::StartMove { .. } | Self::CancelControl { .. } => "minecraft:finite_locomotion",
            Self::StartMoveThrough { .. } => "minecraft:waypoint_passthrough",
            Self::MotionFrame { .. } => "minecraft:movement_frames",
            Self::DropSelected { .. } => "minecraft:inventory",
            Self::Move { .. }
            | Self::Look { .. }
            | Self::Jump
            | Self::Sprint { .. }
            | Self::Sneak { .. } => "minecraft:movement",
            Self::Dig { .. } => "minecraft:mining",
            Self::Place { .. } => "minecraft:placement",
            Self::UseBlock { .. } => "minecraft:block_use",
            Self::UseItem { .. } => "minecraft:item_use",
            Self::StartUseItem { .. } | Self::CancelUseItem { .. } => "minecraft:finite_item_use",
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
            Self::StartMove {
                target, max_ticks, ..
            }
            | Self::StartMoveThrough { target, max_ticks } => {
                *max_ticks > 0
                    && [target.x, target.y, target.z]
                        .iter()
                        .all(|x| x.is_finite() && x.abs() <= 30_000_000.)
            }
            Self::CancelControl { action_id } => valid_id(action_id),
            Self::CancelUseItem { action_id } => valid_id(action_id),
            Self::StartUseItem { max_ticks, .. } => *max_ticks > 0,
            Self::Move { target, .. } | Self::MotionFrame { target } => {
                [target.x, target.y, target.z]
                    .iter()
                    .all(|x| x.is_finite() && x.abs() <= 30_000_000.)
            }
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
    Running,
    Observed,
    Cancelled,
    Expired,
    Failed,
    Rejected,
    Uncertain,
    Reconciled,
    Imported,
}
impl Outcome {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Observed
                | Self::Cancelled
                | Self::Expired
                | Self::Failed
                | Self::Rejected
                | Self::Reconciled
                | Self::Imported
        )
    }
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
/// Own pending journal entries. Imported legacy intents may only have a digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAction {
    pub action: Option<Action>,
    pub receipt: Receipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub revision: String,
    pub player: Value,
    pub world: Value,
    pub menu: Value,
}
pub const LOCAL_CONTROL_VERSION: u32 = 2;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalActionRequest {
    pub version: u32,
    pub request: ActionRequest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum LocalActionResponse {
    Receipt(Receipt),
    Error(ApiError),
}

/// A read-only query to an operator-installed, namespaced observation provider.
/// The gateway supplies the authenticated player identity; input never selects an actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationQuery {
    pub capability: String,
    pub input: Value,
}
impl ObservationQuery {
    pub fn validate(&self) -> Result<()> {
        if !registry_id(&self.capability)
            || !self.input.is_object()
            || serde_json::to_vec(&self.input).map_or(true, |v| v.len() > 8192)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Invalid observation query",
            ));
        }
        Ok(())
    }
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
    ReadObservation { request: ObservationQuery },
    Action { request: ActionRequest },
    ActionStatus { action_id: String },
    Reconcile { action_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum AdapterResponse {
    Info(ServerInfo),
    Connected,
    Disconnected,
    Observation(Observation),
    ObservationData(Value),
    Receipt(Receipt),
    Error(ApiError),
}

/// Correlated process messages after the initial bare Discover handshake. This
/// is trusted operator-selected IPC, never a public gameplay request format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterFrameRequest {
    pub protocol: u32,
    pub channel_id: String,
    pub request_id: u64,
    pub request: AdapterRequest,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterFrameResponse {
    pub protocol: u32,
    pub channel_id: String,
    pub request_id: u64,
    pub instance_id: String,
    pub bot_id: String,
    pub player_uuid: String,
    pub response: AdapterResponse,
}

#[cfg(test)]
mod tests {
    #[test]
    fn waypoint_passage_is_finite_capability_scoped_and_cannot_select_an_actor() {
        let mut value = serde_json::json!({"kind":"start_move_through","target":{"x":2.5,"y":1.,"z":0.5},"max_ticks":60});
        let action: super::Action = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(action.capability(), "minecraft:waypoint_passthrough");
        action.validate().unwrap();
        value["max_ticks"] = serde_json::json!(0);
        assert!(
            serde_json::from_value::<super::Action>(value.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        value["max_ticks"] = serde_json::json!(60);
        value["actor"] = serde_json::json!("victim");
        assert!(serde_json::from_value::<super::Action>(value).is_err());
    }
    #[test]
    fn named_click_modes_have_one_numeric_intent_and_reject_unknown_inputs() {
        let names = [
            "pickup",
            "quick_move",
            "swap",
            "clone",
            "throw",
            "quick_craft",
            "pickup_all",
        ];
        for (code, name) in names.iter().enumerate() {
            let named = serde_json::json!({"kind":"click_slot","menu_id":1,"state_id":7,"slot":3,"button":0,"mode":name});
            let action: super::Action = serde_json::from_value(named.clone()).unwrap();
            let mut numeric = named.clone();
            numeric["mode"] = serde_json::json!(code);
            assert_eq!(serde_json::to_value(action).unwrap(), numeric);
        }
        for mode in [
            serde_json::json!("operator"),
            serde_json::json!(7),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!({"code":1}),
        ] {
            assert!(serde_json::from_value::<super::Action>(serde_json::json!({"kind":"click_slot","menu_id":1,"state_id":7,"slot":3,"button":0,"mode":mode})).is_err());
        }
    }
    use super::*;
    #[test]
    fn observation_query_cannot_select_actor_or_plugin_code() {
        assert!(
            serde_json::from_str::<ObservationQuery>(
                r#"{"capability":"demo:view","input":{},"player_uuid":"victim"}"#
            )
            .is_err()
        );
        for (capability, input) in [
            ("../host", serde_json::json!({})),
            ("demo:view", serde_json::json!("path")),
            ("demo:view", serde_json::json!({"text":"x".repeat(8193)})),
        ] {
            assert!(
                ObservationQuery {
                    capability: capability.into(),
                    input
                }
                .validate()
                .is_err()
            );
        }
    }
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
