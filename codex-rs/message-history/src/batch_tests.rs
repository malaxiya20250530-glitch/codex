use std::fs::File;
use std::io::Write;

use codex_config::types::History;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::*;

fn entry(offset: usize, text: impl Into<String>) -> HistoryEntry {
    HistoryEntry {
        session_id: "session".to_string(),
        ts: offset as u64,
        text: text.into(),
    }
}

fn write_entries(home: &TempDir, entries: &[HistoryEntry]) -> HistoryConfig {
    let mut file = File::create(home.path().join(HISTORY_FILENAME)).expect("create history");
    for entry in entries {
        writeln!(
            file,
            "{}",
            serde_json::to_string(entry).expect("serialize entry")
        )
        .expect("write entry");
    }
    HistoryConfig::new(home.path(), &History::default())
}

async fn batch_for(entries: &[HistoryEntry], end_offset: usize) -> (TempDir, HistoryBatch) {
    let home = TempDir::new().expect("temp dir");
    let config = write_entries(&home, entries);
    let (log_id, _) = history_metadata(&config).await;
    let batch = lookup_batch(log_id, end_offset, &config);
    (home, batch)
}

#[tokio::test]
async fn search_batch_returns_bounded_newest_first_absolute_offsets() {
    let entries: Vec<_> = (0..200)
        .map(|offset| entry(offset, format!("row {offset}")))
        .collect();
    let (_home, batch) = batch_for(&entries, /*end_offset*/ 199).await;

    assert_eq!(batch.entries.len(), 128);
    assert_eq!(batch.entries.first().map(|entry| entry.offset), Some(199));
    assert_eq!(batch.entries.last().map(|entry| entry.offset), Some(72));
    assert_eq!(batch.next_older_offset, Some(71));
    assert_eq!(batch.entries[0].entry, Some(entries[199].clone()));
}

#[tokio::test]
async fn search_batch_stitches_chunks_and_keeps_malformed_offsets() {
    let home = TempDir::new().expect("temp dir");
    let first = entry(0, "a".repeat(HISTORY_READ_BUFFER_SIZE + 17));
    let third = entry(2, "third");
    let contents = format!(
        "{}\nnot-json\n{}\n",
        serde_json::to_string(&first).expect("serialize first"),
        serde_json::to_string(&third).expect("serialize third")
    );
    std::fs::write(home.path().join(HISTORY_FILENAME), contents).expect("write history");
    let config = HistoryConfig::new(home.path(), &History::default());
    let (log_id, _) = history_metadata(&config).await;

    assert_eq!(
        lookup_batch(log_id, /*end_offset*/ 2, &config),
        HistoryBatch {
            entries: vec![
                HistoryBatchEntry {
                    offset: 2,
                    entry: Some(third),
                },
                HistoryBatchEntry {
                    offset: 1,
                    entry: None,
                },
                HistoryBatchEntry {
                    offset: 0,
                    entry: Some(first),
                },
            ],
            next_older_offset: None,
        }
    );
}

#[tokio::test]
async fn search_batch_preserves_identity_append_trim_and_short_file_semantics() {
    let home = TempDir::new().expect("temp dir");
    let initial = vec![entry(0, "zero"), entry(1, "one")];
    let config = write_entries(&home, &initial);
    let (log_id, _) = history_metadata(&config).await;
    assert_eq!(
        lookup_batch(log_id.wrapping_add(1), /*end_offset*/ 1, &config),
        HistoryBatch::default()
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(home.path().join(HISTORY_FILENAME))
        .expect("open history");
    writeln!(
        file,
        "{}",
        serde_json::to_string(&entry(2, "appended")).expect("serialize append")
    )
    .expect("append entry");
    let batch = lookup_batch(log_id, /*end_offset*/ 1, &config);
    assert_eq!(
        batch.entries,
        vec![
            HistoryBatchEntry {
                offset: 1,
                entry: Some(initial[1].clone()),
            },
            HistoryBatchEntry {
                offset: 0,
                entry: Some(initial[0].clone()),
            },
        ]
    );

    let newest = "c".repeat(200);
    let history = History {
        max_bytes: Some(newest.len() + 80),
        ..History::default()
    };
    let trimmed_config = HistoryConfig::new(home.path(), &history);
    append_entry(&newest, "session", &trimmed_config)
        .await
        .expect("append and trim");
    let trimmed = lookup_batch(log_id, /*end_offset*/ 20, &trimmed_config);
    assert_eq!(trimmed.entries.len(), 1);
    assert_eq!(trimmed.entries[0].offset, 0);
    assert_eq!(
        trimmed.entries[0].entry.as_ref().map(|entry| &entry.text),
        Some(&newest)
    );
    assert_eq!(trimmed.next_older_offset, None);
}

#[tokio::test]
async fn search_batch_enforces_byte_cap_and_oversized_row_progress() {
    let entries: Vec<_> = (0..5)
        .map(|offset| {
            entry(
                offset,
                char::from(b'a' + offset as u8).to_string().repeat(20_000),
            )
        })
        .collect();
    let (_home, batch) = batch_for(&entries, /*end_offset*/ 4).await;
    assert_eq!(batch.entries.len(), 3);
    assert_eq!(batch.entries.first().map(|entry| entry.offset), Some(4));
    assert_eq!(batch.entries.last().map(|entry| entry.offset), Some(2));
    assert_eq!(batch.next_older_offset, Some(1));

    let entries = vec![entry(0, "small"), entry(1, "x".repeat(70_000))];
    let (home, oversized) = batch_for(&entries, /*end_offset*/ 1).await;
    assert_eq!(oversized.entries.len(), 1);
    assert_eq!(oversized.entries[0].entry, Some(entries[1].clone()));
    assert_eq!(oversized.next_older_offset, Some(0));
    let config = HistoryConfig::new(home.path(), &History::default());
    let (log_id, _) = history_metadata(&config).await;
    let next = lookup_batch(log_id, /*end_offset*/ 0, &config);
    assert_eq!(next.entries[0].entry, Some(entries[0].clone()));
    assert_eq!(next.next_older_offset, None);
}
