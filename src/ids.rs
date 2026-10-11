//! Deterministic NMOS ids (G3).
//!
//! The formulas are Strom's, so Strom keeps every id it announced before the
//! extraction: the node id is UUIDv5 of the seed, and every other resource id is
//! UUIDv5 of `"{role}:{key}"` in the node id's namespace.

use std::path::Path;

use uuid::Uuid;

/// Namespace of every id derived from `NMOS_SEED`:
/// `UUIDv5(NAMESPACE_OID, "leeo86.strom.nmos.v1")`.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x8c76aff5_a8f9_53ce_97d0_766f1074b699);

/// Node id for `seed`. The same seed always gives the same id.
pub fn node_id_from_seed(seed: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, seed.as_bytes())
}

/// A stable id for `key` under `seed`, for ids the application owns (for example
/// MXL flow ids or its output domain id): `UUIDv5(NAMESPACE, "{seed}/{key}")`.
pub fn derive_id(seed: &str, key: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, format!("{seed}/{key}").as_bytes())
}

/// Id of a resource of `role` (`device`, `source`, `flow`, `sender`, `receiver`)
/// with the application's stable `key`.
pub(crate) fn resource_id(node_id: Uuid, role: &str, key: &str) -> Uuid {
    Uuid::new_v5(&node_id, format!("{role}:{key}").as_bytes())
}

/// The node id: from the seed, else the explicit id, else the id stored in
/// `id_path`, else a new random id (stored in `id_path` when given).
pub(crate) fn node_id(seed: Option<&str>, explicit: Option<Uuid>, id_path: Option<&Path>) -> Uuid {
    if let Some(seed) = seed.filter(|seed| !seed.is_empty()) {
        let id = node_id_from_seed(seed);
        if let Some(path) = id_path {
            let current = std::fs::read_to_string(path).ok();
            if current.as_deref().map(str::trim) != Some(id.to_string().as_str()) {
                write_id(path, id);
            }
        }
        return id;
    }
    if let Some(id) = explicit {
        return id;
    }
    if let Some(path) = id_path {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(id) = Uuid::parse_str(text.trim()) {
                return id;
            }
        }
        let id = Uuid::new_v4();
        write_id(path, id);
        return id;
    }
    Uuid::new_v4()
}

fn write_id(path: &Path, id: Uuid) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(path, id.to_string()) {
        tracing::warn!("NMOS node id not saved to {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_the_documented_uuid5() {
        assert_eq!(
            NAMESPACE,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"leeo86.strom.nmos.v1")
        );
    }

    #[test]
    fn seed_ids_are_stable() {
        let node = node_id_from_seed("sport-sa-strom");
        assert_eq!(node, node_id_from_seed("sport-sa-strom"));
        assert_ne!(node, node_id_from_seed("other"));
        assert_eq!(
            resource_id(node, "sender", "a"),
            resource_id(node, "sender", "a")
        );
        assert_ne!(
            resource_id(node, "sender", "a"),
            resource_id(node, "receiver", "a")
        );
        assert_eq!(derive_id("s", "k"), Uuid::new_v5(&NAMESPACE, b"s/k"));
        assert_eq!(node.get_version_num(), 5);
    }

    #[test]
    fn seed_wins_and_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/node.id");
        let id = node_id(Some("seed"), None, Some(&path));
        assert_eq!(id, node_id_from_seed("seed"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), id.to_string());
    }

    #[test]
    fn random_id_survives_a_restart_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.id");
        let first = node_id(None, None, Some(&path));
        assert_eq!(node_id(None, None, Some(&path)), first);
        let explicit = Uuid::new_v4();
        assert_eq!(node_id(None, Some(explicit), Some(&path)), explicit);
    }
}
