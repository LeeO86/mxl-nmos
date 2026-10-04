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

impl MxlDomain {
    /// The label, or the description when the label is empty.
    pub fn name(&self) -> &str {
        if self.label.is_empty() {
            &self.description
        } else {
            &self.label
        }
    }
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
                tracing::debug!(
                    "NMOS MXL domain {} ({}) at {}",
                    domain.id,
                    domain.name(),
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

/// Key libmxl reads from a domain `options.json`.
pub const HISTORY_DURATION_OPTION: &str = "urn:x-mxl:option:history_duration/v1.0";

/// libmxl's built-in history when `options.json` does not set one (200 ms).
pub const DEFAULT_HISTORY_DURATION_NS: u64 = 200_000_000;

/// Every child directory of `root` that has a readable `domain_def.json`.
/// Mirror domains (`mirror-*`) are included. The root itself is not a domain.
pub fn scan_domain_root(root: &Path) -> Vec<MxlDomain> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            paths.push(path);
        }
    }
    scan_domains(&paths)
}

/// Create this function's output domain once.
///
/// An existing `domain_def.json` with the same id is left untouched. A
/// different id is an error and the file is not overwritten. `options.json`
/// is written only when it is missing.
pub fn ensure_output_domain(
    dir: &Path,
    id: Uuid,
    label: &str,
    history_duration_ns: u64,
) -> Result<(), String> {
    if is_mxl_root(dir) {
        return Err(format!(
            "refusing to use {} as an output domain; it is an MXL root",
            dir.display()
        ));
    }
    fs::create_dir_all(dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
    let def_path = dir.join("domain_def.json");
    if def_path.is_file() {
        match read_domain(dir) {
            Ok(Some(existing)) if existing.id == id => {}
            Ok(Some(existing)) => {
                return Err(format!(
                    "domain {} already has id {}, not {id}; not overwriting",
                    dir.display(),
                    existing.id
                ));
            }
            Ok(None) => {
                write_domain_def(&def_path, id, label)?;
            }
            Err(err) => return Err(format!("{}: {err}", def_path.display())),
        }
    } else {
        write_domain_def(&def_path, id, label)?;
    }
    let options_path = dir.join("options.json");
    if !options_path.exists() {
        let body = serde_json::json!({ HISTORY_DURATION_OPTION: history_duration_ns });
        fs::write(&options_path, body.to_string())
            .map_err(|err| format!("write {}: {err}", options_path.display()))?;
    }
    Ok(())
}

fn write_domain_def(path: &Path, id: Uuid, label: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "id": id.to_string(),
        "label": label,
        "description": "Strom MXL output domain",
        "tags": {}
    });
    fs::write(
        path,
        serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
    )
    .map_err(|err| format!("write {}: {err}", path.display()))
}

/// Delete this function's own output domain. Never deletes an MXL root.
pub fn remove_output_domain(dir: &Path, scan_root: &Path) -> Result<(), String> {
    if is_mxl_root(dir) || dir == scan_root {
        return Err(format!(
            "refusing to delete {} because it is an MXL root",
            dir.display()
        ));
    }
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(dir).map_err(|err| format!("remove {}: {err}", dir.display()))
}

fn is_mxl_root(dir: &Path) -> bool {
    dir == Path::new("/")
        || dir == Path::new("/Volumes/mxl")
        || dir == Path::new("/dev/shm/mxl")
        || dir == Path::new("/dev/shm")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_includes_mirror_directories() {
        let root = tempfile::tempdir().unwrap();
        let mirror = root.path().join("mirror-studio");
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(
            mirror.join("domain_def.json"),
            r#"{"id":"3310f209-9351-47c0-b9a2-14c59b6a4c23","label":"Mirror","description":"","tags":{}}"#,
        )
        .unwrap();
        let found = scan_domain_root(root.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Mirror");
    }

    #[test]
    fn existing_domain_id_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::parse_str("3310f209-9351-47c0-b9a2-14c59b6a4c23").unwrap();
        std::fs::write(
            dir.path().join("domain_def.json"),
            r#"{"id":"11111111-1111-4111-8111-111111111111","label":"Other","description":"","tags":{}}"#,
        )
        .unwrap();
        let err = ensure_output_domain(dir.path(), id, "Strom", DEFAULT_HISTORY_DURATION_NS);
        assert!(err.is_err());
        let text = std::fs::read_to_string(dir.path().join("domain_def.json")).unwrap();
        assert!(text.contains("11111111-1111-4111-8111-111111111111"));
        assert!(!dir.path().join("options.json").exists());
    }

    #[test]
    fn options_json_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::parse_str("3310f209-9351-47c0-b9a2-14c59b6a4c23").unwrap();
        ensure_output_domain(dir.path(), id, "Strom", 100).unwrap();
        let first = std::fs::read_to_string(dir.path().join("options.json")).unwrap();
        ensure_output_domain(dir.path(), id, "Strom", 999).unwrap();
        let second = std::fs::read_to_string(dir.path().join("options.json")).unwrap();
        assert_eq!(first, second);
        assert!(first.contains("100"));
        let removed = remove_output_domain(dir.path(), &dir.path().join(".."));
        assert!(removed.is_ok());
        assert!(!dir.path().exists());
    }

    #[test]
    fn mxl_root_is_not_deleted() {
        let err = remove_output_domain(Path::new("/Volumes/mxl"), Path::new("/Volumes/mxl"));
        assert!(err.is_err());
    }
}
