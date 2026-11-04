//! Per-project append-only conversations. Call while holding WorkspaceStore's lock.
//! JSONL is authoritative; the seek index can be rebuilt. Legacy JSON is retained.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

const MAX_MESSAGE: usize = 16 * 1024 * 1024;
const MAX_BATCH: usize = 32 * 1024 * 1024;
const MAX_SUMMARY: usize = 256 * 1024;
const PAGE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE: usize = MAX_MESSAGE + 128;
const HEADER: u64 = 48;
const MAGIC: &[u8; 8] = b"GZMCHAT1";
const SUFFIXES: [&str; 6] = [
    "jsonl",
    "index",
    "state.json",
    "context.json",
    "json",
    "legacy.json",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEntry {
    pub seq: u64,
    pub message: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub through: u64,
    pub summary: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    pub epoch: String,
    pub total: u64,
    pub checkpoint: Option<Checkpoint>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPage {
    pub epoch: String,
    pub total: u64,
    pub entries: Vec<ChatEntry>,
}

#[derive(Serialize, Deserialize)]
struct DiskState {
    epoch: String,
    #[serde(default)]
    resetting: bool,
}

#[derive(Serialize, Deserialize)]
struct Context {
    epoch: String,
    #[serde(flatten)]
    value: Checkpoint,
}

struct Files {
    log: PathBuf,
    index: PathBuf,
    state: PathBuf,
    context: PathBuf,
    legacy: PathBuf,
}

fn checked_directory(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err("Chat directory must be absolute".into());
    }
    let mut current = PathBuf::new();
    for part in dir.components() {
        if matches!(part, Component::CurDir | Component::ParentDir) {
            return Err("Invalid chat directory".into());
        }
        current.push(part.as_os_str());
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        let meta = fs::symlink_metadata(&current).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("Chat directory cannot contain symbolic links".into());
        }
    }
    Ok(())
}

fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(format!(
            "Chat path is not a regular file: {}",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

impl Files {
    fn new(dir: &Path, id: &str) -> Result<Self, String> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err("Invalid project ID".into());
        }
        checked_directory(dir)?;
        for suffix in SUFFIXES {
            exists(&dir.join(format!("{id}.{suffix}")))?;
        }
        Ok(Self {
            log: dir.join(format!("{id}.jsonl")),
            index: dir.join(format!("{id}.index")),
            state: dir.join(format!("{id}.state.json")),
            context: dir.join(format!("{id}.context.json")),
            legacy: dir.join(format!("{id}.json")),
        })
    }
}

fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<(), String>,
) -> Result<(), String> {
    let dir = path.parent().ok_or("Invalid chat path")?;
    checked_directory(dir)?;
    exists(path)?;
    let temporary = dir.join(format!(".chat-{}.tmp", uuid::Uuid::new_v4()));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        created = true;
        write(&mut file)?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        checked_directory(dir)?;
        exists(path)?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        File::open(dir)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    atomic_write(path, |file| {
        serde_json::to_writer(file, value).map_err(|e| e.to_string())
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, max: u64) -> Result<T, String> {
    if !exists(path)? {
        return Err("Chat metadata is missing".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > max {
        return Err("Chat metadata is too large".into());
    }
    serde_json::from_reader(file.take(max + 1)).map_err(|e| format!("Corrupt chat metadata: {e}"))
}

fn message_size(message: &Value) -> Result<usize, String> {
    if !message.is_object()
        || !matches!(
            message.get("role").and_then(Value::as_str),
            Some("user" | "assistant" | "toolResult")
        )
        || !matches!(
            message.get("content"),
            Some(Value::String(_) | Value::Array(_))
        )
    {
        return Err(
            "Chat messages need a user, assistant or toolResult role and string or array content"
                .into(),
        );
    }
    let size = serde_json::to_vec(message)
        .map_err(|e| e.to_string())?
        .len();
    if size > MAX_MESSAGE {
        return Err("Chat message exceeds 16 MiB".into());
    }
    Ok(size)
}

fn decode(line: &[u8], seq: u64) -> Result<ChatEntry, String> {
    let entry: ChatEntry =
        serde_json::from_slice(line).map_err(|e| format!("Corrupt chat record {seq}: {e}"))?;
    if entry.seq != seq {
        return Err(format!(
            "Corrupt chat sequence: expected {seq}, found {}",
            entry.seq
        ));
    }
    message_size(&entry.message)?;
    Ok(entry)
}

fn migrate(files: &Files) -> Result<(), String> {
    // Migration is the only operation that reads the old full array. Never alter
    // the original; the JSONL file/state mark completion, including after reset.
    let messages: Vec<Value> = serde_json::from_reader(BufReader::new(
        File::open(&files.legacy).map_err(|e| e.to_string())?,
    ))
    .map_err(|e| format!("Cannot migrate legacy chat; original preserved: {e}"))?;
    atomic_write(&files.log, |file| {
        let mut writer = BufWriter::new(file);
        for (index, message) in messages.into_iter().enumerate() {
            message_size(&message)?;
            serde_json::to_writer(
                &mut writer,
                &ChatEntry {
                    seq: index as u64 + 1,
                    message,
                },
            )
            .map_err(|e| e.to_string())?;
            writer.write_all(b"\n").map_err(|e| e.to_string())?;
        }
        writer.flush().map_err(|e| e.to_string())
    })
}

fn finish_reset(files: &Files, state: &mut DiskState) -> Result<(), String> {
    atomic_write(&files.log, |_| Ok(()))?;
    for path in [&files.index, &files.context] {
        if exists(path)? {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    state.resetting = false;
    atomic_json(&files.state, state)
}

fn stamp(log: &File, epoch: &str) -> Result<[u8; HEADER as usize], String> {
    let meta = log.metadata().map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let epoch = uuid::Uuid::parse_str(epoch).map_err(|_| "Corrupt chat epoch")?;
    let mut header = [0; HEADER as usize];
    header[..8].copy_from_slice(MAGIC);
    header[8..24].copy_from_slice(epoch.as_bytes());
    header[24..32].copy_from_slice(&meta.len().to_le_bytes());
    header[32..48].copy_from_slice(&modified.to_le_bytes());
    Ok(header)
}

fn read_offset(index: &mut File, entry: u64) -> Result<u64, String> {
    index
        .seek(SeekFrom::Start(HEADER + entry * 8))
        .map_err(|e| e.to_string())?;
    let mut bytes = [0; 8];
    index.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

fn valid_index(index: &mut File, header: &[u8; HEADER as usize], log_len: u64) -> Option<u64> {
    let size = index.metadata().ok()?.len();
    if size < HEADER + 8 || (size - HEADER) % 8 != 0 {
        return None;
    }
    let mut found = [0; HEADER as usize];
    index.read_exact(&mut found).ok()?;
    let total = (size - HEADER) / 8 - 1;
    (found == *header
        && read_offset(index, 0).ok()? == 0
        && read_offset(index, total).ok()? == log_len)
        .then_some(total)
}

fn rebuild_index(files: &Files, log: &mut File, epoch: &str) -> Result<u64, String> {
    let mut total = 0;
    atomic_write(&files.index, |index| {
        let mut writer = BufWriter::new(index);
        writer
            .write_all(&[0; HEADER as usize])
            .map_err(|e| e.to_string())?;
        writer
            .write_all(&0u64.to_le_bytes())
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(File::open(&files.log).map_err(|e| e.to_string())?);
        let mut offset = 0u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            reader
                .by_ref()
                .take(MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if line.is_empty() {
                break;
            }
            if line.len() > MAX_LINE {
                return Err(format!("Corrupt oversized chat record {}", total + 1));
            }
            let terminated = line.last() == Some(&b'\n');
            match decode(&line, total + 1) {
                Ok(_) => {}
                Err(error) if terminated => return Err(error),
                Err(_) => {
                    // Only an unterminated final record may be discarded.
                    log.set_len(offset).map_err(|e| e.to_string())?;
                    break;
                }
            }
            offset += line.len() as u64;
            if !terminated {
                log.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
                log.write_all(b"\n").map_err(|e| e.to_string())?;
                offset += 1;
            }
            total += 1;
            writer
                .write_all(&offset.to_le_bytes())
                .map_err(|e| e.to_string())?;
            if !terminated {
                break;
            }
        }
        log.sync_all().map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        writer
            .write_all(&stamp(log, epoch)?)
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    })?;
    Ok(total)
}

struct Store {
    files: Files,
    log: File,
    index: File,
    epoch: String,
    total: u64,
    checkpoint: Option<Checkpoint>,
}

impl Store {
    fn open(dir: &Path, id: &str) -> Result<Self, String> {
        let files = Files::new(dir, id)?;
        let mut state = if exists(&files.state)? {
            read_json::<DiskState>(&files.state, 1024)?
        } else {
            if !exists(&files.log)? {
                if exists(&files.legacy)? {
                    migrate(&files)?;
                } else {
                    atomic_write(&files.log, |_| Ok(()))?;
                }
            }
            let state = DiskState {
                epoch: uuid::Uuid::new_v4().to_string(),
                resetting: false,
            };
            atomic_json(&files.state, &state)?;
            state
        };
        uuid::Uuid::parse_str(&state.epoch).map_err(|_| "Corrupt chat epoch")?;
        if state.resetting {
            finish_reset(&files, &mut state)?;
        }
        if !exists(&files.log)? {
            return Err("Chat log is missing; preserve metadata for recovery".into());
        }
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&files.log)
            .map_err(|e| e.to_string())?;
        let header = stamp(&log, &state.epoch)?;
        let log_len = log.metadata().map_err(|e| e.to_string())?.len();
        let existing = if exists(&files.index)? {
            let mut index = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&files.index)
                .map_err(|e| e.to_string())?;
            valid_index(&mut index, &header, log_len).map(|total| (index, total))
        } else {
            None
        };
        let (index, total) = match existing {
            Some(value) => value,
            None => {
                let total = rebuild_index(&files, &mut log, &state.epoch)?;
                let index = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&files.index)
                    .map_err(|e| e.to_string())?;
                (index, total)
            }
        };
        let checkpoint = if exists(&files.context)? {
            let context: Context = read_json(&files.context, (MAX_SUMMARY * 6 + 1024) as u64)?;
            if context.epoch == state.epoch {
                validate_summary(&context.value, total)?;
                Some(context.value)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Self {
            files,
            log,
            index,
            epoch: state.epoch,
            total,
            checkpoint,
        })
    }

    fn state(&self) -> ChatState {
        ChatState {
            epoch: self.epoch.clone(),
            total: self.total,
            checkpoint: self.checkpoint.clone(),
        }
    }

    fn check_epoch(&self, epoch: &str) -> Result<(), String> {
        if self.epoch != epoch {
            return Err(
                "Chat epoch conflict: conversation was reset; reload before writing".into(),
            );
        }
        Ok(())
    }

    fn bounds(&mut self, seq: u64) -> Result<(u64, u64), String> {
        if seq == 0 || seq > self.total {
            return Err("Chat sequence is out of range".into());
        }
        let start = read_offset(&mut self.index, seq - 1)?;
        let end = read_offset(&mut self.index, seq)?;
        let len = self.log.metadata().map_err(|e| e.to_string())?.len();
        if start >= end || end > len || end - start > MAX_LINE as u64 {
            return Err("Corrupt chat index; preserve chat files for recovery".into());
        }
        Ok((start, end))
    }

    fn entry(&mut self, seq: u64) -> Result<ChatEntry, String> {
        let (start, end) = self.bounds(seq)?;
        self.log
            .seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; (end - start) as usize];
        self.log.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.last() != Some(&b'\n') {
            return Err(format!("Corrupt chat record {seq}"));
        }
        decode(&bytes, seq)
    }
}

pub fn state(dir: &Path, id: &str) -> Result<ChatState, String> {
    Ok(Store::open(dir, id)?.state())
}

pub fn page(
    dir: &Path,
    id: &str,
    before: Option<u64>,
    after: Option<u64>,
    limit: usize,
) -> Result<ChatPage, String> {
    if before.is_some() && after.is_some() {
        return Err("Use either before or after, not both".into());
    }
    let mut store = Store::open(dir, id)?;
    let limit = limit.clamp(1, 100) as u64;
    let (first, last, forward) = if let Some(after) = after {
        if after >= store.total {
            (1, 0, true)
        } else {
            (
                after + 1,
                store.total.min(after.saturating_add(limit)),
                true,
            )
        }
    } else {
        let last = before
            .map(|seq| seq.saturating_sub(1))
            .unwrap_or(store.total)
            .min(store.total);
        (last.saturating_sub(limit - 1).max(1), last, false)
    };
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    for offset in 0..=last.saturating_sub(first) {
        if first > last {
            break;
        }
        let seq = if forward {
            first + offset
        } else {
            last - offset
        };
        let (start, end) = store.bounds(seq)?;
        if !entries.is_empty() && bytes + end - start > PAGE_BYTES {
            break;
        }
        entries.push(store.entry(seq)?);
        bytes += end - start;
    }
    if !forward {
        entries.reverse();
    }
    Ok(ChatPage {
        epoch: store.epoch,
        total: store.total,
        entries,
    })
}

pub fn append(
    dir: &Path,
    id: &str,
    epoch: &str,
    expected: u64,
    messages: Vec<Value>,
) -> Result<ChatState, String> {
    let mut batch_size = messages.len().saturating_add(1);
    for message in &messages {
        batch_size = batch_size
            .checked_add(message_size(message)?)
            .ok_or("Chat batch exceeds 32 MiB")?;
        if batch_size > MAX_BATCH {
            return Err("Chat batch exceeds 32 MiB".into());
        }
    }
    let mut store = Store::open(dir, id)?;
    store.check_epoch(epoch)?;
    let end = expected
        .checked_add(messages.len() as u64)
        .ok_or("Chat sequence overflow")?;
    if expected != store.total {
        if !messages.is_empty() && expected < store.total && end <= store.total {
            let mut matches = true;
            for (offset, message) in messages.iter().enumerate() {
                if store.entry(expected + offset as u64 + 1)?.message != *message {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Ok(store.state());
            }
        }
        return Err("Chat conflict: history changed; reload before writing".into());
    }
    if messages.is_empty() {
        return Ok(store.state());
    }
    let mut position = store
        .log
        .seek(SeekFrom::End(0))
        .map_err(|e| e.to_string())?;
    store
        .index
        .seek(SeekFrom::End(0))
        .map_err(|e| e.to_string())?;
    {
        let mut log = BufWriter::new(&mut store.log);
        let mut index = BufWriter::new(&mut store.index);
        for (offset, message) in messages.into_iter().enumerate() {
            let entry = ChatEntry {
                seq: expected + offset as u64 + 1,
                message,
            };
            let mut line = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
            line.push(b'\n');
            log.write_all(&line).map_err(|e| e.to_string())?;
            position += line.len() as u64;
            index
                .write_all(&position.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        log.flush().map_err(|e| e.to_string())?;
        index.flush().map_err(|e| e.to_string())?;
    }
    store.log.sync_all().map_err(|e| e.to_string())?;
    store
        .index
        .seek(SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    store
        .index
        .write_all(&stamp(&store.log, &store.epoch)?)
        .map_err(|e| e.to_string())?;
    store.index.sync_all().map_err(|e| e.to_string())?;
    store.total = end;
    Ok(store.state())
}

fn validate_summary(value: &Checkpoint, total: u64) -> Result<(), String> {
    if value.through == 0 || value.through > total {
        return Err("Checkpoint must cover a positive sequence through the current total".into());
    }
    if value.summary.trim().is_empty() || value.summary.len() > MAX_SUMMARY {
        return Err("Checkpoint summary must be nonempty and at most 256 KiB".into());
    }
    Ok(())
}

pub fn checkpoint(
    dir: &Path,
    id: &str,
    epoch: &str,
    value: Checkpoint,
) -> Result<ChatState, String> {
    let mut store = Store::open(dir, id)?;
    store.check_epoch(epoch)?;
    validate_summary(&value, store.total)?;
    if store
        .checkpoint
        .as_ref()
        .is_some_and(|current| value.through < current.through)
    {
        return Err("Checkpoint cannot go backwards".into());
    }
    if value.through < store.total && store.entry(value.through + 1)?.message["role"] != "user" {
        return Err("Checkpoint would split a tool exchange; next message must be user".into());
    }
    atomic_json(
        &store.files.context,
        &Context {
            epoch: store.epoch.clone(),
            value: value.clone(),
        },
    )?;
    store.checkpoint = Some(value);
    Ok(store.state())
}

pub fn reset(dir: &Path, id: &str, epoch: &str) -> Result<ChatState, String> {
    let store = Store::open(dir, id)?;
    store.check_epoch(epoch)?;
    // Persist the new epoch first. Recovery completes an interrupted reset and
    // never imports the retained legacy array into this conversation again.
    atomic_json(
        &store.files.state,
        &DiskState {
            epoch: uuid::Uuid::new_v4().to_string(),
            resetting: true,
        },
    )?;
    drop(store);
    state(dir, id)
}

pub fn remove(dir: &Path, id: &str) -> Result<(), String> {
    Files::new(dir, id)?;
    for suffix in SUFFIXES {
        let path = dir.join(format!("{id}.{suffix}"));
        if exists(&path)? {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("gizmoth-chat-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn file(&self, suffix: &str) -> PathBuf {
            self.0.join(format!("project.{suffix}"))
        }
        fn state(&self) -> ChatState {
            state(&self.0, "project").unwrap()
        }
        fn append(&self, messages: Vec<Value>) -> ChatState {
            let current = self.state();
            append(&self.0, "project", &current.epoch, current.total, messages).unwrap()
        }
        fn page(&self, before: Option<u64>, after: Option<u64>, limit: usize) -> ChatPage {
            page(&self.0, "project", before, after, limit).unwrap()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn user(text: &str) -> Value {
        json!({"role":"user", "content":text})
    }
    fn seqs(page: ChatPage) -> Vec<u64> {
        page.entries.into_iter().map(|entry| entry.seq).collect()
    }

    #[test]
    fn migration_preserves_original_and_reopens_once() {
        let temp = Temp::new();
        let legacy = serde_json::to_vec(&vec![
            user("old"),
            json!({"role":"assistant", "content":[]}),
        ])
        .unwrap();
        fs::write(temp.file("json"), &legacy).unwrap();
        let initial = temp.state();
        assert_eq!(initial.total, 2);
        assert_eq!(fs::read(temp.file("json")).unwrap(), legacy);
        let saved = temp.append(vec![user("new")]);
        let reopened = temp.state();
        assert_eq!(saved.epoch, initial.epoch);
        assert_eq!(reopened.epoch, initial.epoch);
        assert_eq!(reopened.total, 3);
        assert_eq!(temp.page(None, None, 10).entries[0].message, user("old"));
    }

    #[test]
    fn appends_beyond_old_cap_and_observes_page_budget() {
        let temp = Temp::new();
        let large = user(&"x".repeat(3 * 1024 * 1024));
        temp.append(vec![large.clone(), large.clone(), large]);
        assert!(fs::metadata(temp.file("jsonl")).unwrap().len() > 8 * 1024 * 1024);
        assert_eq!(temp.state().total, 3);
        assert_eq!(seqs(temp.page(None, None, 100)), vec![3]);
        assert_eq!(seqs(temp.page(None, Some(0), 100)), vec![1]);
        let unchanged = fs::metadata(temp.file("jsonl")).unwrap().len();
        let current = temp.state();
        assert!(append(
            &temp.0,
            "project",
            &current.epoch,
            3,
            vec![user(&"x".repeat(MAX_MESSAGE))]
        )
        .is_err());
        assert_eq!(fs::metadata(temp.file("jsonl")).unwrap().len(), unchanged);
    }

    #[test]
    fn tail_forward_and_exclusive_cursors() {
        let temp = Temp::new();
        temp.append((1..=110).map(|seq| user(&seq.to_string())).collect());
        assert_eq!(seqs(temp.page(None, None, 3)), vec![108, 109, 110]);
        assert_eq!(seqs(temp.page(Some(108), None, 3)), vec![105, 106, 107]);
        assert_eq!(seqs(temp.page(None, Some(107), 2)), vec![108, 109]);
        assert_eq!(temp.page(None, None, 1000).entries.len(), 100);
        assert_eq!(seqs(temp.page(None, None, 0)), vec![110]);
        assert!(temp.page(Some(0), None, 2).entries.is_empty());
        assert!(temp.page(None, Some(u64::MAX), 2).entries.is_empty());
        assert!(page(&temp.0, "project", Some(1), Some(1), 1).is_err());
    }

    #[test]
    fn retries_compare_requested_range_even_after_newer_appends() {
        let temp = Temp::new();
        let first = temp.append(vec![user("a"), user("b")]);
        temp.append(vec![user("c")]);
        let retry = append(
            &temp.0,
            "project",
            &first.epoch,
            0,
            vec![user("a"), user("b")],
        )
        .unwrap();
        assert_eq!(retry.total, 3);
        for (expected, messages) in [
            (0, vec![user("different")]),
            (2, vec![user("c"), user("d")]),
            (4, vec![user("d")]),
        ] {
            assert!(append(&temp.0, "project", &first.epoch, expected, messages)
                .unwrap_err()
                .contains("conflict"));
        }
        assert_eq!(
            append(&temp.0, "project", &first.epoch, 3, vec![])
                .unwrap()
                .total,
            3
        );
        assert_eq!(temp.state().total, 3);
    }

    #[test]
    fn checkpoint_reopens_and_respects_exchange_boundaries() {
        let temp = Temp::new();
        let current = temp.append(vec![
            user("a"),
            json!({"role":"assistant","content":[]}),
            json!({"role":"toolResult","content":[]}),
            user("b"),
        ]);
        let set = |through, summary: &str| {
            checkpoint(
                &temp.0,
                "project",
                &current.epoch,
                Checkpoint {
                    through,
                    summary: summary.into(),
                },
            )
        };
        assert!(set(1, "partial").is_err());
        assert!(set(2, "partial").is_err());
        assert!(set(0, "invalid").is_err());
        assert!(set(5, "invalid").is_err());
        assert!(set(3, " ").is_err());
        assert!(set(3, &"x".repeat(MAX_SUMMARY + 1)).is_err());
        set(3, "Earlier exchange").unwrap();
        assert_eq!(temp.state().checkpoint.unwrap().summary, "Earlier exchange");
        assert_eq!(temp.state().total, 4);
        set(4, "All exchanges").unwrap();
        assert!(set(3, "Backwards").is_err());
        assert_eq!(temp.state().checkpoint.unwrap().through, 4);
    }

    #[test]
    fn reset_invalidates_writers_and_never_reimports_legacy() {
        let temp = Temp::new();
        fs::write(
            temp.file("json"),
            serde_json::to_vec(&vec![user("legacy")]).unwrap(),
        )
        .unwrap();
        let old = temp.state();
        checkpoint(
            &temp.0,
            "project",
            &old.epoch,
            Checkpoint {
                through: 1,
                summary: "old context".into(),
            },
        )
        .unwrap();
        let cleared = reset(&temp.0, "project", &old.epoch).unwrap();
        assert_ne!(cleared.epoch, old.epoch);
        assert_eq!(cleared.total, 0);
        assert!(cleared.checkpoint.is_none());
        assert_eq!(temp.state().total, 0);
        assert!(temp.file("json").is_file());
        assert!(append(&temp.0, "project", &old.epoch, 0, vec![user("stale")]).is_err());
        assert!(checkpoint(
            &temp.0,
            "project",
            &old.epoch,
            Checkpoint {
                through: 1,
                summary: "stale".into()
            }
        )
        .is_err());
        assert!(reset(&temp.0, "project", &old.epoch).is_err());
        assert_eq!(temp.append(vec![user("fresh")]).total, 1);
    }

    #[test]
    fn recovers_partial_tail_and_rebuilds_missing_or_stale_index() {
        let temp = Temp::new();
        temp.append(vec![user("a"), user("b")]);
        let complete = fs::read(temp.file("jsonl")).unwrap();
        OpenOptions::new()
            .append(true)
            .open(temp.file("jsonl"))
            .unwrap()
            .write_all(b"{\"seq\":3,\"message\":")
            .unwrap();
        assert_eq!(temp.state().total, 2);
        assert_eq!(fs::read(temp.file("jsonl")).unwrap(), complete);
        fs::remove_file(temp.file("index")).unwrap();
        assert_eq!(seqs(temp.page(None, None, 10)), vec![1, 2]);
        fs::write(temp.file("index"), b"unfinished index").unwrap();
        assert_eq!(temp.append(vec![user("c")]).total, 3);
        // A complete JSON value interrupted just before its newline is retained.
        let mut bytes = fs::read(temp.file("jsonl")).unwrap();
        bytes.pop();
        fs::write(temp.file("jsonl"), bytes).unwrap();
        assert_eq!(temp.state().total, 3);
        assert!(fs::read(temp.file("jsonl")).unwrap().ends_with(b"\n"));
    }

    #[test]
    fn rejects_middle_corruption_without_discarding_history() {
        let temp = Temp::new();
        temp.append(vec![user("a"), user("b"), user("c")]);
        let mut bytes = fs::read(temp.file("jsonl")).unwrap();
        let second = bytes.iter().position(|byte| *byte == b'\n').unwrap() + 1;
        bytes[second] = b'!';
        fs::write(temp.file("jsonl"), &bytes).unwrap();
        fs::remove_file(temp.file("index")).unwrap();
        assert!(state(&temp.0, "project").unwrap_err().contains("Corrupt"));
        assert_eq!(fs::read(temp.file("jsonl")).unwrap(), bytes);
    }

    #[test]
    fn interrupted_reset_completes_before_reopening() {
        let temp = Temp::new();
        temp.append(vec![user("discard")]);
        let epoch = uuid::Uuid::new_v4().to_string();
        atomic_json(
            &temp.file("state.json"),
            &DiskState {
                epoch: epoch.clone(),
                resetting: true,
            },
        )
        .unwrap();
        let reopened = temp.state();
        assert_eq!(reopened.epoch, epoch);
        assert_eq!(reopened.total, 0);
    }

    #[test]
    fn remove_only_touches_the_exact_project_files() {
        let temp = Temp::new();
        temp.append(vec![user("remove")]);
        let other = state(&temp.0, "project-other").unwrap();
        append(
            &temp.0,
            "project-other",
            &other.epoch,
            0,
            vec![user("keep")],
        )
        .unwrap();
        fs::write(temp.file("legacy.json"), "[]").unwrap();
        fs::write(temp.file("json"), "[]").unwrap();
        fs::write(temp.0.join("project.notes"), "keep").unwrap();
        remove(&temp.0, "project").unwrap();
        for suffix in SUFFIXES {
            assert!(!temp.file(suffix).exists());
        }
        assert_eq!(state(&temp.0, "project-other").unwrap().total, 1);
        assert_eq!(
            fs::read_to_string(temp.0.join("project.notes")).unwrap(),
            "keep"
        );
        assert!(remove(&temp.0, "../project-other").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_in_every_chat_file_and_parent() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let outside = Temp::new();
        let secret = outside.0.join("secret");
        fs::write(&secret, "untouched").unwrap();
        for suffix in SUFFIXES {
            symlink(&secret, temp.file(suffix)).unwrap();
            assert!(state(&temp.0, "project").is_err(), "{suffix}");
            assert!(remove(&temp.0, "project").is_err(), "{suffix}");
            fs::remove_file(temp.file(suffix)).unwrap();
        }
        let alias = temp.0.join("linked");
        symlink(&outside.0, &alias).unwrap();
        assert!(state(&alias, "project").is_err());
        assert_eq!(fs::read_to_string(secret).unwrap(), "untouched");
    }
}
