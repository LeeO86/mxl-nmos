//! An AMWA NMOS Node for MXL media functions: IS-04 v1.3 Node API, IS-05 v1.2
//! Connection API and the BCP-007-03 MXL transport, with registration to a static
//! registry (and DNS-SD behind the `dns-sd` feature).
//!
//! The crate knows nothing about media: MXL domain and flow ids are UUIDs, and the
//! application reads and writes MXL itself. It is shaped like the nmos-cpp nodes of
//! the sibling media functions: the application inserts one resource per call
//! ([`Node::insert_device`], [`Node::insert_source`], [`Node::insert_flow`],
//! [`Node::insert_sender`], [`Node::insert_receiver`]) and sets its callbacks on a
//! [`NodeImplementation`].
//!
//! ```no_run
//! use mxl_nmos::{
//!     media_types, Channel, Device, Flow, Format, Node, NodeImplementation, Rational,
//!     Receiver, Sender, Settings, Source,
//! };
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut settings = Settings::new("Mixer", 3312);
//! settings.apply_env()?; // NMOS_SEED, NMOS_REGISTRY_ADDRESS, …; invalid → exit 78
//! let implementation = NodeImplementation::new(|activation| {
//!     Box::pin(async move {
//!         println!("connect {} to {:?}", activation.key, activation.mxl_flow_id);
//!         Ok(())
//!     })
//! });
//! let node = Node::new(settings, implementation)?;
//! node.insert_device(Device::new("mixer", "Audio Mixer"))?;
//! node.insert_receiver(Receiver::mxl(
//!     "in-1",
//!     "mixer",
//!     Format::Audio,
//!     vec![media_types::AUDIO_FLOAT32.to_string()],
//! ))?;
//! node.insert_source(Source::audio("pgm", "mixer", vec![Channel::new("L"), Channel::new("R")]))?;
//! node.insert_flow(Flow::audio(
//!     "pgm",
//!     "pgm",
//!     "mixer",
//!     media_types::AUDIO_FLOAT32,
//!     Rational::new(48_000, 1),
//!     Some(32),
//! ))?;
//! node.insert_sender(Sender::mxl("pgm", "pgm", "mixer"))?;
//! let server = node.bind(([0, 0, 0, 0], 3312).into()).await?; // bind error → exit 75
//! node.start_registration();
//! tokio::spawn(server.run());
//! // … on SIGTERM:
//! node.shutdown(std::time::Duration::from_secs(5)).await;
//! # Ok(())
//! # }
//! ```

mod activation;
mod address;
pub mod clock;
#[cfg(feature = "dns-sd")]
mod dns_sd;
mod http;
pub mod ids;
mod is05;
mod model;
mod node;
mod registration;
mod resources;
mod settings;

use std::fmt;
use std::net::SocketAddr;

pub use activation::{
    Activation, ActivationHandler, ActiveConnection, AutoRequest, Cause, ConnectionsChanged,
    DomainProvider, MxlDomain, NodeImplementation, Param, Rejected, ResolveAuto, Role,
    SavedConnection, StagedConnection,
};
pub use address::first_routable_ipv4;
pub use http::Server;
pub use node::Node;
pub use resources::{
    add_group_hint, group_hint, media_types, Channel, Component, Device, Flow, FlowFormat, Format,
    Rational, Receiver, ReceiverCaps, ResourceType, Sender, Source, Tags, GROUP_HINT_TAG,
};
pub use settings::{parse_tags, query_base_url, require_announce_ipv4, Settings, SettingsError};

/// Errors of the node API.
#[derive(Debug)]
pub enum Error {
    /// The announce address is missing or not routable.
    InvalidAddress(String),
    /// A referenced or named resource does not exist.
    UnknownResource {
        /// Its type.
        typ: ResourceType,
        /// Its key.
        key: String,
    },
    /// The listener could not be bound.
    Bind {
        /// The address.
        addr: SocketAddr,
        /// The cause.
        source: std::io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAddress(text) => f.write_str(text),
            Self::UnknownResource { typ, key } => write!(f, "no {} with key '{key}'", typ.name()),
            Self::Bind { addr, source } => write!(f, "cannot bind {addr}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind { source, .. } => Some(source),
            _ => None,
        }
    }
}
