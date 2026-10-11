//! The resources an application inserts into its [`crate::Node`].
//!
//! One type per IS-04 resource, as the nmos-cpp nodes of the sibling media
//! functions build them (`make_device`, `make_*_source`, `make_*_flow`,
//! `make_sender`, `make_receiver`). Every resource has a stable application
//! `key`; its NMOS id is derived from the node id and that key. References
//! between resources use keys too.

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

/// Tag name to values, as in IS-04 `tags`.
pub type Tags = BTreeMap<String, Vec<String>>;

/// BCP-002-01 group hint tag name.
pub const GROUP_HINT_TAG: &str = "urn:x-nmos:tag:grouphint/v1.0";

/// The BCP-002-01 group hint value `"{group}:{role}"`, as `nmos::make_group_hint`.
pub fn group_hint(group: &str, role: &str) -> String {
    format!("{group}:{role}")
}

/// Add a BCP-002-01 group hint to `tags`.
pub fn add_group_hint(tags: &mut Tags, group: &str, role: &str) {
    tags.entry(GROUP_HINT_TAG.to_string())
        .or_default()
        .push(group_hint(group, role));
}

/// A rational number (`grain_rate`, `sample_rate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    /// Numerator.
    pub numerator: u32,
    /// Denominator.
    pub denominator: u32,
}

impl Rational {
    /// `numerator / denominator`.
    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }
}

/// IS-04 `format` of a Source, Flow or Receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `urn:x-nmos:format:video`
    Video,
    /// `urn:x-nmos:format:audio`
    Audio,
    /// `urn:x-nmos:format:data`
    Data,
}

impl Format {
    /// The format URN.
    pub fn urn(self) -> &'static str {
        match self {
            Self::Video => "urn:x-nmos:format:video",
            Self::Audio => "urn:x-nmos:format:audio",
            Self::Data => "urn:x-nmos:format:data",
        }
    }
}

/// Media types used on the platform.
pub mod media_types {
    /// 10-bit 4:2:2 video.
    pub const VIDEO_V210: &str = "video/v210";
    /// v210 with alpha.
    pub const VIDEO_V210A: &str = "video/v210a";
    /// 32-bit float audio.
    pub const AUDIO_FLOAT32: &str = "audio/float32";
    /// SMPTE ST 291 ancillary data.
    pub const VIDEO_SMPTE291: &str = "video/smpte291";
}

/// A Device.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// Stable application key.
    pub key: String,
    /// Device name. The announced label is `"{NMOS_LABEL} {name}"`, or the name
    /// alone when the node label is empty.
    pub name: String,
    /// Description.
    pub description: String,
    /// Tags; the node's `NMOS_TAGS` are added to them.
    pub tags: Tags,
    /// Device type URN.
    pub device_type: String,
}

impl Device {
    /// A generic Device.
    pub fn new(key: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            description: String::new(),
            tags: Tags::new(),
            device_type: "urn:x-nmos:device:generic".to_string(),
        }
    }

    /// Set the description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

/// An audio channel of an audio Source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// Channel label.
    pub label: String,
    /// Optional VSF TR-03 channel symbol, e.g. `L`, `R`.
    pub symbol: Option<String>,
}

impl Channel {
    /// A channel with a label and no symbol.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            symbol: None,
        }
    }
}

/// A Source.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// Stable application key.
    pub key: String,
    /// Key of the Device.
    pub device_key: String,
    /// Label.
    pub label: String,
    /// Description.
    pub description: String,
    /// Tags.
    pub tags: Tags,
    /// Format.
    pub format: Format,
    /// Audio channels (audio Sources only).
    pub channels: Vec<Channel>,
    /// Grain rate, for periodic Sources.
    pub grain_rate: Option<Rational>,
    /// Keys of parent Sources.
    pub parents: Vec<String>,
}

impl Source {
    fn new(key: String, device_key: String, format: Format) -> Self {
        Self {
            key,
            device_key,
            label: String::new(),
            description: String::new(),
            tags: Tags::new(),
            format,
            channels: Vec::new(),
            grain_rate: None,
            parents: Vec::new(),
        }
    }

    /// A video Source.
    pub fn video(key: impl Into<String>, device_key: impl Into<String>) -> Self {
        Self::new(key.into(), device_key.into(), Format::Video)
    }

    /// An audio Source with `channels`.
    pub fn audio(
        key: impl Into<String>,
        device_key: impl Into<String>,
        channels: Vec<Channel>,
    ) -> Self {
        Self {
            channels,
            ..Self::new(key.into(), device_key.into(), Format::Audio)
        }
    }

    /// A data Source.
    pub fn data(key: impl Into<String>, device_key: impl Into<String>) -> Self {
        Self::new(key.into(), device_key.into(), Format::Data)
    }
}

/// One component of a video Flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// `Y`, `Cb`, `Cr`, `A`, …
    pub name: String,
    /// Width in samples.
    pub width: u32,
    /// Height in samples.
    pub height: u32,
    /// Bits per sample.
    pub bit_depth: u32,
}

impl Component {
    /// A component.
    pub fn new(name: impl Into<String>, width: u32, height: u32, bit_depth: u32) -> Self {
        Self {
            name: name.into(),
            width,
            height,
            bit_depth,
        }
    }

    /// Y, Cb, Cr of 4:2:2 video at `bit_depth`.
    pub fn ycbcr_422(width: u32, height: u32, bit_depth: u32) -> Vec<Self> {
        vec![
            Self::new("Y", width, height, bit_depth),
            Self::new("Cb", width / 2, height, bit_depth),
            Self::new("Cr", width / 2, height, bit_depth),
        ]
    }
}

/// Format-specific attributes of a Flow.
#[derive(Debug, Clone, PartialEq)]
pub enum FlowFormat {
    /// A (coded) video Flow: `video/v210`, `video/v210a`.
    Video {
        /// Frame width.
        frame_width: u32,
        /// Frame height.
        frame_height: u32,
        /// `progressive`, `interlaced_tff`, `interlaced_bff`, `interlaced_psf`.
        interlace_mode: String,
        /// E.g. `BT709`.
        colorspace: String,
        /// E.g. `SDR`; omitted when `None`.
        transfer_characteristic: Option<String>,
        /// Components.
        components: Vec<Component>,
    },
    /// An audio Flow: `audio/float32`.
    Audio {
        /// Sample rate.
        sample_rate: Rational,
        /// Bits per sample; omitted when `None`.
        bit_depth: Option<u32>,
    },
    /// A data Flow: `video/smpte291`.
    Data,
}

/// A Flow.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    /// Stable application key.
    pub key: String,
    /// Key of the Source.
    pub source_key: String,
    /// Key of the Device.
    pub device_key: String,
    /// Label.
    pub label: String,
    /// Description.
    pub description: String,
    /// Tags.
    pub tags: Tags,
    /// Media type.
    pub media_type: String,
    /// Grain rate.
    pub grain_rate: Option<Rational>,
    /// Format-specific attributes.
    pub format: FlowFormat,
    /// Keys of parent Flows.
    pub parents: Vec<String>,
}

impl Flow {
    fn new(
        key: String,
        source_key: String,
        device_key: String,
        media_type: String,
        format: FlowFormat,
    ) -> Self {
        Self {
            key,
            source_key,
            device_key,
            label: String::new(),
            description: String::new(),
            tags: Tags::new(),
            media_type,
            grain_rate: None,
            format,
            parents: Vec::new(),
        }
    }

    /// A video Flow (`nmos::make_coded_video_flow`).
    #[allow(clippy::too_many_arguments)]
    pub fn video(
        key: impl Into<String>,
        source_key: impl Into<String>,
        device_key: impl Into<String>,
        media_type: impl Into<String>,
        grain_rate: Rational,
        frame_width: u32,
        frame_height: u32,
        components: Vec<Component>,
    ) -> Self {
        let mut flow = Self::new(
            key.into(),
            source_key.into(),
            device_key.into(),
            media_type.into(),
            FlowFormat::Video {
                frame_width,
                frame_height,
                interlace_mode: "progressive".to_string(),
                colorspace: "BT709".to_string(),
                transfer_characteristic: None,
                components,
            },
        );
        flow.grain_rate = Some(grain_rate);
        flow
    }

    /// An audio Flow.
    pub fn audio(
        key: impl Into<String>,
        source_key: impl Into<String>,
        device_key: impl Into<String>,
        media_type: impl Into<String>,
        sample_rate: Rational,
        bit_depth: Option<u32>,
    ) -> Self {
        Self::new(
            key.into(),
            source_key.into(),
            device_key.into(),
            media_type.into(),
            FlowFormat::Audio {
                sample_rate,
                bit_depth,
            },
        )
    }

    /// A data Flow.
    pub fn data(
        key: impl Into<String>,
        source_key: impl Into<String>,
        device_key: impl Into<String>,
        media_type: impl Into<String>,
    ) -> Self {
        Self::new(
            key.into(),
            source_key.into(),
            device_key.into(),
            media_type.into(),
            FlowFormat::Data,
        )
    }

    /// The Flow's format.
    pub fn nmos_format(&self) -> Format {
        match self.format {
            FlowFormat::Video { .. } => Format::Video,
            FlowFormat::Audio { .. } => Format::Audio,
            FlowFormat::Data => Format::Data,
        }
    }
}

/// An MXL Sender (`urn:x-nmos:transport:mxl`).
#[derive(Debug, Clone, PartialEq)]
pub struct Sender {
    /// Stable application key.
    pub key: String,
    /// Key of the Flow, if any.
    pub flow_key: Option<String>,
    /// Key of the Device.
    pub device_key: String,
    /// Label.
    pub label: String,
    /// Description.
    pub description: String,
    /// Tags.
    pub tags: Tags,
    /// The MXL flow id `auto` resolves to. It may differ from the IS-04 Flow id
    /// (BCP-007-03). `None`: a random id is chosen once.
    pub mxl_flow_id: Option<Uuid>,
    /// The MXL domain id `auto` resolves to. `None`: the only visible domain.
    pub mxl_domain_id: Option<Uuid>,
}

impl Sender {
    /// An MXL Sender of `flow_key`.
    pub fn mxl(
        key: impl Into<String>,
        flow_key: impl Into<String>,
        device_key: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            flow_key: Some(flow_key.into()),
            device_key: device_key.into(),
            label: String::new(),
            description: String::new(),
            tags: Tags::new(),
            mxl_flow_id: None,
            mxl_domain_id: None,
        }
    }
}

/// BCP-004-01 receiver capabilities.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReceiverCaps {
    /// Accepted media types.
    pub media_types: Vec<String>,
    /// Constraint sets (objects of `urn:x-nmos:cap:…` constraints).
    pub constraint_sets: Vec<Value>,
    /// Caps `version` (TAI). `None`: the crate stamps the time the caps last
    /// changed whenever constraint sets are present, as BCP-004-01 requires.
    pub version: Option<String>,
}

/// An MXL Receiver (`urn:x-nmos:transport:mxl`).
#[derive(Debug, Clone, PartialEq)]
pub struct Receiver {
    /// Stable application key.
    pub key: String,
    /// Key of the Device.
    pub device_key: String,
    /// Label.
    pub label: String,
    /// Description.
    pub description: String,
    /// Tags.
    pub tags: Tags,
    /// Format.
    pub format: Format,
    /// Capabilities.
    pub caps: ReceiverCaps,
}

impl Receiver {
    /// An MXL Receiver accepting `media_types`.
    pub fn mxl(
        key: impl Into<String>,
        device_key: impl Into<String>,
        format: Format,
        media_types: Vec<String>,
    ) -> Self {
        Self {
            key: key.into(),
            device_key: device_key.into(),
            label: String::new(),
            description: String::new(),
            tags: Tags::new(),
            format,
            caps: ReceiverCaps {
                media_types,
                ..ReceiverCaps::default()
            },
        }
    }
}

/// The kind of a resource, for [`crate::Node::erase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceType {
    /// Node.
    Node,
    /// Device.
    Device,
    /// Source.
    Source,
    /// Flow.
    Flow,
    /// Sender.
    Sender,
    /// Receiver.
    Receiver,
}

impl ResourceType {
    /// The IS-04 type name (`node`, `device`, …).
    pub fn name(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Device => "device",
            Self::Source => "source",
            Self::Flow => "flow",
            Self::Sender => "sender",
            Self::Receiver => "receiver",
        }
    }

    /// Registration order: Node, Device, Source, Flow, then Senders and Receivers.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::Node => 0,
            Self::Device => 1,
            Self::Source => 2,
            Self::Flow => 3,
            Self::Sender | Self::Receiver => 4,
        }
    }
}
