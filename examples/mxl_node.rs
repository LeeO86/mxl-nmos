//! A small MXL node: one video sender, one audio receiver, a fake activation
//! handler, and registration with the registry from `NMOS_REGISTRY_ADDRESS` /
//! `NMOS_REGISTRY_PORT`.
//!
//! ```sh
//! NMOS_HOST_ADDRESS=192.0.2.10 NMOS_REGISTRY_ADDRESS=192.0.2.20 NMOS_REGISTRY_PORT=8010 \
//!     cargo run --example mxl_node
//! ```
//!
//! The node listens on `NMOS_PORT` (default 3312). It announces one MXL domain,
//! whose id is derived from the seed; it reads and writes no MXL data.

use std::time::Duration;

use mxl_nmos::{
    ids, media_types, ActiveConnection, Component, Device, Flow, Format, MxlDomain, Node,
    NodeImplementation, Rational, Receiver, ReceiverCaps, Role, Sender, Settings, Source,
    StagedConnection,
};
use serde_json::json;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut settings = Settings::new("mxl-nmos example", 3312);
    settings.description = "mxl-nmos example node".to_string();
    settings.service_name_prefix = "mxl-nmos-example".to_string();
    if let Err(err) = settings.apply_env() {
        eprintln!("invalid setting: {err}");
        std::process::exit(78);
    }
    let seed = settings
        .seed
        .clone()
        .unwrap_or_else(|| "mxl-nmos-example".to_string());
    settings.seed = Some(seed.clone());
    let domain = MxlDomain {
        id: ids::derive_id(&seed, "mxl-output-domain"),
        label: "example".to_string(),
        path: "/Volumes/mxl/example".into(),
    };
    let video_flow_id = ids::derive_id(&seed, "video-out/mxl-flow");

    let domains = vec![domain.clone()];
    let implementation = NodeImplementation::new(|activation| {
        Box::pin(async move {
            tracing::info!(
                "activation of {:?} {}: master_enable={} domain={:?} flow={:?} ({:?})",
                activation.role,
                activation.key,
                activation.master_enable,
                activation.mxl_domain_id,
                activation.mxl_flow_id,
                activation.cause
            );
            Ok(())
        })
    })
    .domains(move || domains.clone());

    let node = match Node::new(settings, implementation) {
        Ok(node) => node,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(78);
        }
    };
    build(&node, &domain, video_flow_id).expect("resources");

    let port = node.settings().port;
    let server = match node.bind(([0, 0, 0, 0], port).into()).await {
        Ok(server) => server,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(75);
        }
    };
    tracing::info!(
        "Node API http://{}:{port}/x-nmos/node/v1.3/, node id {}",
        node.host(),
        node.node_id()
    );
    node.start_registration();
    let serving = tokio::spawn(server.run());

    wait_for_signal().await;
    node.shutdown(Duration::from_secs(5)).await;
    let _ = serving.await;
}

fn build(node: &Node, domain: &MxlDomain, video_flow_id: uuid::Uuid) -> Result<(), mxl_nmos::Error> {
    let group = "Example";
    node.insert_device(Device::new("example", "Example").description("mxl-nmos example device"))?;

    let mut source = Source::video("video-out", "example");
    source.label = "Example Video".to_string();
    source.grain_rate = Some(Rational::new(50, 1));
    mxl_nmos::add_group_hint(&mut source.tags, group, "Video");
    node.insert_source(source)?;

    let mut flow = Flow::video(
        "video-out",
        "video-out",
        "example",
        media_types::VIDEO_V210,
        Rational::new(50, 1),
        1920,
        1080,
        Component::ycbcr_422(1920, 1080, 10),
    );
    flow.label = "Example Video".to_string();
    mxl_nmos::add_group_hint(&mut flow.tags, group, "Video");
    node.insert_flow(flow)?;

    let mut sender = Sender::mxl("video-out", "video-out", "example");
    sender.label = "Example Video".to_string();
    sender.mxl_domain_id = Some(domain.id);
    sender.mxl_flow_id = Some(video_flow_id);
    mxl_nmos::add_group_hint(&mut sender.tags, group, "Video");
    node.insert_sender(sender)?;
    node.set_staged(
        Role::Sender,
        "video-out",
        StagedConnection {
            master_enable: true,
            peer_id: None,
            mxl_domain_id: Some(domain.id).into(),
            mxl_flow_id: Some(video_flow_id).into(),
        },
    )?;
    node.set_active(
        Role::Sender,
        "video-out",
        ActiveConnection {
            master_enable: true,
            peer_id: None,
            mxl_domain_id: Some(domain.id),
            mxl_flow_id: Some(video_flow_id),
        },
    )?;

    let mut receiver = Receiver::mxl(
        "audio-in",
        "example",
        Format::Audio,
        vec![media_types::AUDIO_FLOAT32.to_string()],
    );
    receiver.label = "Example Audio".to_string();
    receiver.caps = ReceiverCaps {
        media_types: vec![media_types::AUDIO_FLOAT32.to_string()],
        constraint_sets: vec![json!({
            "urn:x-nmos:cap:format:channel_count": {"enum": [1, 2, 4, 8, 16]},
            "urn:x-nmos:cap:format:sample_rate": {"enum": [{"numerator": 48000, "denominator": 1}]},
            "urn:x-nmos:cap:format:sample_depth": {"enum": [32]}
        })],
        version: None,
    };
    mxl_nmos::add_group_hint(&mut receiver.tags, group, "Audio");
    node.insert_receiver(receiver)?;
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
