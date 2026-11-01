//! Research-only native request authority; no provider credentials are accessed.
//! A FULL fsync-before-reply journal is held under one stable profile lock.
use codex_core::relay_authority::{NativeAuthority, ShellRoute};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

static HOST: OnceLock<Result<Arc<Host>, String>> = OnceLock::new();
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Root {
    owner: String,
    epoch: u64,
    active: bool,
    root: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    fingerprint: String,
    owner: String,
    epoch: u64,
    thread: Option<String>,
    parent: Option<String>,
    controller_generation: u64,
    host_generation: String,
    bound_thread: Option<String>,
    method: String,
    result: Option<Value>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Controller {
    active: bool,
    lease: String,
    generation: u64,
    host: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    policy_digest: String,
    controllers: BTreeMap<String, Controller>,
    roots: BTreeMap<String, Root>,
    operations: BTreeMap<String, Operation>,
}
struct State {
    store: Store,
    requests: BTreeMap<String, String>,
    connections: BTreeMap<u64, (String, u64)>,
    active_connections: BTreeMap<String, u64>,
    poisoned: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    profile_identity: String,
    enrollment_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shell_realm: Option<ShellRealm>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellRealm {
    manager: PathBuf,
    manager_sha256: String,
    kernel: PathBuf,
    kernel_sha256: String,
    initramfs: PathBuf,
    initramfs_sha256: String,
}
pub(crate) struct Host {
    directory: PathBuf,
    policy: Policy,
    policy_digest: String,
    generation: String,
    _lock: File,
    state: Mutex<State>,
}
pub(crate) enum Admission {
    Execute,
    Reply(Value),
}
fn uuid_generation() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn read_only(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "thread/list"
            | "thread/loaded/list"
            | "thread/read"
            | "thread/turns/list"
            | "thread/items/list"
            | "thread/attachment/list"
            | "config/read"
            | "configRequirements/read"
            | "model/list"
            | "collaborationMode/list"
            | "skills/list"
            | "experimentalFeature/list"
            | "mcpServerStatus/list"
            | "account/read"
            | "account/rateLimits/read"
    )
}
fn mutation(method: &str) -> bool {
    matches!(
        method,
        "thread/start"
            | "thread/resume"
            | "thread/fork"
            | "turn/start"
            | "turn/steer"
            | "turn/interrupt"
            | "thread/shellCommand"
            | "thread/unsubscribe"
            | "thread/name/set"
            | "thread/compact/start"
            | "thread/settings/update"
            | "turn/settings/update"
            | "thread/archive"
            | "thread/unarchive"
    )
}

pub(crate) fn install(home: &Path, store_path: &Path) {
    if let Some(existing) = HOST.get() {
        if let Ok(host) = existing
            && let Ok(mut state) = host.state.lock()
        {
            state.poisoned = true;
        }
        return; // one protected native broker per process; no connection-ID aliasing
    }
    if !home.join("relay-authority-v1").exists() {
        if let Err(error) = codex_core::relay_authority::require_source(store_path) {
            HOST.get_or_init(|| Err(error));
        }
        return;
    }
    HOST.get_or_init(|| {
        let store_home = store_path.parent().ok_or("native store has no parent")?;
        if store_home
            .canonicalize()
            .map_err(|error| error.to_string())?
            != home.canonicalize().map_err(|error| error.to_string())?
        {
            return Err("protected native relocated store unsupported until enrolled".into());
        }
        Host::open(home).and_then(|host| {
            codex_core::relay_authority::install(host.clone())?;
            Ok(host)
        })
    });
}
pub(crate) fn current() -> Result<Option<Arc<Host>>, String> {
    HOST.get().cloned().transpose()
}
pub(crate) fn key(connection: u64, request: &impl Serialize) -> String {
    format!(
        "{connection}:{}",
        serde_json::to_string(request).unwrap_or_default()
    )
}

impl Host {
    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }
    pub(crate) fn classified(&self, method: &str) -> bool {
        read_only(method) || mutation(method) || method == "relay/authority"
    }
    fn identity(path: &Path) -> Result<String, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let stat = fs::metadata(path).map_err(|e| e.to_string())?;
            Ok(format!("{}:{}", stat.dev(), stat.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err("protected native authority unsupported on this platform".into())
        }
    }
    fn verify_profile(&self) -> Result<(), String> {
        if Self::identity(self.directory.parent().ok_or("missing profile")?)?
            != self.policy.profile_identity
        {
            return Err("native profile physical identity changed".into());
        }
        if hash(&fs::read(self.directory.join("policy.json")).map_err(|e| e.to_string())?)
            != self.policy_digest
        {
            return Err("native authority policy changed".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self._lock.metadata().map_err(|e| e.to_string())?;
            let current =
                fs::metadata(self.directory.join("host.lock")).map_err(|e| e.to_string())?;
            if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
                return Err("native authority lock identity changed".into());
            }
        }
        Ok(())
    }
    fn open(home: &Path) -> Result<Arc<Self>, String> {
        let directory = home
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join("relay-authority-v1");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("host.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "another native host owns protected profile".to_string())?;
        let policy_bytes = fs::read(directory.join("policy.json")).map_err(|e| e.to_string())?;
        let policy: Policy = serde_json::from_slice(&policy_bytes).map_err(|e| e.to_string())?;
        if policy.version != 1
            || policy.profile_identity != Self::identity(home)?
            || policy.enrollment_sha256.len() != 64
        {
            return Err("invalid native enrollment/profile policy".into());
        }
        let policy_digest = hash(&policy_bytes);
        let path = directory.join("state.json");
        let store = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("authority journal size limit".into());
            }
            let store: Store = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if store.version != 3 || store.policy_digest != policy_digest {
                return Err("unsupported authority journal".into());
            }
            store
        } else {
            Store {
                version: 3,
                policy_digest: policy_digest.clone(),
                ..Store::default()
            }
        };
        let host = Arc::new(Self {
            directory,
            policy,
            policy_digest,
            generation: uuid_generation(),
            _lock: lock,
            state: Mutex::new(State {
                store,
                requests: BTreeMap::new(),
                connections: BTreeMap::new(),
                active_connections: BTreeMap::new(),
                poisoned: false,
            }),
        });
        {
            let mut state = host.state.lock().map_err(|_| "authority poisoned")?;
            host.persist(&mut state)?;
        }
        Ok(host)
    }
    fn persist(&self, state: &mut State) -> Result<(), String> {
        let result = (|| -> Result<(), String> {
            self.verify_profile()?;
            let bytes = serde_json::to_vec(&state.store).map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("authority journal full".into());
            }
            let tmp = self.directory.join("state.pending");
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(tmp, self.directory.join("state.json")).map_err(|e| e.to_string())?;
            File::open(&self.directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            state.poisoned = true;
        }
        result
    }
    fn root<'a>(
        state: &'a State,
        thread: &str,
        owner: &str,
        epoch: u64,
        cleanup: bool,
    ) -> Result<&'a Root, String> {
        if state.poisoned {
            return Err("authority persistence unavailable".into());
        }
        let binding = state
            .store
            .roots
            .get(thread)
            .ok_or("unowned native thread")?;
        let root = state
            .store
            .roots
            .get(&binding.root)
            .ok_or("missing native root")?;
        if root.owner != owner || root.epoch != epoch || (!root.active && !cleanup) {
            return Err("native ownership refused: foreign, stale or fenced".into());
        }
        Ok(root)
    }
    fn capability(proof: &Value, field: &str) -> Result<String, String> {
        let secret = proof
            .get(field)
            .and_then(Value::as_str)
            .ok_or("missing native capability")?;
        if secret.len() != 64 || !secret.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid native capability".into());
        }
        Ok(hash(secret.as_bytes()))
    }
    fn register_controller(&self, proof: &Value, token: &str) -> Result<Admission, String> {
        let owner = Self::capability(proof, "recoveryKey")?;
        let lease = Self::capability(proof, "key")?;
        if owner == lease {
            return Err("recovery and controller capabilities must differ".into());
        }
        let fingerprint = hash(
            serde_json::to_vec(&json!(["register", &lease]))
                .map_err(|e| e.to_string())?
                .as_slice(),
        );
        let operation_key = format!("{owner}:{token}");
        let mut state = self.state.lock().map_err(|_| "authority poisoned")?;
        if state.poisoned {
            return Err("authority unavailable".into());
        }
        if let Some(operation) = state.store.operations.get(&operation_key) {
            if operation.fingerprint != fingerprint {
                return Err("registration token substitution refused".into());
            }
            return operation
                .result
                .clone()
                .map(Admission::Reply)
                .ok_or("registration unresolved".into());
        }
        if !state.store.controllers.contains_key(&owner)
            && Self::capability(proof, "enrollmentKey").ok().as_deref()
                != Some(self.policy.enrollment_sha256.as_str())
        {
            return Err("supervisor enrollment authority required".into());
        }
        if state
            .store
            .controllers
            .values()
            .any(|controller| controller.lease == lease)
        {
            return Err("recovery requires a fresh unique controller lease".into());
        }
        let generation = state
            .store
            .controllers
            .get(&owner)
            .map(|controller| controller.generation)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("controller generation overflow")?;
        let active = state
            .store
            .controllers
            .get(&owner)
            .map(|controller| controller.active)
            .unwrap_or(true);
        state.store.controllers.insert(
            owner.clone(),
            Controller {
                active,
                lease,
                generation,
                host: self.generation.clone(),
            },
        );
        state.active_connections.remove(&owner);
        let result = json!({"owner":owner,"controllerGeneration":generation,"hostGeneration":self.generation,"accepting":active});
        state.store.operations.insert(
            operation_key,
            Operation {
                fingerprint,
                owner,
                epoch: 0,
                thread: None,
                parent: None,
                controller_generation: generation,
                host_generation: self.generation.clone(),
                bound_thread: None,
                method: "relay/authority".into(),
                result: Some(result.clone()),
            },
        );
        self.persist(&mut state)?;
        Ok(Admission::Reply(result))
    }
    fn operation_current(&self, state: &State, operation: &Operation) -> Result<(), String> {
        let controller = state
            .store
            .controllers
            .get(&operation.owner)
            .ok_or("missing controller")?;
        if !controller.active
            && !matches!(
                operation.method.as_str(),
                "turn/interrupt" | "thread/unsubscribe"
            )
        {
            return Err("native controller fenced".into());
        }
        if controller.generation != operation.controller_generation
            || controller.host != self.generation
            || operation.host_generation != self.generation
        {
            return Err("stale native controller generation".into());
        }
        Ok(())
    }
    pub(crate) fn admit(
        &self,
        connection: u64,
        request_key: &str,
        method: &str,
        params: &Value,
        proof: Option<&Value>,
    ) -> Result<Admission, String> {
        self.verify_profile()?;
        if self
            .state
            .lock()
            .map_err(|_| "authority poisoned")?
            .poisoned
        {
            return Err("authority unavailable".into());
        }
        if read_only(method) {
            return Ok(Admission::Execute);
        }
        if !self.classified(method) {
            return Err("unclassified native operation refused".into());
        }
        let proof = proof.ok_or("native authority proof required")?;
        let token = proof
            .get("token")
            .and_then(Value::as_str)
            .ok_or("missing durable operation token")?;
        if token.is_empty() || token.len() > 128 {
            return Err("invalid operation token".into());
        }
        let command = (method == "relay/authority")
            .then(|| params.get("command").and_then(Value::as_str))
            .flatten();
        if command == Some("register") {
            return self.register_controller(proof, token);
        }
        let epoch = proof
            .get("epoch")
            .and_then(Value::as_u64)
            .ok_or("missing native epoch")?;
        if self.policy.shell_realm.is_some() {
            // This is an explicit experimental capability envelope, never a
            // claim that other native execution paths have been contained.
            if !matches!(
                method,
                "thread/start"
                    | "thread/resume"
                    | "thread/shellCommand"
                    | "turn/interrupt"
                    | "thread/unsubscribe"
                    | "relay/authority"
            ) {
                return Err("research shell realm: unsupported execution/mutation route".into());
            }
            if matches!(method, "thread/start" | "thread/resume")
                && params.as_object().is_some_and(|object| {
                    object.iter().any(|(field, value)| {
                        !value.is_null()
                            && !matches!(
                                field.as_str(),
                                "threadId"
                                    | "model"
                                    | "modelProvider"
                                    | "cwd"
                                    | "approvalPolicy"
                                    | "sandbox"
                            )
                    })
                })
            {
                return Err("research shell realm: unclassified startup override".into());
            }
        }
        if matches!(method, "thread/resume" | "thread/fork")
            && ["path", "history"]
                .iter()
                .any(|field| params.get(*field).is_some_and(|value| !value.is_null()))
        {
            return Err("path/history selection requires exact native identity proof".into());
        }
        let thread = params.get("threadId").and_then(Value::as_str);
        let mut state = self.state.lock().map_err(|_| "authority poisoned")?;
        if state.poisoned {
            return Err("authority unavailable".into());
        }
        if state.requests.contains_key(request_key) {
            return Err("native request ID still in flight".into());
        }
        let supervisory = matches!(command, Some("status" | "revoke" | "operationStatus"))
            && proof.get("recoveryKey").is_some();
        let (owner, generation) = if supervisory {
            let owner = Self::capability(proof, "recoveryKey")?;
            let controller = state
                .store
                .controllers
                .get(&owner)
                .ok_or("unknown supervisor")?;
            (owner, controller.generation)
        } else {
            let lease = Self::capability(proof, "key")?;
            let (owner, controller) = state
                .store
                .controllers
                .iter()
                .find(|(_, controller)| controller.lease == lease)
                .ok_or("unknown or retired native controller lease")?;
            if !(controller.active
                || matches!(method, "turn/interrupt" | "thread/unsubscribe")
                || matches!(command, Some("status" | "revoke" | "operationStatus")))
            {
                return Err("native controller fenced".into());
            }
            if controller.host != self.generation {
                return Err("host restarted; supervisor recovery required".into());
            }
            (owner.clone(), controller.generation)
        };
        if command == Some("operationStatus") {
            let token = params
                .get("operationToken")
                .and_then(Value::as_str)
                .ok_or("missing operation token")?;
            if token.is_empty() || token.len() > 128 {
                return Err("invalid operation token".into());
            }
            let operation = state.store.operations.get(&format!("{owner}:{token}"));
            let result = match operation {
                Some(operation) => {
                    json!({"state":if operation.result.is_some(){"COMMITTED"}else{"UNRESOLVED"},"method":operation.method,"result":operation.result,"reservedThreadId":operation.bound_thread,"hostGeneration":self.generation})
                }
                None => json!({"state":"NOT_FOUND","hostGeneration":self.generation}),
            };
            return Ok(Admission::Reply(result));
        }
        let cleanup = matches!(method, "turn/interrupt" | "thread/unsubscribe")
            || matches!(command, Some("status" | "revoke" | "operationStatus"));
        if let Some(thread) = thread {
            Self::root(&state, thread, &owner, epoch, cleanup)?;
        } else if method != "thread/start" {
            return Err("exact owned native thread required".into());
        }
        if method == "thread/start" && epoch != 1 {
            return Err("new root starts at epoch one".into());
        }
        if command == Some("status") {
            let root = state
                .store
                .roots
                .get(thread.ok_or("missing root")?)
                .ok_or("missing root")?;
            let root = state.store.roots.get(&root.root).ok_or("missing root")?;
            return Ok(Admission::Reply(
                json!({"threadId":root.root,"state":if root.active {"ACTIVE"} else {"FENCED"},"epoch":root.epoch,"controllerGeneration":generation,"hostGeneration":self.generation,"executionQuiescent":false,"handoffPermitted":false}),
            ));
        }
        if method != "relay/authority" {
            if proof.get("recoveryKey").is_some() {
                return Err("supervisor capability cannot drive a conversation".into());
            }
            if let Some((bound_owner, bound_generation)) = state.connections.get(&connection)
                && (bound_owner != &owner || *bound_generation != generation)
            {
                return Err("stale or foreign native connection generation".into());
            }
            if let Some(active) = state.active_connections.get(&owner)
                && *active != connection
            {
                return Err(
                    "controller already attached; explicit supervisor recovery required".into(),
                );
            }
            state
                .connections
                .insert(connection, (owner.clone(), generation));
            state.active_connections.insert(owner.clone(), connection);
        }
        let supplied_parent = proof.get("parentRoot").and_then(Value::as_str);
        let parent = supplied_parent.or_else(|| {
            (method == "thread/fork"
                && params.get("ephemeral").and_then(Value::as_bool) == Some(true))
            .then_some(thread)
            .flatten()
        });
        if let Some(parent) = parent {
            if supplied_parent.is_some()
                && (method != "thread/start"
                    || params.get("ephemeral").and_then(Value::as_bool) != Some(true))
            {
                return Err("parentRoot is reserved for ephemeral creation".into());
            }
            Self::root(&state, parent, &owner, epoch, false)?;
        }
        if method == "thread/start"
            && params.get("ephemeral").and_then(Value::as_bool) == Some(true)
            && parent.is_none()
        {
            return Err("ephemeral creation requires exact owned parentRoot".into());
        }
        let fingerprint = hash(
            serde_json::to_vec(&json!([method, params, epoch, parent]))
                .map_err(|e| e.to_string())?
                .as_slice(),
        );
        let operation_key = format!("{owner}:{token}");
        if let Some(old) = state.store.operations.get(&operation_key) {
            if old.fingerprint != fingerprint {
                return Err("operation token substitution refused".into());
            }
            return old
                .result
                .clone()
                .map(Admission::Reply)
                .ok_or("native operation unresolved; never retry execution".into());
        }
        if method == "relay/authority" {
            if command != Some("revoke") {
                return Err("executor authority unavailable; native handoff refused".into());
            }
            if self.policy.shell_realm.is_some() {
                // Fence is published before changing journal admission. Any
                // partial failure can only close execution, never reopen it.
                let directory = self.directory.join("realm-fences");
                fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
                let fence = directory.join(&owner);
                let file = File::create(fence).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                File::open(directory)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            let root = state
                .store
                .roots
                .get(thread.ok_or("missing root")?)
                .ok_or("missing root")?
                .root
                .clone();
            state
                .store
                .controllers
                .get_mut(&owner)
                .ok_or("missing controller")?
                .active = false;
            for binding in state
                .store
                .roots
                .values_mut()
                .filter(|binding| binding.owner == owner)
            {
                binding.active = false;
            }

            let result = json!({"state":"FENCED","threadId":root,"epoch":epoch,"hostGeneration":self.generation,"executionQuiescent":false,"handoffPermitted":false});
            state.store.operations.insert(
                operation_key,
                Operation {
                    fingerprint,
                    owner,
                    epoch,
                    thread: Some(root),
                    parent: None,
                    controller_generation: generation,
                    host_generation: self.generation.clone(),
                    bound_thread: None,
                    method: method.into(),
                    result: Some(result.clone()),
                },
            );
            self.persist(&mut state)?;
            return Ok(Admission::Reply(result));
        }
        state.store.operations.insert(
            operation_key.clone(),
            Operation {
                fingerprint,
                owner,
                epoch,
                thread: thread.map(str::to_owned),
                parent: parent.map(str::to_owned),
                controller_generation: generation,
                host_generation: self.generation.clone(),
                bound_thread: None,
                method: method.into(),
                result: None,
            },
        );
        state.requests.insert(request_key.to_owned(), operation_key);
        self.persist(&mut state)?;
        Ok(Admission::Execute)
    }
    pub(crate) fn prepared(&self, key: &str, method: &str, params: &Value) -> Result<bool, String> {
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        let Some(operation) = state
            .requests
            .get(key)
            .and_then(|key| state.store.operations.get(key))
        else {
            return Ok(false);
        };
        self.operation_current(&state, operation)?;
        let fingerprint = hash(
            serde_json::to_vec(&json!([method, params, operation.epoch, operation.parent]))
                .map_err(|e| e.to_string())?
                .as_slice(),
        );
        if fingerprint != operation.fingerprint {
            return Err("typed request does not match native intent".into());
        }
        Ok(true)
    }
    pub(crate) fn recheck(&self, key: &str) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        if state.poisoned {
            return Err("authority unavailable".into());
        }
        if let Some(operation) = state
            .requests
            .get(key)
            .and_then(|key| state.store.operations.get(key))
        {
            self.operation_current(&state, operation)?;
            if let Some(thread) = operation.thread.as_ref().or(operation.parent.as_ref()) {
                Self::root(
                    &state,
                    thread,
                    &operation.owner,
                    operation.epoch,
                    matches!(
                        operation.method.as_str(),
                        "turn/interrupt" | "thread/unsubscribe"
                    ),
                )?;
            }
        }
        Ok(())
    }
    pub(crate) fn complete(
        &self,
        request_key: &str,
        method: &str,
        result: Value,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "authority poisoned")?;
        if let Some(key) = state.requests.get(request_key).cloned() {
            let operation = state.store.operations.get(&key).ok_or("missing intent")?;
            if operation.method != method {
                return Err("native response method does not match intent".into());
            }
            let expected = match method {
                "thread/start" | "thread/fork" => operation.bound_thread.as_deref(),
                "thread/resume" => operation.thread.as_deref(),
                _ => None,
            };
            if matches!(method, "thread/start" | "thread/fork" | "thread/resume")
                && (expected.is_none()
                    || result
                        .get("thread")
                        .and_then(|t| t.get("id"))
                        .and_then(Value::as_str)
                        != expected)
            {
                return Err("native response identity does not match durable binding".into());
            }
            state
                .store
                .operations
                .get_mut(&key)
                .ok_or("missing intent")?
                .result = Some(result);
            self.persist(&mut state)?;
            state.requests.remove(request_key);
        }
        Ok(())
    }
    fn shell_intent_directory(
        &self,
        thread: &str,
        execution: &str,
    ) -> Result<(PathBuf, String), String> {
        self.verify_profile()?;
        uuid::Uuid::parse_str(execution).map_err(|_| "invalid execution identity")?;
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        let binding = state
            .store
            .roots
            .get(thread)
            .ok_or("unowned cancellation")?;
        let directory = self.directory.join("realm-executions").join(execution);
        let bytes = fs::read(directory.join("intent.json")).map_err(|e| e.to_string())?;
        if bytes.len() > 16384 {
            return Err("execution intent size limit".into());
        }
        let intent: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if intent["thread"] != thread
            || intent["root"] != binding.root
            || intent["owner"] != binding.owner
            || intent["epoch"] != binding.epoch
            || intent["hostGeneration"] != self.generation
            || intent["realmID"] != execution
        {
            return Err("execution cancellation identity mismatch".into());
        }
        Ok((directory, binding.root.clone()))
    }

    pub(crate) fn response_allowed(
        &self,
        connection: u64,
        thread: Option<&str>,
    ) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        let (owner, generation) = state
            .connections
            .get(&connection)
            .ok_or("unbound approval controller")?;
        let controller = state
            .store
            .controllers
            .get(owner)
            .ok_or("missing controller")?;
        if !controller.active
            || controller.generation != *generation
            || controller.host != self.generation
        {
            return Err("stale approval controller".into());
        }
        let thread = thread.ok_or("unscoped approval refused")?;
        let binding = state
            .store
            .roots
            .get(thread)
            .ok_or("unowned approval thread")?;
        Self::root(&state, thread, owner, binding.epoch, false).map(|_| ())
    }
}
impl NativeAuthority for Host {
    fn cancellation_grace(&self, thread: &str) -> Option<std::time::Duration> {
        if self.policy.shell_realm.is_some()
            && self.state.lock().ok()?.store.roots.contains_key(thread)
        {
            Some(std::time::Duration::from_secs(6))
        } else {
            None
        }
    }
    fn shell_route(&self, thread: &str, script: &str) -> Result<Option<ShellRoute>, String> {
        self.verify_profile()?;
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        if state.poisoned {
            return Err("native authority poisoned".into());
        }
        let binding = state.store.roots.get(thread).ok_or("unowned execution")?;
        Self::root(&state, thread, &binding.owner, binding.epoch, false)?;
        let fence = self.directory.join("realm-fences").join(&binding.owner);
        if fence.exists() {
            return Err("native executor owner fenced".into());
        }
        let Some(realm) = &self.policy.shell_realm else {
            return Ok(None);
        };
        if script.len() > 1024 {
            return Err("research guest script limit is 1024 bytes".into());
        }
        for (path, expected) in [
            (&realm.manager, &realm.manager_sha256),
            (&realm.kernel, &realm.kernel_sha256),
            (&realm.initramfs, &realm.initramfs_sha256),
        ] {
            if !path.is_absolute() || path.canonicalize().map_err(|e| e.to_string())? != *path {
                return Err("canonical realm executable/image required".into());
            }
            if hash(&fs::read(path).map_err(|e| e.to_string())?) != *expected {
                return Err("enrolled executor/image digest changed".into());
            }
        }
        let id = uuid_generation();
        let runtimes = self.directory.join("realm-executions");
        fs::create_dir_all(&runtimes).map_err(|e| e.to_string())?;
        let directory = runtimes.join(&id);
        fs::create_dir(&directory).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let request = directory.join("script");
        let mut file = File::create(&request).map_err(|e| e.to_string())?;
        file.write_all(script.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        let generation = state
            .store
            .controllers
            .get(&binding.owner)
            .ok_or("missing controller")?
            .generation;
        let intent = json!({"realmID":id,"thread":thread,"root":binding.root,"owner":binding.owner,"epoch":binding.epoch,"controllerGeneration":generation,"hostGeneration":self.generation,"scope":"USER_SHELL_RAM_ONLY_LINUX_GUEST","handoffPermitted":false});
        let mut file = File::create(directory.join("intent.json")).map_err(|e| e.to_string())?;
        file.write_all(
            serde_json::to_string(&intent)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
        File::open(&directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        File::open(&runtimes)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(Some(ShellRoute {
            execution: id.clone(),
            command: vec![
                realm.manager.to_string_lossy().into_owned(),
                realm.kernel.to_string_lossy().into_owned(),
                realm.initramfs.to_string_lossy().into_owned(),
                binding.root.clone(),
                directory.join("state.json").to_string_lossy().into_owned(),
                fence.to_string_lossy().into_owned(),
                request.to_string_lossy().into_owned(),
                id,
            ],
        }))
    }
    fn cancel_shell(&self, thread: &str, execution: &str) -> Result<(), String> {
        let (directory, _) = self.shell_intent_directory(thread, execution)?;
        let file = File::create(directory.join("cancel")).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        File::open(directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }
    fn shell_stopped(&self, thread: &str, execution: &str) -> Result<bool, String> {
        let (directory, root) = self.shell_intent_directory(thread, execution)?;
        let Ok(bytes) = fs::read(directory.join("state.json")) else {
            return Ok(false);
        };
        if bytes.len() > 16384 {
            return Err("realm state size limit".into());
        }
        let state: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        Ok(state["realmID"] == execution
            && state["label"] == root
            && state["admissionClosed"] == true
            && state["vmStopped"] == true
            && state["vmRunning"] == false
            && matches!(state["status"].as_str(), Some("STOPPED" | "GUEST_STOPPED")))
    }
    fn check_home(&self, home: &Path) -> Result<(), String> {
        self.verify_profile()?;
        if Self::identity(home)? != self.policy.profile_identity {
            return Err("native authority cannot cross profile identity".into());
        }
        Ok(())
    }
    fn register(
        &self,
        thread: &str,
        parent: Option<&str>,
        request: Option<&str>,
    ) -> Result<(), String> {
        self.verify_profile()?;
        let mut state = self.state.lock().map_err(|_| "authority poisoned")?;
        if state.poisoned {
            return Err("authority unavailable".into());
        }
        let operation = request
            .and_then(|r| state.requests.get(r))
            .and_then(|key| state.store.operations.get(key))
            .cloned();
        if let Some(op) = &operation {
            self.operation_current(&state, op)?;
        }
        if let Some(op) = &operation
            && let Some(thread) = &op.thread
        {
            Self::root(&state, thread, &op.owner, op.epoch, false)?;
        }
        let parent = parent.or_else(|| operation.as_ref().and_then(|op| op.parent.as_deref()));
        let binding = if let Some(parent) = parent {
            let binding = state
                .store
                .roots
                .get(parent)
                .ok_or("unowned internal parent")?
                .clone();
            Self::root(&state, parent, &binding.owner, binding.epoch, false)?;
            if let Some(operation) = &operation
                && operation.owner != binding.owner
            {
                return Err("foreign native parent".into());
            }
            binding
        } else if let Some(operation) = operation {
            if let Some(existing) = state.store.roots.get(thread) {
                Self::root(&state, thread, &operation.owner, operation.epoch, false)?;
                existing.clone()
            } else {
                if !matches!(operation.method.as_str(), "thread/start" | "thread/fork") {
                    return Err("native spawn outside creation intent".into());
                }
                Root {
                    owner: operation.owner,
                    epoch: 1,
                    active: true,
                    root: thread.to_owned(),
                }
            }
        } else {
            return Err("native spawn requires owned intent or exact parent".into());
        };
        state.store.roots.insert(thread.to_owned(), binding);
        if let Some(key) = request
            .and_then(|request| state.requests.get(request))
            .cloned()
            && let Some(operation) = state.store.operations.get_mut(&key)
            && matches!(operation.method.as_str(), "thread/start" | "thread/fork")
            && operation.bound_thread.is_none()
        {
            operation.bound_thread = Some(thread.to_owned());
        }
        self.persist(&mut state)
    }
    fn check(&self, thread: &str) -> Result<(), String> {
        self.verify_profile()?;
        let state = self.state.lock().map_err(|_| "authority poisoned")?;
        let binding = state
            .store
            .roots
            .get(thread)
            .ok_or("unowned core execution")?;
        if self
            .directory
            .join("realm-fences")
            .join(&binding.owner)
            .exists()
        {
            return Err("native execution owner fenced".into());
        }
        Self::root(&state, thread, &binding.owner, binding.epoch, false).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Isolated standard-library fixture; avoids pulling platform RNG packages
    // merely to name a disposable unit-test directory.
    mod tempfile {
        use std::path::{Path, PathBuf};
        pub struct TempDir(PathBuf);
        impl TempDir {
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        pub fn tempdir() -> std::io::Result<TempDir> {
            // Wall-clock nanoseconds collided between parallel test threads on
            // macOS. Reuse the already-linked native UUID generator instead.
            let nonce = uuid::Uuid::now_v7();
            let path = std::env::temp_dir().join(format!(
                "relay-authority-unit-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path)?;
            Ok(TempDir(path))
        }
    }
    fn attach(host: &Host, owner: char, lease: char, token: &str) {
        let proof = json!({"key":lease.to_string().repeat(64),"recoveryKey":format!("{owner}f").repeat(32),"enrollmentKey":"e".repeat(64),"epoch":1,"token":token});
        assert!(matches!(
            host.admit(
                999,
                "register",
                "relay/authority",
                &json!({"command":"register"}),
                Some(&proof)
            ),
            Ok(Admission::Reply(_))
        ));
    }
    fn fixture() -> (tempfile::TempDir, Arc<Host>) {
        fixture_config(false)
    }
    fn fixture_config(realm: bool) -> (tempfile::TempDir, Arc<Host>) {
        let root = tempfile::tempdir().expect("test directory");
        fs::create_dir(root.path().join("relay-authority-v1")).expect("marker");
        let mut policy = json!({"version":1,"profile_identity":Host::identity(root.path()).expect("profile identity"),"enrollment_sha256":hash("e".repeat(64).as_bytes())});
        if realm {
            let mut spec = json!({});
            for name in ["manager", "kernel", "initramfs"] {
                let path = root.path().join(name);
                fs::write(&path, name).unwrap();
                spec[name] = json!(path.canonicalize().unwrap());
                spec[format!("{name}_sha256")] = json!(hash(name.as_bytes()));
            }
            policy["shell_realm"] = spec;
        }
        fs::write(
            root.path().join("relay-authority-v1/policy.json"),
            serde_json::to_vec(&policy).expect("policy bytes"),
        )
        .expect("policy");
        let host = Host::open(root.path()).expect("host");
        attach(&host, 'a', 'a', "register-a");
        (root, host)
    }
    fn proof(owner: char, token: &str) -> Value {
        json!({"key":owner.to_string().repeat(64),"epoch":1,"token":token})
    }
    fn create(host: &Host, connection: u64, owner: char, thread: &str) {
        attach(host, owner, owner, &format!("register-{owner}"));
        let key = format!("{connection}:1");
        assert!(matches!(
            host.admit(
                connection,
                &key,
                "thread/start",
                &json!({}),
                Some(&proof(owner, "create"))
            ),
            Ok(Admission::Execute)
        ));
        host.register(thread, None, Some(&key))
            .expect("exact native identity");
        host.complete(&key, "thread/start", json!({"thread":{"id":thread}}))
            .expect("commit");
    }
    #[test]
    fn foreign_unmediated_unknown_and_fenced_paths_refuse() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        create(&host, 2, 'b', "B");
        let params = json!({"threadId":"A"});
        for method in [
            "thread/resume",
            "thread/fork",
            "turn/start",
            "turn/steer",
            "thread/shellCommand",
            "thread/settings/update",
        ] {
            assert!(host.admit(3, "3:1", method, &params, None).is_err());
            assert!(
                host.admit(2, "2:2", method, &params, Some(&proof('b', method)))
                    .is_err()
            );
        }
        assert!(
            host.admit(
                1,
                "1:2",
                "process/spawn",
                &json!({}),
                Some(&proof('a', "process"))
            )
            .is_err()
        );
        assert!(matches!(
            host.admit(
                1,
                "1:3",
                "relay/authority",
                &json!({"command":"revoke","threadId":"A"}),
                Some(&proof('a', "revoke"))
            ),
            Ok(Admission::Reply(_))
        ));
        assert!(host.check("A").is_err());
        assert!(host.check("B").is_ok());
        assert!(
            host.admit(1, "1:4", "turn/start", &params, Some(&proof('a', "late")))
                .is_err()
        );
        assert!(
            host.admit(
                1,
                "1:5",
                "thread/resume",
                &params,
                Some(&proof('a', "resume"))
            )
            .is_err()
        );
        assert!(host.response_allowed(2, Some("A")).is_err());
        assert!(host.response_allowed(1, Some("A")).is_err());
        assert!(host.response_allowed(2, Some("B")).is_ok());
    }
    #[test]
    fn committed_replay_survives_host_restart_without_execution() {
        let (root, host) = fixture();
        create(&host, 1, 'a', "A");
        drop(host);
        let host = Host::open(root.path()).expect("restart");
        attach(&host, 'a', 'c', "recover-a");
        let reply = host
            .admit(
                10,
                "10:2",
                "thread/start",
                &json!({}),
                Some(&proof('c', "create")),
            )
            .expect("replay");
        assert!(matches!(reply,Admission::Reply(value) if value["thread"]["id"]=="A"));
        assert!(
            host.admit(
                10,
                "10:3",
                "thread/start",
                &json!({"model":"other"}),
                Some(&proof('c', "create"))
            )
            .is_err()
        );
        assert!(
            host.admit(
                10,
                "10:4",
                "thread/resume",
                &json!({"threadId":"A"}),
                Some(&proof('c', "resume"))
            )
            .is_ok()
        );
    }
    #[test]
    fn unresolved_creation_never_replays_and_second_host_cannot_start() {
        let (root, host) = fixture();
        assert!(Host::open(root.path()).is_err());
        host.admit(
            1,
            "1:1",
            "thread/start",
            &json!({}),
            Some(&proof('a', "lost")),
        )
        .expect("intent");
        drop(host);
        let host = Host::open(root.path()).expect("restart");
        attach(&host, 'a', 'c', "recover-a");
        assert!(
            host.admit(
                2,
                "2:1",
                "thread/start",
                &json!({}),
                Some(&proof('c', "lost"))
            )
            .is_err()
        );
    }
    #[test]
    fn queued_recheck_and_internal_descendants_follow_exact_root() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        create(&host, 2, 'b', "B");
        host.register("child", Some("A"), None)
            .expect("internal child");
        host.admit(
            1,
            "queued",
            "turn/start",
            &json!({"threadId":"A"}),
            Some(&proof('a', "queued")),
        )
        .expect("queue");
        host.admit(
            1,
            "revoke",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "revoke")),
        )
        .expect("fence");
        assert!(host.recheck("queued").is_err());
        assert!(host.check("child").is_err());
        assert!(host.register("late-child", Some("A"), None).is_err());
        assert!(host.register("B-child", Some("B"), None).is_ok());
        assert!(host.check("B-child").is_ok());
    }
    #[test]
    fn persistence_failure_poison_is_sticky() {
        let (root, host) = fixture();
        create(&host, 1, 'a', "A");
        fs::create_dir(root.path().join("relay-authority-v1/state.pending"))
            .expect("inject failed write");
        assert!(
            host.admit(
                1,
                "1:2",
                "turn/start",
                &json!({"threadId":"A"}),
                Some(&proof('a', "failed"))
            )
            .is_err()
        );
        assert!(host.check("A").is_err());
        assert!(
            host.admit(
                2,
                "2:1",
                "thread/start",
                &json!({}),
                Some(&proof('b', "new"))
            )
            .is_err()
        );
    }
    #[test]
    fn request_identity_cannot_be_reused_to_bypass_typed_admission() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        let params = json!({"threadId":"A"});
        host.admit(
            1,
            "queued",
            "turn/start",
            &params,
            Some(&proof('a', "queued")),
        )
        .expect("queue");
        assert!(
            host.prepared("queued", "turn/start", &params)
                .expect("exact intent")
        );
        assert!(
            host.prepared("queued", "thread/shellCommand", &params)
                .is_err()
        );
        assert!(
            host.prepared("queued", "turn/start", &json!({"threadId":"foreign"}))
                .is_err()
        );
        assert!(
            host.admit(
                1,
                "queued",
                "thread/start",
                &json!({}),
                Some(&proof('a', "collision"))
            )
            .is_err()
        );
    }

    #[test]
    fn ephemeral_intent_requires_parent_and_revokes_with_it() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        let params = json!({"ephemeral":true});
        assert!(
            host.admit(
                1,
                "unscoped",
                "thread/start",
                &params,
                Some(&proof('a', "unscoped"))
            )
            .is_err()
        );
        let mut owned = proof('a', "helper");
        owned["parentRoot"] = json!("A");
        host.admit(1, "helper", "thread/start", &params, Some(&owned))
            .expect("owned helper intent");
        host.register("helper-thread", None, Some("helper"))
            .expect("response-bound helper identity");
        assert!(host.check("helper-thread").is_ok());
        host.admit(
            1,
            "revoke",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "revoke")),
        )
        .expect("fence parent");
        assert!(host.recheck("helper").is_err());
        assert!(host.check("helper-thread").is_err());
    }

    #[test]
    fn fork_admitted_before_revoke_cannot_register_new_authority_after_it() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        host.admit(
            1,
            "fork",
            "thread/fork",
            &json!({"threadId":"A"}),
            Some(&proof('a', "fork")),
        )
        .expect("admitted fork");
        host.admit(
            1,
            "revoke",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "revoke")),
        )
        .expect("fence");
        assert!(host.register("late-fork", None, Some("fork")).is_err());
    }

    #[test]
    fn alternate_history_selection_cannot_override_authorized_thread() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        for method in ["thread/resume", "thread/fork"] {
            assert!(
                host.admit(
                    1,
                    method,
                    method,
                    &json!({"threadId":"A","path":"foreign-history"}),
                    Some(&proof('a', method))
                )
                .is_err()
            );
            assert!(
                host.admit(
                    1,
                    method,
                    method,
                    &json!({"threadId":"A","history":[]}),
                    Some(&proof('a', method))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn supervisor_rotation_fences_old_connection_and_pending_intent_only() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        create(&host, 2, 'b', "B");
        assert!(
            host.admit(
                3,
                "duplicate",
                "turn/start",
                &json!({"threadId":"A"}),
                Some(&proof('a', "duplicate"))
            )
            .is_err()
        );
        host.admit(
            1,
            "queued",
            "turn/start",
            &json!({"threadId":"A"}),
            Some(&proof('a', "queued")),
        )
        .expect("queue");
        attach(&host, 'a', 'c', "recover-a");
        assert!(host.recheck("queued").is_err());
        assert!(
            host.admit(
                1,
                "old",
                "turn/start",
                &json!({"threadId":"A"}),
                Some(&proof('a', "old"))
            )
            .is_err()
        );
        assert!(
            host.admit(
                3,
                "new",
                "turn/start",
                &json!({"threadId":"A"}),
                Some(&proof('c', "new"))
            )
            .is_ok()
        );
        assert!(
            host.admit(
                2,
                "neighbor",
                "turn/start",
                &json!({"threadId":"B"}),
                Some(&proof('b', "neighbor"))
            )
            .is_ok()
        );
        assert!(host.response_allowed(1, Some("A")).is_err());
        assert!(host.response_allowed(3, Some("A")).is_ok());
    }
    #[test]
    fn enrollment_and_owner_fence_prevent_fresh_conversation_escape() {
        let (_root, host) = fixture();
        create(&host, 1, 'a', "A");
        let unauthorized = json!({"key":"d".repeat(64),"recoveryKey":"b".repeat(64),"token":"invent-owner","epoch":1});
        assert!(
            host.admit(
                3,
                "invent",
                "relay/authority",
                &json!({"command":"register"}),
                Some(&unauthorized)
            )
            .is_err()
        );
        host.admit(
            1,
            "other",
            "thread/start",
            &json!({}),
            Some(&proof('a', "other")),
        )
        .expect("second owned root");
        host.register("A-other", None, Some("other"))
            .expect("native second binding");
        host.admit(
            1,
            "fence",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "fence")),
        )
        .expect("fence owner");
        assert!(host.check("A-other").is_err());
        assert!(
            host.admit(
                1,
                "escape",
                "thread/start",
                &json!({}),
                Some(&proof('a', "escape"))
            )
            .is_err()
        );
        attach(&host, 'a', 'c', "reconnect-fenced");
        assert!(
            host.admit(
                3,
                "escape2",
                "thread/start",
                &json!({}),
                Some(&proof('c', "escape2"))
            )
            .is_err()
        );
    }
    #[test]
    fn profile_identity_and_policy_changes_fail_closed() {
        let (root, host) = fixture();
        let other = tempfile::tempdir().expect("other physical home");
        assert!(host.check_home(other.path()).is_err());
        fs::write(root.path().join("relay-authority-v1/policy.json"), b"{}")
            .expect("inject policy change");
        assert!(
            host.admit(
                1,
                "changed",
                "thread/start",
                &json!({}),
                Some(&proof('a', "changed"))
            )
            .is_err()
        );
    }
    #[test]
    fn mismatched_native_response_never_commits_another_binding() {
        let (_root, host) = fixture();
        host.admit(
            1,
            "create",
            "thread/start",
            &json!({}),
            Some(&proof('a', "create")),
        )
        .expect("intent");
        host.register("exact", None, Some("create"))
            .expect("native binding");
        assert!(
            host.complete("create", "thread/read", json!({"thread":{"id":"exact"}}))
                .is_err()
        );
        assert!(
            host.complete("create", "thread/start", json!({"thread":{"id":"foreign"}}))
                .is_err()
        );
        let status = json!({"recoveryKey":"af".repeat(32),"epoch":1,"token":"status"});
        assert!(
            matches!(host.admit(9,"status","relay/authority",&json!({"command":"operationStatus","operationToken":"create"}),Some(&status)),Ok(Admission::Reply(value)) if value["state"]=="UNRESOLVED")
        );
    }
    #[test]
    fn shell_execution_grant_is_durable_exact_and_owner_fenced() {
        let (_root, host) = fixture_config(true);
        create(&host, 1, 'a', "A");
        create(&host, 2, 'b', "B");
        assert!(
            host.admit(
                1,
                "turn",
                "turn/start",
                &json!({"threadId":"A"}),
                Some(&proof('a', "unsupported-turn"))
            )
            .is_err()
        );
        assert!(
            host.admit(
                1,
                "config",
                "thread/start",
                &json!({"config":{"mcp_servers":{"uncontained":{}}}}),
                Some(&proof('a', "unsupported-config"))
            )
            .is_err()
        );
        let route = host.shell_route("A", "echo research").unwrap().unwrap();
        let directory = Path::new(&route.command[4]).parent().unwrap();
        let intent: Value =
            serde_json::from_slice(&fs::read(directory.join("intent.json")).unwrap()).unwrap();
        assert_eq!(intent["root"], "A");
        assert_eq!(intent["thread"], "A");
        assert_eq!(intent["realmID"], route.command[7]);
        assert_eq!(intent["hostGeneration"], host.generation);
        assert_eq!(
            fs::read_to_string(&route.command[6]).unwrap(),
            "echo research"
        );
        assert!(host.shell_route("foreign", "true").is_err());
        assert!(host.shell_route("A", &"x".repeat(1025)).is_err());
        host.admit(
            1,
            "revoke",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "revoke")),
        )
        .unwrap();
        assert!(Path::new(&route.command[5]).exists());
        assert!(host.shell_route("A", "true").is_err());
        assert!(host.shell_route("B", "true").unwrap().is_some());
        fs::write(&host.policy.shell_realm.as_ref().unwrap().kernel, "changed").unwrap();
        assert!(host.shell_route("B", "true").is_err());
    }

    #[test]
    fn legacy_shell_still_rechecks_core_ownership_before_launch() {
        let (_root, host) = fixture();
        assert!(host.shell_route("unowned", "true").is_err());
        create(&host, 1, 'a', "A");
        assert!(host.shell_route("A", "true").unwrap().is_none());
        host.admit(
            1,
            "revoke",
            "relay/authority",
            &json!({"command":"revoke","threadId":"A"}),
            Some(&proof('a', "revoke")),
        )
        .unwrap();
        assert!(host.shell_route("A", "true").is_err());
    }
    #[test]
    fn cancellation_and_stop_evidence_require_exact_native_execution_identity() {
        let (_root, host) = fixture_config(true);
        create(&host, 1, 'a', "A");
        create(&host, 2, 'b', "B");
        assert_eq!(
            host.cancellation_grace("A"),
            Some(std::time::Duration::from_secs(6))
        );
        assert_eq!(host.cancellation_grace("foreign"), None);
        let route = host.shell_route("A", "echo research").unwrap().unwrap();
        assert!(host.cancel_shell("B", &route.execution).is_err());
        assert!(host.cancel_shell("A", "../foreign").is_err());
        host.cancel_shell("A", &route.execution).unwrap();
        assert!(host.check("A").is_ok()); // turn cancellation does not revoke the owner
        assert!(!host.shell_stopped("A", &route.execution).unwrap());
        let path = Path::new(&route.command[4]);
        let mut state = json!({"realmID":route.execution,"label":"B","vmStopped":true,"vmRunning":false,"admissionClosed":true,"status":"STOPPED"});
        fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(!host.shell_stopped("A", &route.execution).unwrap());
        state["label"] = json!("A");
        fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(host.shell_stopped("A", &route.execution).unwrap());
        state["realmID"] = json!(uuid_generation());
        fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(!host.shell_stopped("A", &route.execution).unwrap());
    }
}
