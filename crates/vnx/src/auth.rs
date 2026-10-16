//! VNX authentication, role permissions and cryptographic sessions.
//!
//! Native Proxy clients send lowercase SHA256(password); the stored credential is
//! SHA256(salt + that client hash + salt). `login()` intentionally returns a
//! default-role anonymous session for bad credentials, matching vnx-base.
use crate::{config::Config, error::invalid, runtime::Session, util, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};
use subtle::ConstantTimeEq;

pub const PASSWORD_SALT: &str = "xZJIdIv8ByEP6fCmzkyS7ca";
pub fn normalize_permission(permission: &str) -> String {
    if permission.contains('.') || permission == "*" {
        permission.into()
    } else {
        format!("vnx.permission_e.{permission}")
    }
}
fn role_name(role: &str) -> &str {
    role.strip_prefix("vnx.access_role_e.").unwrap_or(role)
}
pub fn password_hash(password: &str) -> String {
    util::to_hex_string(&util::sha256(password.as_bytes()), false, true)
}
pub fn final_hash(input_hash: &str) -> String {
    password_hash(&format!("{PASSWORD_SALT}{input_hash}{PASSWORD_SALT}"))
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub name: String,
    pub hashed_password: String,
    pub access_roles: Vec<String>,
    pub permissions: BTreeSet<String>,
}
impl fmt::Debug for User {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("User")
            .field("name", &self.name)
            .field("hashed_password", &"[redacted]")
            .field("access_roles", &self.access_roles)
            .field("permissions", &self.permissions)
            .finish()
    }
}
#[derive(Default)]
struct State {
    users: BTreeMap<String, User>,
    permissions: BTreeMap<String, BTreeSet<String>>,
    sessions: BTreeMap<u64, Session>,
}
/// All operations are thread-safe. Session values are copied out; logout revokes
/// lookups on the authority. Transports must check authority session validity if
/// they retain authenticated connections after logout.
#[derive(Clone)]
pub struct AuthenticationServer {
    state: Arc<RwLock<State>>,
}
pub type AuthServer = AuthenticationServer;
impl Default for AuthenticationServer {
    fn default() -> Self {
        Self::new()
    }
}
impl AuthenticationServer {
    pub fn new() -> Self {
        let mut state = State::default();
        for (role, permissions) in [
            ("UNAUTHENTICATED", &["PROXY_EXPORT", "PROXY_INTERNAL"][..]),
            ("DEFAULT", &[][..]),
            ("VIEWER", &["VIEW", "TIME_SYNC"][..]),
            ("OBSERVER", &["VIEW", "TIME_SYNC", "CONST_REQUEST"][..]),
            (
                "USER",
                &["VIEW", "TIME_SYNC", "CONST_REQUEST", "READ_CONFIG"][..],
            ),
            (
                "INSTALLER",
                &[
                    "VIEW",
                    "TIME_SYNC",
                    "CONST_REQUEST",
                    "PUBLISH",
                    "READ_CONFIG",
                    "WRITE_CONFIG",
                    "START",
                    "STOP",
                    "RESTART",
                    "SHUTDOWN",
                    "SELF_TEST",
                ][..],
            ),
            (
                "ADMIN",
                &[
                    "VIEW",
                    "TIME_SYNC",
                    "CONST_REQUEST",
                    "PUBLISH",
                    "REQUEST",
                    "READ_CONFIG",
                    "WRITE_CONFIG",
                    "PROTECTED_CONFIG",
                    "PROXY_IMPORT",
                    "PROXY_EXPORT",
                    "PROXY_FORWARD",
                    "START",
                    "STOP",
                    "RESTART",
                    "SHUTDOWN",
                    "SELF_TEST",
                    "HOST_SHUTDOWN",
                    "LOCAL",
                ][..],
            ),
        ] {
            state.permissions.insert(
                role.into(),
                permissions
                    .iter()
                    .map(|permission| normalize_permission(permission))
                    .collect(),
            );
        }
        Self {
            state: Arc::new(RwLock::new(state)),
        }
    }
    pub fn from_config(config: &Config) -> Result<Self> {
        let auth = Self::new();
        if let Some(users) = config.read::<Vec<User>>("vnx.authentication.users")? {
            for user in users {
                auth.add_user(user)?;
            }
        }
        if let Some(passwords) =
            config.read::<BTreeMap<String, String>>("vnx.authentication.passwd")?
        {
            for (name, hash) in passwords {
                let mut user = auth.get_user(&name)?.unwrap_or_else(|| User {
                    name: name.clone(),
                    ..Default::default()
                });
                user.hashed_password = hash;
                auth.add_user(user)?;
            }
        }
        if let Some(permissions) =
            config.read::<BTreeMap<String, Vec<String>>>("vnx.authentication.permissions")?
        {
            for (role, permissions) in permissions {
                auth.update_permissions(&role, permissions)?;
            }
        }
        Ok(auth)
    }
    pub fn add_user(&self, mut user: User) -> Result<()> {
        if user.name.is_empty() {
            return Err(invalid("empty authentication user name"));
        }
        // Empty passwords in shipped role declarations mean login is disabled
        // until a credential is configured; never accept an empty stored digest.
        user.permissions = user
            .permissions
            .iter()
            .map(|permission| normalize_permission(permission))
            .collect();
        self.state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .users
            .insert(user.name.clone(), user);
        Ok(())
    }
    pub fn get_user(&self, name: &str) -> Result<Option<User>> {
        Ok(self
            .state
            .read()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .users
            .get(name)
            .cloned())
    }
    /// Extend a role, with `!PERMISSION` removing an inherited builtin grant.
    pub fn update_permissions(
        &self,
        role: &str,
        permissions: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?;
        let grants = state.permissions.entry(role_name(role).into()).or_default();
        for permission in permissions {
            if let Some(negated) = permission.strip_prefix('!') {
                grants.remove(&normalize_permission(negated));
            } else {
                grants.insert(normalize_permission(&permission));
            }
        }
        Ok(())
    }
    pub fn get_permissions(&self, role: &str) -> Result<BTreeSet<String>> {
        Ok(self
            .state
            .read()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .permissions
            .get(role_name(role))
            .cloned()
            .unwrap_or_default())
    }
    /// Input is the hash sent by a Proxy, not a cleartext password. Failed
    /// credentials create an anonymous session with only `default_access` grants.
    pub fn login(&self, name: &str, input_hash: &str, default_access: &str) -> Result<Session> {
        let digest = util::sha256(format!("{PASSWORD_SALT}{input_hash}{PASSWORD_SALT}").as_bytes());
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?;
        let mut session = Session {
            id: 0,
            user: String::new(),
            permissions: state
                .permissions
                .get(role_name(default_access))
                .cloned()
                .unwrap_or_default(),
            login_time: util::get_time_micros(),
        };
        if let Some(user) = state.users.get(name) {
            // Fixed-size byte comparison uses subtle's compiler barriers rather
            // than a string equality that may stop at the first different byte.
            let stored = util::from_hex_string(&user.hashed_password)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .unwrap_or([0; 32]);
            let authenticated =
                bool::from(digest.ct_eq(&stored)) && user.hashed_password.len() == 64;
            if authenticated {
                session.user = user.name.clone();
                for role in &user.access_roles {
                    if let Some(grants) = state.permissions.get(role_name(role)) {
                        session.permissions.extend(grants.iter().cloned());
                    }
                }
                session.permissions.extend(user.permissions.iter().cloned());
            }
        } else {
            // Perform the same fixed-width digest comparison on unknown users.
            let _ = digest.ct_eq(&[0; 32]);
        }
        Self::insert_session(&mut state, session)
    }
    /// Adapter for transports that use errors for failed login. Such a transport
    /// must reset its previous session before attempting authentication.
    pub fn authenticate_hash(&self, name: &str, input_hash: &str) -> Result<Session> {
        let session = self.login(name, input_hash, "DEFAULT")?;
        if session.is_anonymous() {
            self.logout(session.id)?;
            Err(Error::PermissionDenied("invalid credentials".into()))
        } else {
            Ok(session)
        }
    }
    pub fn authenticate_password(&self, name: &str, password: &str) -> Result<Session> {
        self.authenticate_hash(name, &password_hash(password))
    }
    pub fn login_anonymous(&self, role: &str) -> Result<Session> {
        self.login_anonymous_roles([role])
    }
    pub fn login_anonymous_roles<'a>(
        &self,
        roles: impl IntoIterator<Item = &'a str>,
    ) -> Result<Session> {
        let mut state = self
            .state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?;
        let mut session = Session {
            id: 0,
            user: String::new(),
            permissions: BTreeSet::new(),
            login_time: util::get_time_micros(),
        };
        for role in roles {
            if let Some(grants) = state.permissions.get(role_name(role)) {
                session.permissions.extend(grants.iter().cloned());
            }
        }
        Self::insert_session(&mut state, session)
    }
    fn insert_session(state: &mut State, mut session: Session) -> Result<Session> {
        loop {
            let id = util::random_nonzero_u64()?;
            if !state.sessions.contains_key(&id) {
                session.id = id;
                break;
            }
        }
        state.sessions.insert(session.id, session.clone());
        Ok(session)
    }
    pub fn logout(&self, id: u64) -> Result<()> {
        self.state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .sessions
            .remove(&id);
        Ok(())
    }
    pub fn get_session(&self, id: u64) -> Result<Option<Session>> {
        Ok(self
            .state
            .read()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .sessions
            .get(&id)
            .cloned())
    }
    /// Session zero is VNX's trusted in-process identity. Do not use it for an
    /// unauthenticated network connection; create a nonzero anonymous session.
    pub fn is_allowed(&self, id: u64, permission: &str) -> Result<bool> {
        if id == 0 {
            return Ok(true);
        }
        Ok(self
            .get_session(id)?
            .is_some_and(|session| session.has_permission(&normalize_permission(permission))))
    }
    pub fn session_count(&self) -> Result<usize> {
        Ok(self
            .state
            .read()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .sessions
            .len())
    }
    pub fn revoke_all(&self) -> Result<()> {
        self.state
            .write()
            .map_err(|_| invalid("authentication lock poisoned"))?
            .sessions
            .clear();
        Ok(())
    }
}
