//! Durable A2A enqueue shared by explicit queueing and native delivery.

use super::store::{self, Store};
use anyhow::{bail, ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_BODY_BYTES: usize = 1 << 18;
const MAX_DATA_BYTES: usize = 1 << 20;
const QUEUE_REASON: &str = "awaiting explicit flush";

#[derive(Args)]
pub struct EnqueueArgs {
    #[arg(long)]
    from_name: String,
    #[arg(long)]
    to: String,
    /// UTF-8 message body, at most 262144 bytes.
    #[arg(
        long,
        conflicts_with_all = ["body_file", "stdin"],
        required_unless_present_any = ["body_file", "stdin"]
    )]
    body: Option<String>,
    /// Read the UTF-8 body from an existing file, at most 262144 bytes.
    #[arg(long, conflicts_with_all = ["body", "stdin"])]
    body_file: Option<PathBuf>,
    /// Read a bounded UTF-8 body from standard input.
    #[arg(long, conflicts_with_all = ["body", "body_file"])]
    stdin: bool,
    /// Existing JSON object with a supported A2A data kind, at most 1 MiB.
    #[arg(long)]
    data_file: Option<PathBuf>,
    /// Stable caller key: identical sends reuse one durable message ID.
    #[arg(long)]
    idempotency_key: Option<String>,
}

pub use conductor_native::a2a_store::PreparedMessage;

pub(super) struct ValidatedMessage {
    pub sender: String,
    pub recipient: String,
    pub body: String,
    pub data_json: Option<String>,
    pub idempotency_key: Option<String>,
}

fn read_bounded(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let file = File::open(path).with_context(|| format!("opening {label} {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {label} {}", path.display()))?;
    ensure!(
        bytes.len() <= max_bytes,
        "{label} exceeds {max_bytes} bytes"
    );
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8: {}", path.display()))
}

fn body_text(args: &EnqueueArgs) -> Result<String> {
    let body = match (&args.body, &args.body_file, args.stdin) {
        (Some(body), None, false) => body.clone(),
        (None, Some(path), false) => read_bounded(path, MAX_BODY_BYTES, "body")?,
        (None, None, true) => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .take((MAX_BODY_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .context("reading body from standard input")?;
            ensure!(
                bytes.len() <= MAX_BODY_BYTES,
                "body exceeds {MAX_BODY_BYTES} bytes"
            );
            String::from_utf8(bytes).context("standard input body is not UTF-8")?
        }
        _ => bail!("exactly one of --body, --body-file or --stdin is required"),
    };
    ensure!(
        body.len() <= MAX_BODY_BYTES,
        "body exceeds {MAX_BODY_BYTES} bytes"
    );
    Ok(body)
}

fn validate_data(value: &Value) -> Result<()> {
    let object = value
        .as_object()
        .context("--data-file must contain a JSON object")?;
    let kind = object.get("kind").and_then(Value::as_str);
    match kind {
        Some("coordination-v2") => {
            conductor_native::validate_coordination_v2_value(value)
                .map_err(anyhow::Error::msg)?;
        }
        Some("coordination") => {}
        Some("gate-review-request") => validate_gate_request(object)?,
        _ => bail!("unknown data kind {kind:?}; expected coordination, coordination-v2 or gate-review-request"),
    }
    Ok(())
}

fn validate_gate_request(object: &serde_json::Map<String, Value>) -> Result<()> {
    let gate = object.get("gate").and_then(Value::as_f64);
    ensure!(
        gate.is_some_and(
            |gate| gate.fract() == 0.0 && [1.0, 2.0, 3.0, 4.0, 5.0, 7.0].contains(&gate)
        ),
        "gate-review-request requires integer gate in {{1, 2, 3, 4, 5, 7}}"
    );
    let fingerprint = object.get("fingerprint").and_then(Value::as_str);
    ensure!(
        fingerprint.is_some_and(|text| text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
        "gate-review-request requires 64-hex fingerprint"
    );
    ensure!(
        object
            .get("artifact_paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| !paths.is_empty()
                && paths
                    .iter()
                    .all(|path| path.as_str().is_some_and(|path| !path.is_empty()))),
        "gate-review-request requires non-empty artifact_paths"
    );
    Ok(())
}

fn data_json(path: Option<&Path>) -> Result<Option<String>> {
    let Some(path) = path else { return Ok(None) };
    let input = read_bounded(path, MAX_DATA_BYTES, "data payload")?;
    let mut value: Value = serde_json::from_str(&input).context("invalid --data-file JSON")?;
    validate_data(&value)?;
    value.sort_all_objects();
    let encoded = serde_json::to_string(&value)?;
    ensure!(
        encoded.len() <= MAX_DATA_BYTES,
        "serialized data payload exceeds {MAX_DATA_BYTES} bytes"
    );
    Ok(Some(encoded))
}

fn message_id() -> Result<String> {
    let mut random = [0_u8; 16];
    File::open("/dev/urandom")
        .context("opening operating-system random source")?
        .read_exact(&mut random)
        .context("reading operating-system random source")?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let mut rendered = String::with_capacity(36);
    for (index, byte) in random.iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            rendered.push('-');
        }
        use std::fmt::Write as _;
        write!(rendered, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Ok(rendered)
}

pub(super) fn validate_input(args: EnqueueArgs) -> Result<ValidatedMessage> {
    store::validate_identity(&args.from_name)?;
    store::validate_identity(&args.to)?;
    if let Some(key) = &args.idempotency_key {
        ensure!(
            !key.trim().is_empty() && key.len() <= 256,
            "idempotency key must contain 1..256 bytes"
        );
    }
    let body = body_text(&args)?;
    let data_json = data_json(args.data_file.as_deref())?;
    Ok(ValidatedMessage {
        sender: args.from_name,
        recipient: args.to,
        body,
        data_json,
        idempotency_key: args.idempotency_key,
    })
}

pub(super) fn prepare(input: ValidatedMessage) -> Result<PreparedMessage> {
    let id = if let Some(key) = &input.idempotency_key {
        use sha2::{Digest, Sha256};
        let tuple = serde_json::to_vec(&(&input.sender, &input.recipient, key))?;
        format!("key-{:x}", Sha256::digest(tuple))
    } else {
        message_id()?
    };
    let created_at = crate::instant::isoformat_millis_utc(crate::instant::now());
    let row = json!({
        "message_id": id, "direction": "outbound", "sender": input.sender,
        "recipient": input.recipient, "body": input.body, "data_json": input.data_json,
        "created_at": created_at, "received_at": null,
        "delivery_status": "queued", "status_reason": QUEUE_REASON, "read_at": null,
    });
    let metadata = conductor_native::compact_a2a_message(&row)
        .map_err(|error| anyhow::anyhow!("message compaction metadata is invalid: {error}"))?;
    Ok(PreparedMessage {
        id,
        sender: input.sender,
        recipient: input.recipient,
        body: input.body,
        data_json: input.data_json,
        created_at,
        metadata,
    })
}

pub(super) struct SenderLock(File);

impl SenderLock {
    pub(super) fn acquire(state_dir: &Path, sender: &str) -> Result<Self> {
        let path = state_dir.join(sender).join(".delivery.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("opening sender delivery lock {}", path.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        // SAFETY: flock operates on this owned, open descriptor; Drop releases it.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                bail!("sender {sender:?} has an active delivery");
            }
            return Err(error).with_context(|| format!("locking sender {sender:?}"));
        }
        Ok(Self(file))
    }
}

impl Drop for SenderLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor stays open for the lifetime of this lock.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub fn run(args: EnqueueArgs, state_dir: &Path) -> Result<Value> {
    let input = validate_input(args)?;
    store::registered_identity(state_dir, &input.sender)
        .with_context(|| format!("validating sender {:?}", input.sender))?;
    store::registered_identity(state_dir, &input.recipient)
        .with_context(|| format!("validating recipient {:?}", input.recipient))?;
    let mut store = Store::required(state_dir, &input.sender, true)?;
    store.require_enqueue_schema()?;
    let _lock = SenderLock::acquire(state_dir, &input.sender)?;
    // Python flush sorts by (created_at, rowid); stamp while serialized so
    // a later insert cannot overtake an earlier sender under contention.
    let prepared = prepare(input)?;
    store.enqueue(&prepared, QUEUE_REASON)
}
