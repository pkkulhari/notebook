use loro::{LoroDoc, LoroMap, LoroText, LoroValue, ValueOrContainer, VersionVector};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

const BODY: &str = "body";
const META: &str = "meta";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Notebook,
    Note,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Notebook => "notebook",
            Kind::Note => "note",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "notebook" => Some(Kind::Notebook),
            "note" => Some(Kind::Note),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteMeta {
    /// The notebook the note was last put in, which may since have been deleted.
    pub notebook: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotebookMeta {
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

/// A fresh writer identity. Never persist it: a copied or restored database
/// that reused one would create different changes with the same IDs.
pub fn random_peer() -> u64 {
    // PeerID::MAX is reserved by Loro.
    uuid::Uuid::new_v4().as_u64_pair().0 >> 1
}

/// Deriving a migrated document's writer identity from its content means two
/// devices that migrate copies of the same database produce byte-identical
/// changes, which merge instead of duplicating every note's text.
pub fn genesis_peer(parts: &[&[u8]]) -> u64 {
    let mut hasher = blake3::Hasher::new_derive_key("notebook genesis peer v1");
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let bytes: [u8; 8] = hasher.finalize().as_bytes()[..8].try_into().unwrap();
    u64::from_le_bytes(bytes) >> 1
}

pub fn text(doc: &LoroDoc) -> LoroText {
    doc.get_text(BODY)
}

pub fn meta(doc: &LoroDoc) -> LoroMap {
    doc.get_map(META)
}

fn get_i64(map: &LoroMap, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::I64(value))) => Some(value),
        _ => None,
    }
}

fn get_string(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
        _ => None,
    }
}

pub fn set_deleted(map: &LoroMap, deleted_at: Option<i64>) -> Result<()> {
    map.insert(
        "deleted_at",
        deleted_at.map_or(LoroValue::Null, LoroValue::from),
    )?;
    Ok(())
}

pub fn new_note(
    peer: u64,
    body: &str,
    notebook: &str,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
) -> Result<LoroDoc> {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer)?;
    if !body.is_empty() {
        text(&doc).insert(0, body)?;
    }
    let map = meta(&doc);
    map.insert("notebook", notebook)?;
    map.insert("created_at", created_at)?;
    map.insert("updated_at", updated_at)?;
    set_deleted(&map, deleted_at)?;
    doc.commit();
    Ok(doc)
}

pub fn new_notebook(
    peer: u64,
    name: &str,
    created_at: i64,
    updated_at: i64,
    deleted_at: Option<i64>,
) -> Result<LoroDoc> {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer)?;
    let map = meta(&doc);
    map.insert("name", name)?;
    map.insert("created_at", created_at)?;
    map.insert("updated_at", updated_at)?;
    set_deleted(&map, deleted_at)?;
    doc.commit();
    Ok(doc)
}

pub fn read_note(doc: &LoroDoc, default_notebook: &str) -> NoteMeta {
    let map = meta(doc);
    let created_at = get_i64(&map, "created_at").unwrap_or(0);
    NoteMeta {
        notebook: get_string(&map, "notebook").unwrap_or_else(|| default_notebook.into()),
        created_at,
        updated_at: get_i64(&map, "updated_at").unwrap_or(created_at),
        deleted_at: get_i64(&map, "deleted_at"),
    }
}

pub fn read_notebook(doc: &LoroDoc) -> NotebookMeta {
    let map = meta(doc);
    let created_at = get_i64(&map, "created_at").unwrap_or(0);
    NotebookMeta {
        name: get_string(&map, "name").unwrap_or_else(|| "Untitled".into()),
        created_at,
        updated_at: get_i64(&map, "updated_at").unwrap_or(created_at),
        deleted_at: get_i64(&map, "deleted_at"),
    }
}

pub fn load(snapshot: &[u8], updates: &[Vec<u8>]) -> Result<LoroDoc> {
    let doc = LoroDoc::from_snapshot(snapshot)?;
    doc.import_batch(updates)?;
    Ok(doc)
}

/// Encodes a version vector canonically: entries sorted by peer, so equal
/// versions always produce equal bytes and equal digests on every device.
pub fn encode_version(vv: &VersionVector) -> Vec<u8> {
    let mut entries: Vec<(u64, i32)> = vv.iter().map(|(p, c)| (*p, *c)).collect();
    entries.sort_unstable();
    let mut bytes = Vec::with_capacity(entries.len() * 12);
    for (peer, counter) in entries {
        bytes.extend_from_slice(&peer.to_be_bytes());
        bytes.extend_from_slice(&counter.to_be_bytes());
    }
    bytes
}

pub fn decode_version(bytes: &[u8]) -> Result<VersionVector> {
    if !bytes.len().is_multiple_of(12) {
        return Err("Malformed document version".into());
    }
    let mut vv = VersionVector::new();
    for entry in bytes.chunks_exact(12) {
        let peer = u64::from_be_bytes(entry[..8].try_into().unwrap());
        let counter = i32::from_be_bytes(entry[8..].try_into().unwrap());
        vv.insert(peer, counter);
    }
    Ok(vv)
}

pub fn bucket(id: &str) -> u8 {
    blake3::hash(id.as_bytes()).as_bytes()[0]
}

/// One document's contribution to its bucket digest. Buckets XOR these, so a
/// digest is independent of row order and cheap to recompute.
pub fn entry_digest(id: &str, version: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(id.len() as u64).to_le_bytes());
    hasher.update(id.as_bytes());
    hasher.update(version);
    *hasher.finalize().as_bytes()
}

pub fn differing_buckets(ours: &[[u8; 32]], theirs: &[[u8; 32]]) -> Vec<u8> {
    (0..=255u8)
        .filter(|&b| ours.get(b as usize) != theirs.get(b as usize))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_round_trip_canonically() {
        let doc = LoroDoc::new();
        doc.set_peer_id(7).unwrap();
        text(&doc).insert(0, "a").unwrap();
        doc.commit();
        let other = LoroDoc::new();
        other.set_peer_id(3).unwrap();
        other
            .import(&doc.export(loro::ExportMode::all_updates()).unwrap())
            .unwrap();
        text(&other).insert(1, "b").unwrap();
        other.commit();
        let vv = other.oplog_vv();
        let bytes = encode_version(&vv);
        assert_eq!(decode_version(&bytes).unwrap(), vv);
        assert_eq!(encode_version(&decode_version(&bytes).unwrap()), bytes);
        assert!(vv.includes_vv(&doc.oplog_vv()));
        assert!(!doc.oplog_vv().includes_vv(&vv));
    }

    #[test]
    fn genesis_peers_depend_on_every_part() {
        let a = genesis_peer(&[b"id", b"body"]);
        assert_eq!(a, genesis_peer(&[b"id", b"body"]));
        assert_ne!(a, genesis_peer(&[b"id", b"bod", b"y"]));
        assert_ne!(a, genesis_peer(&[b"id", b"other"]));
        assert!(a < u64::MAX);
    }
}
