//! Read-only, exact-ID identity hydration. Unknown fields (including transcripts)
//! are skipped by serde rather than materialized. Limits fail closed to unknown,
//! never to an observed root or a stale prefix truncated by the IO budget.
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
const JOURNAL_BYTES: u64 = 32 * 1024 * 1024;
const JOURNAL_LINE_BYTES: u64 = 4 * 1024 * 1024;
const JOURNAL_LINES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LiveSessionIdentity {
    pub(super) parent_id: Option<String>,
    pub(super) is_debug: bool,
}

// Derive also accepts JSON sequences for structs. Require an actual metadata
// object so an invalid snapshot such as [] cannot become an observed root.
impl<'de> Deserialize<'de> for LiveSessionIdentity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IdentityVisitor;
        impl<'de> serde::de::Visitor<'de> for IdentityVisitor {
            type Value = LiveSessionIdentity;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a session metadata object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                #[derive(Deserialize)]
                struct Fields {
                    #[serde(default)]
                    parent_id: Option<String>,
                    #[serde(default)]
                    is_debug: bool,
                }
                let fields =
                    Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(LiveSessionIdentity {
                    parent_id: fields.parent_id,
                    is_debug: fields.is_debug,
                })
            }
        }
        deserializer.deserialize_map(IdentityVisitor)
    }
}

#[derive(Deserialize)]
struct JournalEntry {
    meta: LiveSessionIdentity,
}

pub(super) fn load_live_session_identity(
    sessions_dir: &Path,
    session_id: &str,
) -> Option<LiveSessionIdentity> {
    // Restrict to a portable single filename component, not a path or CLI ID.
    if session_id.is_empty()
        || session_id.len() > 240
        || session_id == "."
        || session_id == ".."
        || !session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    {
        return None;
    }
    let snapshot = File::open(sessions_dir.join(format!("{session_id}.json"))).ok()?;
    if !snapshot.metadata().ok()?.is_file() || snapshot.metadata().ok()?.len() > SNAPSHOT_BYTES {
        return None;
    }
    let mut snapshot = snapshot.take(SNAPSHOT_BYTES + 1);
    let mut identity =
        serde_json::from_reader::<_, LiveSessionIdentity>(BufReader::new(&mut snapshot)).ok()?;
    if snapshot.limit() == 0 {
        return None;
    }
    let journal = match File::open(sessions_dir.join(format!("{session_id}.journal.jsonl"))) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(identity),
        Err(_) => return None,
    };
    let metadata = journal.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > JOURNAL_BYTES {
        return None;
    }
    let mut reader = BufReader::new(journal.take(JOURNAL_BYTES + 1));
    let mut line = Vec::new();
    let mut total = 0;
    let mut lines = 0;
    loop {
        line.clear();
        let size = reader
            .by_ref()
            .take(JOURNAL_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)
            .ok()?;
        if size == 0 {
            return Some(identity);
        }
        total += size as u64;
        lines += 1;
        if size as u64 > JOURNAL_LINE_BYTES || total > JOURNAL_BYTES || lines > JOURNAL_LINES {
            return None;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<JournalEntry>(&line) {
            Ok(entry) => identity = entry.meta,
            // Match the summary loader: keep the last valid journal prefix.
            Err(_) => return Some(identity),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_identity_snapshot_matrix_including_zero_messages() {
        let dir = tempfile::tempdir().unwrap();
        for parent in [None, Some("root")] {
            for debug in [false, true] {
                let snapshot = serde_json::json!({"parent_id": parent, "is_debug": debug,
                    "messages": [], "tool_content": {"arbitrary": [1, 2, 3]}});
                std::fs::write(dir.path().join("id.json"), snapshot.to_string()).unwrap();
                assert_eq!(
                    load_live_session_identity(dir.path(), "id"),
                    Some(LiveSessionIdentity {
                        parent_id: parent.map(str::to_owned),
                        is_debug: debug,
                    })
                );
            }
        }
    }

    #[test]
    fn live_identity_missing_bad_snapshot_and_hostile_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "missing"), None);
        for content in ["{", "null", "[]", r#"{"is_debug":"true"}"#] {
            std::fs::write(dir.path().join("id.json"), content).unwrap();
            assert_eq!(load_live_session_identity(dir.path(), "id"), None);
        }
        for id in ["", ".", "..", "../id", "/id", "a/b", "a\\b", "a:b", "a\0b"] {
            assert_eq!(load_live_session_identity(dir.path(), id), None);
        }
    }

    #[test]
    fn live_identity_journal_order_null_parent_and_malformed_tail() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("id.json"),
            r#"{"parent_id":"old","is_debug":false}"#,
        )
        .unwrap();
        let journal = dir.path().join("id.journal.jsonl");
        std::fs::write(&journal, concat!(
            "\n", r#"{"meta":{"parent_id":"new","is_debug":true},"append_messages":[{"content":"ignored"}]}"#,
            "\n", r#"{"meta":{"parent_id":null,"is_debug":false}}"#, "\n{broken\n",
            r#"{"meta":{"parent_id":"must-not-read","is_debug":true}}"#,
        )).unwrap();
        assert_eq!(
            load_live_session_identity(dir.path(), "id"),
            Some(LiveSessionIdentity {
                parent_id: None,
                is_debug: false,
            })
        );
        std::fs::write(&journal, r#"{"meta":{"parent_id":"new","is_debug":true}}"#).unwrap();
        assert_eq!(
            load_live_session_identity(dir.path(), "id"),
            Some(LiveSessionIdentity {
                parent_id: Some("new".into()),
                is_debug: true,
            })
        );
    }

    #[test]
    fn live_identity_exact_id_and_missing_snapshot_ignore_other_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("id-other.json"),
            r#"{"parent_id":"other","is_debug":true}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("id.journal.jsonl"),
            r#"{"meta":{"parent_id":"journal","is_debug":true}}"#,
        )
        .unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "id"), None);
        std::fs::write(
            dir.path().join("id.json"),
            r#"{"parent_id":null,"is_debug":false}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("id.journal.jsonl"), "malformed\n").unwrap();
        assert_eq!(
            load_live_session_identity(dir.path(), "id"),
            Some(LiveSessionIdentity {
                parent_id: None,
                is_debug: false,
            })
        );
    }

    #[test]
    fn live_identity_io_budgets_return_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = dir.path().join("id.json");
        File::create(&snapshot)
            .unwrap()
            .set_len(SNAPSHOT_BYTES + 1)
            .unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "id"), None);
        std::fs::write(&snapshot, r#"{"is_debug":false}"#).unwrap();
        let journal = dir.path().join("id.journal.jsonl");
        File::create(&journal)
            .unwrap()
            .set_len(JOURNAL_BYTES + 1)
            .unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "id"), None);
        std::fs::write(&journal, " ".repeat(JOURNAL_LINE_BYTES as usize + 1)).unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "id"), None);
        std::fs::write(&journal, "\n".repeat(JOURNAL_LINES + 1)).unwrap();
        assert_eq!(load_live_session_identity(dir.path(), "id"), None);
    }
}
