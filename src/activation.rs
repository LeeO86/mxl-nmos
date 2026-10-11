//! The application side of IS-05: activation callback, `auto` resolution, the
//! list of visible MXL domains, and connection state.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::clock::{SharedClock, SystemClock};

/// Sender or Receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// A Sender (MXL writer).
    Sender,
    /// A Receiver (MXL reader).
    Receiver,
}

/// Why the callback is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// `activate_immediate` on `/single/.../staged`.
    Immediate,
    /// A scheduled activation reached its time.
    Scheduled,
    /// An immediate activation inside a `/bulk` request.
    Bulk,
    /// [`crate::Node::restore`] at start-up.
    Restore,
}

/// The resolved parameters of one activation, for one Sender or Receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// Sender or Receiver.
    pub role: Role,
    /// The application's key of the resource.
    pub key: String,
    /// The NMOS id of the resource.
    pub nmos_id: Uuid,
    /// `master_enable`. `false` is a normal activation: stop reading or writing.
    pub master_enable: bool,
    /// Resolved `mxl_domain_id` (never `auto`).
    pub mxl_domain_id: Option<Uuid>,
    /// Directory of that domain, from the application's domain list.
    pub domain_path: Option<PathBuf>,
    /// Resolved `mxl_flow_id` (never `auto`).
    pub mxl_flow_id: Option<Uuid>,
    /// `sender_id` of a Receiver, `receiver_id` of a Sender.
    pub peer_id: Option<Uuid>,
    /// Why the callback is called.
    pub cause: Cause,
}

/// The application refused an activation; IS-05 answers 500 with this reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected(pub String);

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

/// Called for every activation of one resource.
///
/// `Ok` means the application took the parameters; it does not mean media flows.
/// A Receiver whose flow does not exist yet is accepted, and the application waits
/// for the flow. The future must not wait for media: it is cancelled after
/// [`crate::Settings::activation_timeout`] and the activation then fails.
pub type ActivationHandler =
    Arc<dyn Fn(Activation) -> BoxFuture<'static, Result<(), Rejected>> + Send + Sync>;

/// An MXL domain visible to this node (from its `domain_def.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MxlDomain {
    /// The domain id.
    pub id: Uuid,
    /// Label.
    pub label: String,
    /// The domain directory.
    pub path: PathBuf,
}

/// Returns the MXL domains this node can read and write. Called on every request
/// and registration tick, so it should be cheap.
pub type DomainProvider = Arc<dyn Fn() -> Vec<MxlDomain> + Send + Sync>;

/// A transport parameter as staged: `null`, `"auto"` or a UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Param {
    /// `null`: not configured.
    #[default]
    Null,
    /// `"auto"`.
    Auto,
    /// A UUID.
    Id(Uuid),
}

impl Param {
    /// `Some(id)` for [`Param::Id`].
    pub fn id(self) -> Option<Uuid> {
        match self {
            Self::Id(id) => Some(id),
            _ => None,
        }
    }
}

impl From<Option<Uuid>> for Param {
    fn from(id: Option<Uuid>) -> Self {
        id.map_or(Self::Null, Self::Id)
    }
}

/// What `auto` should resolve to.
#[derive(Debug, Clone)]
pub struct AutoRequest {
    /// Sender or Receiver.
    pub role: Role,
    /// The application's key.
    pub key: String,
    /// Staged `mxl_domain_id`.
    pub mxl_domain_id: Param,
    /// Staged `mxl_flow_id`.
    pub mxl_flow_id: Param,
    /// Visible domains.
    pub domains: Vec<MxlDomain>,
}

/// Resolves `auto` in a staged activation. `Ok((domain, flow))`; an `Err` fails
/// the activation with 500.
pub type ResolveAuto =
    Arc<dyn Fn(&AutoRequest) -> Result<(Option<Uuid>, Option<Uuid>), String> + Send + Sync>;

/// Connection state of a Sender or Receiver, as the application sets it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StagedConnection {
    /// `master_enable`.
    pub master_enable: bool,
    /// `sender_id` / `receiver_id`.
    pub peer_id: Option<Uuid>,
    /// `mxl_domain_id`.
    pub mxl_domain_id: Param,
    /// `mxl_flow_id`.
    pub mxl_flow_id: Param,
}

/// Active connection state; `auto` is always resolved.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActiveConnection {
    /// `master_enable`.
    pub master_enable: bool,
    /// `sender_id` / `receiver_id`.
    pub peer_id: Option<Uuid>,
    /// `mxl_domain_id`.
    pub mxl_domain_id: Option<Uuid>,
    /// `mxl_flow_id`.
    pub mxl_flow_id: Option<Uuid>,
}

/// The active state of one Sender or Receiver, for persistence (G9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    /// Sender or Receiver.
    pub role: Role,
    /// The application's key.
    pub key: String,
    /// The active state.
    #[serde(flatten)]
    pub active: ActiveConnection,
}

/// Called with every Sender's and Receiver's active state after an activation
/// changed one of them, so the application can save it.
pub type ConnectionsChanged = Arc<dyn Fn(Vec<SavedConnection>) + Send + Sync>;

/// The callbacks of a node, as `nmos::experimental::node_implementation`.
#[derive(Clone)]
pub struct NodeImplementation {
    pub(crate) on_activation: ActivationHandler,
    pub(crate) on_resolve_auto: Option<ResolveAuto>,
    pub(crate) domains: Option<DomainProvider>,
    pub(crate) on_connections_changed: Option<ConnectionsChanged>,
    pub(crate) clock: SharedClock,
}

impl NodeImplementation {
    /// Callbacks with the required activation handler.
    pub fn new<F>(on_activation: F) -> Self
    where
        F: Fn(Activation) -> BoxFuture<'static, Result<(), Rejected>> + Send + Sync + 'static,
    {
        Self {
            on_activation: Arc::new(on_activation),
            on_resolve_auto: None,
            domains: None,
            on_connections_changed: None,
            clock: Arc::new(SystemClock),
        }
    }

    /// Replace the default `auto` resolution.
    pub fn on_resolve_auto<F>(mut self, f: F) -> Self
    where
        F: Fn(&AutoRequest) -> Result<(Option<Uuid>, Option<Uuid>), String> + Send + Sync + 'static,
    {
        self.on_resolve_auto = Some(Arc::new(f));
        self
    }

    /// The MXL domains this node can use.
    pub fn domains<F>(mut self, f: F) -> Self
    where
        F: Fn() -> Vec<MxlDomain> + Send + Sync + 'static,
    {
        self.domains = Some(Arc::new(f));
        self
    }

    /// Persistence hook, see [`ConnectionsChanged`].
    pub fn on_connections_changed<F>(mut self, f: F) -> Self
    where
        F: Fn(Vec<SavedConnection>) + Send + Sync + 'static,
    {
        self.on_connections_changed = Some(Arc::new(f));
        self
    }

    /// Use another TAI clock (tests).
    pub fn clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }
}

impl fmt::Debug for NodeImplementation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeImplementation")
            .field("on_resolve_auto", &self.on_resolve_auto.is_some())
            .field("domains", &self.domains.is_some())
            .field(
                "on_connections_changed",
                &self.on_connections_changed.is_some(),
            )
            .finish()
    }
}
