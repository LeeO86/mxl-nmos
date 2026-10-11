//! Parsing and checking of IS-05 `/staged` PATCH bodies (IS-05 v1.2 stage
//! schemas with the BCP-007-03 MXL transport parameters).

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::activation::{MxlDomain, Param, Role, StagedConnection};
use crate::clock::TaiTime;

/// The `activation` of a PATCH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivationRequest {
    /// `mode: null`: no activation; cancels a pending one.
    Cancel,
    Immediate,
    Absolute(TaiTime),
    Relative(TaiTime),
}

/// A parsed PATCH body. `None` fields were absent.
#[derive(Debug, Clone, Default)]
pub(crate) struct StagePatch {
    pub peer_id: Option<Option<Uuid>>,
    pub master_enable: Option<bool>,
    pub activation: Option<ActivationRequest>,
    pub mxl_domain_id: Option<Param>,
    pub mxl_flow_id: Option<Param>,
}

impl StagePatch {
    /// Apply the patch to `staged`, checking ids against the visible domains.
    pub(crate) fn apply(
        &self,
        role: Role,
        staged: &mut StagedConnection,
        domains: &[MxlDomain],
    ) -> Result<(), String> {
        if let Some(domain) = self.mxl_domain_id {
            if let Param::Id(id) = domain {
                if domains.is_empty() {
                    return Err("no MXL domains with domain_def.json are visible".to_string());
                }
                if !domains.iter().any(|d| d.id == id) {
                    return Err(format!(
                        "mxl_domain_id {id} is not a domain this node can access"
                    ));
                }
            }
            staged.mxl_domain_id = domain;
        }
        if let Some(flow) = self.mxl_flow_id {
            if role == Role::Receiver && flow == Param::Auto {
                return Err("receivers must not accept auto for mxl_flow_id".to_string());
            }
            staged.mxl_flow_id = flow;
        }
        if let Some(peer) = self.peer_id {
            staged.peer_id = peer;
        }
        if let Some(enable) = self.master_enable {
            staged.master_enable = enable;
        }
        Ok(())
    }
}

/// Parse a PATCH body for a Sender or Receiver.
pub(crate) fn parse_patch(role: Role, body: &Value) -> Result<StagePatch, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the request body must be a JSON object".to_string())?;
    let peer_field = match role {
        Role::Sender => "receiver_id",
        Role::Receiver => "sender_id",
    };
    let mut patch = StagePatch::default();
    for (key, value) in object {
        match key.as_str() {
            "master_enable" => {
                patch.master_enable = Some(
                    value
                        .as_bool()
                        .ok_or_else(|| "master_enable must be a boolean".to_string())?,
                );
            }
            "activation" => patch.activation = parse_activation(value)?,
            "transport_params" => {
                let (domain, flow) = parse_transport_params(role, value)?;
                patch.mxl_domain_id = domain;
                patch.mxl_flow_id = flow;
            }
            "transport_file" if role == Role::Receiver => check_transport_file(value)?,
            field if field == peer_field => {
                patch.peer_id = Some(if value.is_null() {
                    None
                } else {
                    Some(parse_nmos_uuid(value).ok_or_else(|| {
                        format!("{peer_field} must be null or a UUID")
                    })?)
                });
            }
            other => return Err(format!("'{other}' is not a staged parameter")),
        }
    }
    Ok(patch)
}

fn parse_activation(value: &Value) -> Result<Option<ActivationRequest>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "activation must be an object".to_string())?;
    if let Some(key) = object
        .keys()
        .find(|key| *key != "mode" && *key != "requested_time")
    {
        return Err(format!("'{key}' is not an activation parameter"));
    }
    let mode = object
        .get("mode")
        .ok_or_else(|| "activation.mode is required".to_string())?;
    let requested = match object.get("requested_time") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(
            text.parse::<TaiTime>()
                .map_err(|err| format!("activation.requested_time: {err}"))?,
        ),
        Some(_) => return Err("activation.requested_time must be a string or null".to_string()),
    };
    match mode {
        Value::Null => Ok(Some(ActivationRequest::Cancel)),
        Value::String(mode) => match mode.as_str() {
            "activate_immediate" => Ok(Some(ActivationRequest::Immediate)),
            "activate_scheduled_absolute" => requested
                .map(|t| Some(ActivationRequest::Absolute(t)))
                .ok_or_else(|| "a scheduled activation needs requested_time".to_string()),
            "activate_scheduled_relative" => requested
                .map(|t| Some(ActivationRequest::Relative(t)))
                .ok_or_else(|| "a scheduled activation needs requested_time".to_string()),
            other => Err(format!("'{other}' is not an IS-05 activation mode")),
        },
        _ => Err("activation.mode must be a string or null".to_string()),
    }
}

type ParamPair = (Option<Param>, Option<Param>);

fn parse_transport_params(role: Role, value: &Value) -> Result<ParamPair, String> {
    let legs = value
        .as_array()
        .ok_or_else(|| "transport_params must be an array".to_string())?;
    if legs.len() != 1 {
        return Err("MXL transport_params must contain exactly one leg".to_string());
    }
    let leg: &Map<String, Value> = legs[0]
        .as_object()
        .ok_or_else(|| "a transport_params leg must be an object".to_string())?;
    let mut domain = None;
    let mut flow = None;
    for (key, value) in leg {
        match key.as_str() {
            "mxl_domain_id" => domain = Some(parse_param(key, value)?),
            "mxl_flow_id" => {
                let param = parse_param(key, value)?;
                if role == Role::Receiver && param == Param::Auto {
                    return Err("receivers must not accept auto for mxl_flow_id".to_string());
                }
                flow = Some(param);
            }
            ext if is_ext_name(ext) => {
                if !(value.is_string() || value.is_boolean() || value.is_null() || value.is_number()) {
                    return Err(format!("{ext} must be a string, boolean, number or null"));
                }
            }
            other => return Err(format!("'{other}' is not an MXL transport parameter")),
        }
    }
    Ok((domain, flow))
}

fn is_ext_name(name: &str) -> bool {
    name.strip_prefix("ext_").is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

fn parse_param(name: &str, value: &Value) -> Result<Param, String> {
    match value {
        Value::Null => Ok(Param::Null),
        Value::String(text) if text == "auto" => Ok(Param::Auto),
        Value::String(_) => parse_mxl_uuid(value)
            .map(Param::Id)
            .ok_or_else(|| format!("{name} must be null, \"auto\" or a UUID")),
        _ => Err(format!("{name} must be null, \"auto\" or a UUID")),
    }
}

fn check_transport_file(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "transport_file must be an object".to_string())?;
    if let Some(key) = object.keys().find(|key| *key != "data" && *key != "type") {
        return Err(format!("'{key}' is not a transport_file parameter"));
    }
    let null = |key: &str| object.get(key).is_none_or(Value::is_null);
    if !(null("data") && null("type")) {
        return Err(
            "MXL receivers do not accept a transport file; omit it or set data and type to null"
                .to_string(),
        );
    }
    Ok(())
}

/// A lowercase, hyphenated UUID of version 1–5 with the RFC 4122 variant, as the
/// NMOS id pattern requires.
fn parse_nmos_uuid(value: &Value) -> Option<Uuid> {
    let text = value.as_str()?;
    let id = Uuid::parse_str(text).ok()?;
    let canonical = text.len() == 36 && text == id.hyphenated().to_string();
    let version_ok = (1..=5).contains(&id.get_version_num());
    let variant_ok = id.get_variant() == uuid::Variant::RFC4122;
    (canonical && version_ok && variant_ok).then_some(id)
}

/// An MXL id: any hyphenated UUID (`mxl_uuid.json`).
fn parse_mxl_uuid(value: &Value) -> Option<Uuid> {
    let text = value.as_str()?;
    let id = Uuid::parse_str(text).ok()?;
    (text.len() == 36 && text.eq_ignore_ascii_case(&id.hyphenated().to_string())).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_full_sender_patch() {
        let patch = parse_patch(
            Role::Sender,
            &json!({
                "receiver_id": null,
                "master_enable": true,
                "activation": {"mode": "activate_scheduled_absolute", "requested_time": "100:5"},
                "transport_params": [{"mxl_domain_id": "auto", "mxl_flow_id": "auto", "ext_x": 1}]
            }),
        )
        .unwrap();
        assert_eq!(patch.peer_id, Some(None));
        assert_eq!(patch.master_enable, Some(true));
        assert_eq!(
            patch.activation,
            Some(ActivationRequest::Absolute(TaiTime::new(100, 5)))
        );
        assert_eq!(patch.mxl_domain_id, Some(Param::Auto));
        assert_eq!(patch.mxl_flow_id, Some(Param::Auto));
    }

    #[test]
    fn rejects_what_the_schemas_reject() {
        for (role, body) in [
            (Role::Sender, json!([])),
            (Role::Sender, json!({"sender_id": null})),
            (Role::Sender, json!({"transport_file": {"data": null, "type": null}})),
            (Role::Sender, json!({"master_enable": "yes"})),
            (Role::Sender, json!({"activation": {}})),
            (Role::Sender, json!({"activation": {"mode": "now"}})),
            (Role::Sender, json!({"activation": {"mode": "activate_scheduled_relative"}})),
            (Role::Sender, json!({"activation": {"mode": null, "activation_time": "1:0"}})),
            (Role::Sender, json!({"activation": {"mode": "activate_scheduled_absolute", "requested_time": "1.5"}})),
            (Role::Sender, json!({"transport_params": [{}, {}]})),
            (Role::Sender, json!({"transport_params": [{"destination_ip": "1.2.3.4"}]})),
            (Role::Sender, json!({"transport_params": [{"mxl_flow_id": "not-a-uuid"}]})),
            (Role::Sender, json!({"receiver_id": "ABCDEFAB-1111-4111-8111-111111111111"})),
            (Role::Receiver, json!({"transport_params": [{"mxl_flow_id": "auto"}]})),
            (Role::Receiver, json!({"transport_file": {"data": "v=0", "type": "application/sdp"}})),
        ] {
            assert!(parse_patch(role, &body).is_err(), "{role:?} {body}");
        }
    }

    #[test]
    fn receiver_accepts_null_transport_file_and_cancel() {
        let patch = parse_patch(
            Role::Receiver,
            &json!({
                "sender_id": "11111111-1111-4111-8111-111111111111",
                "transport_file": {"data": null, "type": null},
                "activation": {"mode": null, "requested_time": null}
            }),
        )
        .unwrap();
        assert_eq!(patch.activation, Some(ActivationRequest::Cancel));
        assert!(patch.peer_id.unwrap().is_some());
    }

    #[test]
    fn domain_ids_must_be_visible() {
        let domain = MxlDomain {
            id: Uuid::new_v4(),
            label: "d".into(),
            path: "/tmp/d".into(),
        };
        let mut staged = StagedConnection::default();
        let patch = StagePatch {
            mxl_domain_id: Some(Param::Id(Uuid::new_v4())),
            ..StagePatch::default()
        };
        assert!(patch.apply(Role::Sender, &mut staged, std::slice::from_ref(&domain)).is_err());
        assert!(patch.apply(Role::Sender, &mut staged, &[]).is_err());
        let ok = StagePatch {
            mxl_domain_id: Some(Param::Id(domain.id)),
            ..StagePatch::default()
        };
        ok.apply(Role::Sender, &mut staged, std::slice::from_ref(&domain)).unwrap();
        assert_eq!(staged.mxl_domain_id, Param::Id(domain.id));
    }
}
