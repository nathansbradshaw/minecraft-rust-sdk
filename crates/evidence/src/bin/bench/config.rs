use super::*;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pinned {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub file: Pinned,
    pub container_path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorRcon {
    pub address: std::net::SocketAddr,
    pub password_file: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub version: u32,
    pub case_id: String,
    pub instance_id: String,
    pub player_uuid: String,
    pub npc_uuid: String,
    pub connection_request_id: String,
    pub duration_secs: u64,
    pub source_revision: String,
    pub container: String,
    pub container_id: Option<String>,
    pub container_started_at: Option<String>,
    #[serde(default)]
    pub owned_container: bool,
    pub bot_dev: Option<Pinned>,
    pub workspace: PathBuf,
    pub container_workspace: String,
    pub secret_dir: PathBuf,
    pub container_secret_dir: String,
    pub profile_name: String,
    pub operator_profile: Option<PathBuf>,
    pub model_key_name: Option<String>,
    pub allocated_model_actor_uuid: Option<String>,
    pub allocated_model_kind: Option<String>,
    pub ca_file: Option<Artifact>,
    pub sdk: Pinned,
    pub docker: Pinned,
    pub evidence: Pinned,
    pub artifacts: Vec<Artifact>,
    pub handler: String,
    pub runtime_check: Option<String>,
    pub handler_args: Vec<String>,
    pub expected_player_position: [f64; 3],
    pub expected_npc_position: [f64; 3],
    pub position_tolerance: f64,
    pub expected_health: f64,
    pub expected_food: u64,
    pub trace_action_prefix: Option<String>,
    pub operator_rcon: Option<OperatorRcon>,
}
impl Pinned {
    pub fn verify(&self) -> Result<()> {
        canonical(&self.path)?;
        ensure(
            valid_hash(&self.sha256, 64) && hash_file(&self.path)? == self.sha256,
            "pinned artifact differs",
        )
    }
}
impl Fixture {
    pub fn load(path: &Path, expected: &str) -> Result<Self> {
        let bytes = read(path, 65536)?;
        ensure(
            valid_hash(expected, 64) && sha256(&bytes) == expected,
            "fixture hash differs",
        )?;
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "fixture JSON does not match bench schema")?;
        value.validate()?;
        Ok(value)
    }
    pub fn profile(&self) -> PathBuf {
        self.secret_dir.join(&self.profile_name)
    }
    pub fn model_key(&self) -> Option<PathBuf> {
        self.model_key_name
            .as_ref()
            .map(|name| self.secret_dir.join(name))
    }
    pub fn container_profile(&self) -> String {
        format!("{}/{}", self.container_secret_dir, self.profile_name)
    }
    pub fn host_profile(&self) -> Result<&Path> {
        self.operator_profile
            .as_deref()
            .ok_or_else(|| "operator profile outside bot mounts required".into())
    }
    pub fn target(&self) -> Result<&str> {
        self.container_id
            .as_deref()
            .ok_or_else(|| "frozen owned container ID required".into())
    }
    pub fn state_path(&self) -> Result<String> {
        let artifact = self
            .artifacts
            .iter()
            .find(|a| a.container_path == self.handler_args[0])
            .ok_or("config artifact absent")?;
        let value = json(&artifact.file.path, 65536)?;
        let state = value["state"]
            .as_str()
            .ok_or("pinned runtime state path required")?;
        container_path(state)?;
        ensure(
            (Path::new(state).starts_with(&self.container_workspace)
                || Path::new(state).starts_with("/tmp"))
                && !Path::new(state).starts_with(&self.container_secret_dir)
                && !Path::new(&self.container_secret_dir).starts_with(state),
            "runtime state must be a scoped workspace/tmp path outside secrets",
        )?;
        Ok(state.into())
    }
    pub fn validate(&self) -> Result<()> {
        ensure(
            self.version == 1
                && id(&self.case_id)
                && id(&self.container)
                && id(&self.connection_request_id),
            "invalid case/container/request identity",
        )?;
        ensure(
            self.owned_container
                && self
                    .container_id
                    .as_deref()
                    .is_some_and(|id| valid_hash(id, 64))
                && self
                    .container_started_at
                    .as_deref()
                    .is_some_and(|s| s.len() <= 40 && s.ends_with('Z') && s.contains('T')),
            "frozen exclusively-owned container ID/start required",
        )?;
        self.bot_dev
            .as_ref()
            .ok_or("pinned bot-dev required")?
            .verify()?;
        ensure(
            uuid(&self.player_uuid)
                && uuid(&self.npc_uuid)
                && self.player_uuid != self.npc_uuid
                && valid_hash(&self.instance_id, 64)
                && valid_hash(&self.source_revision, 40),
            "invalid frozen identities",
        )?;
        ensure(
            self.duration_secs == 120
                && self.expected_health == 20.0
                && self.expected_food == 20
                && self.position_tolerance.is_finite()
                && (0.001..=0.5).contains(&self.position_tolerance)
                && self.expected_player_position.iter().all(|p| p.is_finite()),
            "bounded healthy fixture required",
        )?;
        ensure(
            id(&self.profile_name)
                && self.model_key_name.as_deref().is_none_or(id)
                && self.model_key_name.as_ref() != Some(&self.profile_name),
            "secret filenames required",
        )?;
        ensure(
            self.allocated_model_kind
                .as_deref()
                .is_none_or(|k| k == "operator-allocated-proxy")
                && self
                    .allocated_model_actor_uuid
                    .as_deref()
                    .is_none_or(|u| u == self.player_uuid),
            "explicit operator-attested model allocation required",
        )?;
        canonical(&self.workspace)?;
        canonical(self.host_profile()?)?;
        ensure(
            !self.host_profile()?.starts_with(&self.workspace)
                && !self.host_profile()?.starts_with(&self.secret_dir),
            "operator profile must be outside bot workspace and secret mount",
        )?;
        directory(&self.secret_dir)?;
        canonical(&self.secret_dir)?;
        ensure(
            !self.secret_dir.starts_with(&self.workspace)
                && !self.workspace.starts_with(&self.secret_dir),
            "secrets must be outside writable/model workspace",
        )?;
        for name in fs::read_dir(&self.secret_dir)? {
            let name = name?.file_name();
            ensure(
                name == self.profile_name.as_str()
                    || self
                        .model_key_name
                        .as_ref()
                        .is_some_and(|m| name == m.as_str())
                    || self
                        .ca_file
                        .as_ref()
                        .is_some_and(|ca| ca.file.path == self.secret_dir.join(&name)),
                "secret mount contains unrelated files",
            )?;
        }
        for path in [&self.container_workspace, &self.container_secret_dir] {
            container_path(path)?;
        }
        ensure(
            !Path::new(&self.container_secret_dir).starts_with(&self.container_workspace)
                && !Path::new(&self.container_workspace).starts_with(&self.container_secret_dir),
            "container secret mount overlaps workspace",
        )?;
        self.sdk.verify()?;
        self.docker.verify()?;
        self.evidence.verify()?;
        if let Some(ca) = &self.ca_file {
            ca.file.verify()?;
            container_path(&ca.container_path)?;
            let bytes = fs::read(&ca.file.path)?;
            ensure(
                bytes.len() <= 1024 * 1024
                    && bytes
                        .windows(b"-----BEGIN CERTIFICATE-----".len())
                        .any(|w| w == b"-----BEGIN CERTIFICATE-----")
                    && !bytes
                        .windows(b"PRIVATE KEY".len())
                        .any(|w| w == b"PRIVATE KEY"),
                "pinned public CA certificate bundle required",
            )?;
            if ca.file.path.starts_with(&self.secret_dir) {
                ensure(
                    ca.file.path.parent() == Some(&self.secret_dir)
                        && ca.container_path
                            == format!(
                                "{}/{}",
                                self.container_secret_dir,
                                ca.file
                                    .path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .ok_or("CA filename invalid")?
                            ),
                    "CA bundle mount differs",
                )?;
            }
        }
        for tool in [&self.sdk, &self.docker, &self.evidence]
            .into_iter()
            .chain(self.bot_dev.iter())
        {
            ensure(
                !tool.path.starts_with(&self.workspace) && !tool.path.starts_with(&self.secret_dir),
                "operator tools must be outside bot workspace and secrets",
            )?;
        }
        ensure(
            !self.artifacts.is_empty() && self.artifacts.len() <= 16,
            "bounded reviewed artifacts required",
        )?;
        let mut paths = std::collections::BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.file.verify()?;
            container_path(&artifact.container_path)?;
            ensure(
                paths.insert(&artifact.container_path)
                    && !Path::new(&artifact.container_path).starts_with(&self.container_secret_dir),
                "artifact must be distinct from secret paths",
            )?;
        }
        ensure(
            paths.contains(&self.handler)
                && self.handler_args.len() <= 16
                && self
                    .handler_args
                    .iter()
                    .all(|a| !a.is_empty() && a.len() <= 512 && !a.contains(['\n', '\r', '\0'])),
            "reviewed handler and bounded argv required",
        )?;
        if let Some(check) = &self.runtime_check {
            container_path(check)?;
            ensure(
                paths.contains(check) && check != &self.handler,
                "runtime filesystem checker must be a distinct pinned artifact",
            )?;
        }
        ensure(
            self.trace_action_prefix.as_deref().is_none_or(id),
            "invalid optional exact action prefix",
        )?;
        ensure(
            self.expected_npc_position.iter().all(|p| p.is_finite()),
            "finite NPC pose required",
        )?;
        ensure(
            self.handler_args.len() == 4
                && matches!(self.handler_args[1].as_str(), "blocked" | "task")
                && self.handler_args[2] == "120",
            "existing bounded probe argv required",
        )?;
        ensure(
            self.handler_args[1] != "task"
                || (self.model_key_name.is_some()
                    && self.allocated_model_kind.as_deref() == Some("operator-allocated-proxy")),
            "task mode requires an explicitly allocated proxy key",
        )?;
        for argument in [&self.handler_args[0], &self.handler_args[3]] {
            ensure(
                paths.contains(argument),
                "config and admission spec must be pinned artifacts",
            )?;
        }
        let config = self
            .artifacts
            .iter()
            .find(|a| a.container_path == self.handler_args[0])
            .ok_or("config artifact absent")?;
        let cfg = json(&config.file.path, 65536)?;
        ensure(
            cfg["profile"] == self.container_profile()
                && self
                    .model_key_name
                    .as_ref()
                    .map_or(cfg["model_key"] == "", |name| {
                        cfg["model_key"] == format!("{}/{}", self.container_secret_dir, name)
                    })
                && cfg["workspace"]
                    .as_str()
                    .is_some_and(|p| Path::new(p).starts_with(&self.container_workspace))
                && cfg["body"]["period_ms"] == 150
                && cfg["body"]["decision_samples"] == 128
                && cfg["body"]["enabled"] == false,
            "handler config must bind scoped secrets, workspace and pre-admission body",
        )?;
        let admission = self
            .artifacts
            .iter()
            .find(|a| a.container_path == self.handler_args[3])
            .ok_or("admission artifact absent")?;
        let spec = json(&admission.file.path, 16384)?;
        ensure(
            spec["entity_uuid"] == self.npc_uuid
                && spec["entity_type"] == "minecraft:skeleton"
                && spec["main_hand"] == "minecraft:bow"
                && spec["entity_health"].as_f64() == Some(20.0)
                && spec["position_tolerance"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=self.position_tolerance).contains(&n))
                && spec["player_health"].as_f64() == Some(20.0)
                && spec["player_food"] == 20
                && spec["timeout_secs"] == 120
                && same_point(&spec["player_position"], self.expected_player_position)
                && same_point(&spec["entity_position"], self.expected_npc_position),
            "admission spec differs from fixture",
        )?;
        if let Some(rcon) = &self.operator_rcon {
            ensure(
                rcon.address.ip().is_loopback(),
                "operator RCON must be loopback",
            )?;
            canonical(&rcon.password_file)?;
            read(&rcon.password_file, 4096)?;
            ensure(
                !rcon.password_file.starts_with(&self.workspace)
                    && !rcon.password_file.starts_with(&self.secret_dir),
                "operator password must be outside workspace and bot secret mount",
            )?;
        }
        self.secrets()?;
        self.state_path()?;
        Ok(())
    }
    pub fn secrets(&self) -> Result<Secrets> {
        let profile = json(&self.profile(), 16384)?;
        let operator = json(self.host_profile()?, 16384)?;
        ensure(
            operator["instance_id"] == profile["instance_id"]
                && operator["credential"] == profile["credential"],
            "operator/internal profiles must have identical instance and scoped credential",
        )?;
        ensure(
            profile["instance_id"] == self.instance_id
                && profile["credential"]["player_uuid"] == self.player_uuid
                && profile["credential"]["bot_id"].as_str().is_some_and(id),
            "profile identity differs",
        )?;
        let key = profile["credential"]["key"]
            .as_str()
            .filter(|s| s.len() >= 16)
            .ok_or("invalid scoped profile key")?;
        let mut values = vec![key.to_owned()];
        if let Some(path) = self.model_key() {
            let model = String::from_utf8(read(&path, 16384)?)?;
            let model = model.trim();
            ensure(
                model.len() >= 16
                    && !model.contains(['\n', '\r'])
                    && !model.starts_with('{')
                    && !["sk-", "sk_", "ghp_", "github_pat_", "eyJ"]
                        .iter()
                        .any(|p| model.starts_with(p)),
                "one allocated proxy key required, not provider account credential",
            )?;
            values.push(model.to_owned());
        }
        if let Some(session) = profile["session"]["session_id"].as_str() {
            values.push(session.to_owned());
        }
        if let Some(session) = operator["session"]["session_id"].as_str() {
            values.push(session.to_owned());
        }
        Ok(Secrets(values))
    }
}
fn container_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    ensure(
        path.is_absolute()
            && path.components().all(|p| {
                matches!(
                    p,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
            && value != "/",
        "absolute canonical container path required",
    )
}
fn same_point(value: &Value, expected: [f64; 3]) -> bool {
    value
        .as_array()
        .is_some_and(|p| p.len() == 3 && p.iter().zip(expected).all(|(v, e)| v.as_f64() == Some(e)))
}
/// Inspect only selected Docker fields, never Config.Env or full inspect output.
pub fn validate_mounts(f: &Fixture, value: &Value) -> Result<()> {
    ensure(
        value["id"].as_str() == f.container_id.as_deref()
            && value["started_at"].as_str() == f.container_started_at.as_deref(),
        "container ID/start differs from frozen owner",
    )?;
    let names = value["env_names"]
        .as_array()
        .ok_or("container environment-name inventory absent")?;
    for name in names {
        let name = name.as_str().ok_or("invalid environment name")?;
        ensure(
            [
                "PATH",
                "HOSTNAME",
                "HOME",
                "USER",
                "TERM",
                "LANG",
                "LANGUAGE",
                "LC_ALL",
                "LC_CTYPE",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "RUSTUP_TOOLCHAIN",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "NO_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
                "no_proxy",
                "SSL_CERT_FILE",
                "SSL_CERT_DIR",
                "MC_SDK_CA_CERT",
            ]
            .contains(&name),
            "unreviewed container environment variable; no credential environment allowed",
        )?;
    }
    ensure(
        value["user"]
            .as_str()
            .is_some_and(|u| !matches!(u.split(':').next(), None | Some("" | "0" | "root"))),
        "non-root configured container user required",
    )?;
    ensure(
        value["running"] == true
            && value["readonly_rootfs"] == true
            && value["privileged"] == false,
        "existing unprivileged read-only-root container required",
    )?;
    ensure(
        value["memory"]
            .as_u64()
            .is_some_and(|n| (1..=3 * 1024 * 1024 * 1024).contains(&n))
            && value["nano_cpus"]
                .as_u64()
                .is_some_and(|n| (1..=2_000_000_000).contains(&n))
            && value["pids"]
                .as_u64()
                .is_some_and(|n| (1..=2048).contains(&n)),
        "bounded container memory/CPU/PID limits required",
    )?;
    ensure(
        value["cap_drop"].as_array().is_some_and(|a| {
            a.iter()
                .any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("ALL")))
        }) && value["cap_add"].as_array().is_none_or(Vec::is_empty)
            && value["security"].as_array().is_some_and(|a| {
                a.iter().any(|v| {
                    matches!(
                        v.as_str(),
                        Some("no-new-privileges" | "no-new-privileges:true")
                    )
                })
            })
            && value["devices"].as_array().is_none_or(Vec::is_empty),
        "drop ALL caps/no-new-privileges/no devices required",
    )?;
    for field in ["network", "pid_mode", "ipc_mode"] {
        ensure(
            value[field]
                .as_str()
                .is_some_and(|s| s != "host" && !s.starts_with("container:")),
            "host/shared container namespace forbidden",
        )?;
    }
    let mounts = value["mounts"].as_array().ok_or("mount inventory absent")?;
    let mut secret = false;
    for mount in mounts {
        let source = mount["Source"].as_str().unwrap_or("");
        let dest = mount["Destination"]
            .as_str()
            .ok_or("mount destination absent")?;
        let rw = mount["RW"].as_bool().ok_or("mount access absent")?;
        ensure(
            source.is_empty() || !f.host_profile()?.starts_with(source),
            "operator profile exposed by bot mount",
        )?;
        if let Some(rcon) = &f.operator_rcon {
            ensure(
                source.is_empty() || !rcon.password_file.starts_with(source),
                "operator password exposed by bot mount",
            )?;
        }
        ensure(
            !dest.contains("docker.sock") && !source.contains("docker.sock"),
            "Docker socket forbidden",
        )?;
        if Path::new(source) == f.secret_dir && dest == f.container_secret_dir {
            ensure(!rw, "secret directory must be read-only")?;
            secret = true;
        } else {
            // No ancestor bind may expose secret files through a second alias.
            ensure(
                source.is_empty()
                    || (!f.secret_dir.starts_with(source)
                        && !Path::new(source).starts_with(&f.secret_dir)),
                "secret alias mount forbidden",
            )?;
            if mount["Type"] == "bind" {
                let artifact =
                    f.artifacts.iter().chain(f.ca_file.iter()).any(|a| {
                        a.file.path == Path::new(source) && a.container_path == dest && !rw
                    });
                ensure(
                    artifact || (Path::new(source) == f.workspace && dest == f.container_workspace),
                    "unreviewed host bind mount forbidden",
                )?;
            } else {
                ensure(
                    mount["Type"] == "tmpfs"
                        && rw
                        && (dest == f.container_workspace || dest == "/tmp" || dest == "/cache"),
                    "unreviewed mount type/path forbidden, including read-only volumes",
                )?;
            }
            ensure(
                !Path::new(&f.container_secret_dir).starts_with(dest)
                    && !Path::new(dest).starts_with(&f.container_secret_dir),
                "mount overlaps secret destination",
            )?;
        }
    }
    ensure(
        secret,
        "dedicated read-only secret directory mount required",
    )
}
