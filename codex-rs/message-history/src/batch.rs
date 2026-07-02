use std::collections::VecDeque;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use memchr::memchr_iter;

use super::HISTORY_READ_BUFFER_SIZE;
use super::HistoryConfig;
use super::HistoryEntry;
use super::MAX_RETRIES;
use super::RETRY_SLEEP;
use super::history_filepath;
use super::log_identity;

const MAX_BATCH_ROWS: usize = 128;
const MAX_BATCH_BYTES: usize = 64 * 1024;

/// One absolute history offset covered by a bounded lookup.
///
/// Malformed records retain their offset with `entry` set to `None`, allowing callers to continue
/// searching older valid records without changing offset semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryBatchEntry {
    pub offset: usize,
    pub entry: Option<HistoryEntry>,
}

/// A bounded newest-first suffix ending at a requested absolute history offset.
///
/// `next_older_offset` identifies the next offset a caller should request after exhausting
/// `entries`. It is explicit because the byte cap can make a batch contain fewer than 128 rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistoryBatch {
    pub entries: Vec<HistoryBatchEntry>,
    pub next_older_offset: Option<usize>,
}

struct RawHistoryBatchEntry {
    offset: usize,
    bytes: Vec<u8>,
}

/// Look up a bounded batch of history records ending at `end_offset`.
///
/// The file is opened, identity-checked, and shared-locked once. Records are counted from the
/// oldest offset while the file is streamed with forward newline discovery. The result retains at
/// most 128 rows and 64 KiB of raw JSONL, except that one oversized newest row is returned alone so
/// callers always make progress.
pub fn lookup_batch(log_id: u64, end_offset: usize, config: &HistoryConfig) -> HistoryBatch {
    let path = history_filepath(config);
    match lookup_batch_from_file(&path, log_id, end_offset) {
        Ok(batch) => batch,
        Err(error) => {
            tracing::warn!(%error, "failed to read history batch");
            HistoryBatch::default()
        }
    }
}

fn lookup_batch_from_file(
    path: &Path,
    log_id: u64,
    end_offset: usize,
) -> std::io::Result<HistoryBatch> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let current_log_id = log_identity(&file.metadata()?).unwrap_or(0);
    if log_id != 0 && current_log_id != log_id {
        return Ok(HistoryBatch::default());
    }

    for _ in 0..MAX_RETRIES {
        match file.try_lock_shared() {
            Ok(()) => return scan_batch(&mut file, end_offset),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(RETRY_SLEEP),
            Err(error) => return Err(error.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "could not acquire shared history lock after multiple attempts",
    ))
}

fn scan_batch(file: &mut File, end_offset: usize) -> std::io::Result<HistoryBatch> {
    let mut suffix = VecDeque::new();
    let mut suffix_bytes = 0usize;
    let mut pending = Vec::new();
    let mut read_buffer = [0u8; HISTORY_READ_BUFFER_SIZE];
    let mut offset = 0usize;

    loop {
        let read = file.read(&mut read_buffer)?;
        if read == 0 {
            if !pending.is_empty() && offset <= end_offset {
                retain_row(&mut suffix, &mut suffix_bytes, offset, pending);
            }
            return Ok(finish_batch(suffix));
        }

        let chunk = &read_buffer[..read];
        let mut row_start = 0;
        for newline in memchr_iter(b'\n', chunk) {
            pending.extend_from_slice(&chunk[row_start..=newline]);
            if offset <= end_offset {
                retain_row(
                    &mut suffix,
                    &mut suffix_bytes,
                    offset,
                    std::mem::take(&mut pending),
                );
            }
            if offset == end_offset {
                return Ok(finish_batch(suffix));
            }
            offset = offset.saturating_add(1);
            row_start = newline + 1;
        }
        pending.extend_from_slice(&chunk[row_start..]);
    }
}

fn retain_row(
    suffix: &mut VecDeque<RawHistoryBatchEntry>,
    suffix_bytes: &mut usize,
    offset: usize,
    bytes: Vec<u8>,
) {
    let row_bytes = bytes.len();
    if row_bytes > MAX_BATCH_BYTES {
        suffix.clear();
        *suffix_bytes = row_bytes;
        suffix.push_back(RawHistoryBatchEntry { offset, bytes });
        return;
    }

    *suffix_bytes += row_bytes;
    suffix.push_back(RawHistoryBatchEntry { offset, bytes });
    while suffix.len() > MAX_BATCH_ROWS || *suffix_bytes > MAX_BATCH_BYTES {
        if let Some(removed) = suffix.pop_front() {
            *suffix_bytes -= removed.bytes.len();
        }
    }
}

fn finish_batch(suffix: VecDeque<RawHistoryBatchEntry>) -> HistoryBatch {
    let next_older_offset = suffix.front().and_then(|entry| entry.offset.checked_sub(1));
    let entries = suffix
        .into_iter()
        .rev()
        .map(|raw| HistoryBatchEntry {
            offset: raw.offset,
            entry: parse_entry(&raw.bytes),
        })
        .collect();
    HistoryBatch {
        entries,
        next_older_offset,
    }
}

fn parse_entry(raw: &[u8]) -> Option<HistoryEntry> {
    let raw = raw.strip_suffix(b"\n").unwrap_or(raw);
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    serde_json::from_slice(raw).ok()
}
