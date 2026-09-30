//! MXL domain identity. BCP-007-03 identifies a domain by the UUID in
//! `domain_def.json`, not by the path it is mounted at.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

/// One MXL domain this process can read or write.
#[derive(Debug, Clone)]
pub struct MxlDomain {
    pub id: Uuid,
    pub label: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct DomainDef {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
}

/// Read `domain_def.json` from each configured directory.
pub fn scan_domains(paths: &[PathBuf]) -> Vec<MxlDomain> {
    let mut found = Vec::new();
    for path in paths {
        match read_domain(path) {
            Ok(Some(domain)) => {
                tracing::info!(
                    "NMOS MXL domain {} ({}) at {}",
                    domain.id,
                    if domain.label.is_empty() {
                        domain.description.as_str()
                    } else {
                        domain.label.as_str()
                    },
                    domain.path.display()
                );
                found.push(domain);
            }
            Ok(None) => {}
            Err(err) => warn!(
                "MXL domain {} is not usable for NMOS: {err}",
                path.display()
            ),
        }
    }
    found.sort_by_key(|domain| domain.id);
    found.dedup_by_key(|domain| domain.id);
    found
}

fn read_domain(path: &Path) -> Result<Option<MxlDomain>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let def_path = path.join("domain_def.json");
    if !def_path.is_file() {
        return Err("domain_def.json is missing".to_string());
    }
    let text = fs::read_to_string(&def_path).map_err(|err| err.to_string())?;
    let def: DomainDef = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let id = Uuid::parse_str(def.id.trim()).map_err(|err| format!("id: {err}"))?;
    Ok(Some(MxlDomain {
        id,
        label: def.label,
        description: def.description,
        path: path.to_path_buf(),
    }))
}

/// A flow directory created by libmxl is `{flow-id}.mxl-flow`.
pub fn domain_has_flow(domain: &Path, flow_id: Uuid) -> bool {
    domain.join(format!("{flow_id}.mxl-flow")).is_dir()
}
