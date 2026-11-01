//! Finite presentation stream. Emission precedes presentation acknowledgment;
//! a crash between them can replay a batch, but never silently consumes it.
use super::{events, startup, store::Store, view};
use anyhow::{ensure, Context, Result};
use clap::Args;
use forge::process_io::StandardStream;
use forge::process_probe::Interrupted;
use forge::process_signals::Interrupts;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct WatchArgs {
    #[arg(long)]
    as_name: String,
    /// Present at most one batch and exit immediately, including on an empty inbox.
    #[arg(long)]
    once: bool,
    /// Finite foreground lifetime; no service or model starts.
    #[arg(long, default_value_t = 300, value_parser=clap::value_parser!(u64).range(1..=3600))]
    duration: u64,
    /// Reconciliation seconds (5..3600); notifications wake the normal view sooner.
    #[arg(long, default_value_t = 120.0)]
    interval: f64,
    /// Re-present unread messages on each reconciliation; use read to acknowledge them.
    #[arg(long)]
    replay_unread: bool,
    #[arg(long, default_value_t = 8)]
    max_messages: usize,
    #[arg(long, default_value_t = 140)]
    preview_chars: usize,
    #[arg(long, default_value_t = 1200)]
    max_chars: usize,
    #[arg(long)]
    json: bool,
}

impl WatchArgs {
    fn validate(&self) -> Result<()> {
        super::store::validate_identity(&self.as_name)?;
        ensure!(
            (5.0..=3600.0).contains(&self.interval),
            "interval must be 5..3600 seconds"
        );
        ensure!(
            (1..=8).contains(&self.max_messages),
            "max_messages must be 1..8"
        );
        ensure!(
            (32..=320).contains(&self.preview_chars),
            "preview_chars must be 32..320"
        );
        ensure!(
            (256..=10000).contains(&self.max_chars),
            "max_chars must be 256..10000"
        );
        Ok(())
    }
}

fn emit(
    output: &StandardStream,
    bytes: &[u8],
    deadline: Instant,
    signals: &Interrupts,
) -> Result<()> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(3));
    let mut remaining = bytes;
    while !remaining.is_empty() {
        if signals.interrupted()? {
            return Err(Interrupted.into());
        }
        let time = deadline
            .checked_duration_since(Instant::now())
            .context("mailbox stdout backpressure deadline exceeded; batch remains unpresented")?;
        let mut descriptors = [output.as_raw_fd(), signals.as_raw_fd()].map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
        descriptors[0].events = libc::POLLOUT;
        // SAFETY: poll borrows two initialized descriptors for this call.
        if unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                2,
                time.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32,
            )
        } < 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("wait for mailbox stdout");
        }
        if signals.interrupted()? {
            return Err(Interrupted.into());
        }
        ensure!(
            descriptors[0].revents & libc::POLLNVAL == 0,
            "mailbox stdout became invalid"
        );
        if descriptors[0].revents == 0 {
            continue;
        }
        // SAFETY: the live nonblocking descriptor and borrowed byte slice are valid.
        let count = unsafe {
            libc::write(
                output.as_raw_fd(),
                remaining.as_ptr().cast(),
                remaining.len(),
            )
        };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(
                error.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error).context("write mailbox presentation; batch remains unpresented");
        }
        ensure!(count > 0, "mailbox stdout accepted zero bytes");
        remaining = &remaining[count as usize..];
    }
    Ok(())
}

fn present(
    args: &WatchArgs,
    store: &mut Store,
    output: &StandardStream,
    deadline: Instant,
    signals: &Interrupts,
) -> Result<bool> {
    let payload = view::presentation(
        store,
        &args.as_name,
        args.replay_unread,
        args.max_messages,
        args.preview_chars,
        args.max_chars,
    )?;
    if payload["total"] == 0 {
        return Ok(false);
    }
    let messages = payload["messages"]
        .as_array()
        .context("presentation messages missing")?;
    ensure!(
        !messages.is_empty(),
        "no message fits max_chars; increase the presentation budget"
    );
    let mut text = if args.json {
        view::compact_json(&payload)?
    } else {
        view::render_inbox(&payload)?
    };
    ensure!(
        text.chars().count() <= args.max_chars,
        "rendered presentation exceeds max_chars"
    );
    let ids = messages
        .iter()
        .map(|row| {
            row["id"]
                .as_str()
                .map(str::to_owned)
                .context("presentation message id missing")
        })
        .collect::<Result<Vec<_>>>()?;
    text.push('\n');
    emit(output, text.as_bytes(), deadline, signals)?;
    store.mark_presented(&ids)?;
    Ok(true)
}

pub fn run(args: WatchArgs, root: &Path) -> Result<u8> {
    args.validate()?;
    let root = root.canonicalize()?;
    let mut store = Store::required(&root, &args.as_name, true)?;
    store.require_enqueue_schema()?;
    let startup_guard = startup::startup_lock(&root, &args.as_name)?;
    let directory = root.join(&args.as_name);
    let _lease = forge::file_lock::Exclusive::try_new(
        startup::owned_file(&directory.join(".watch.lock"))?,
        "mailbox presentation",
    )?;
    let mut watch = events::Watch::new(&directory)?;
    drop(startup_guard);
    let signals = Interrupts::install()?;
    let output = StandardStream::new(libc::STDOUT_FILENO)?;
    let deadline = Instant::now() + Duration::from_secs(args.duration);
    loop {
        if signals.interrupted()? {
            return Ok(130);
        }
        if Instant::now() >= deadline {
            return Ok(0);
        }
        watch.drain()?;
        let emitted = match present(&args, &mut store, &output, deadline, &signals) {
            Ok(emitted) => emitted,
            Err(error) if error.is::<Interrupted>() => return Ok(130),
            Err(error) => return Err(error),
        };
        if args.once {
            return Ok(0);
        }
        if emitted && !args.replay_unread {
            continue;
        }
        // Explicit replay is periodic. Drain our presentation writes so they do
        // not immediately re-present the same unread batch in a tight loop.
        if args.replay_unread {
            watch.drain()?;
        }
        let until = deadline.min(Instant::now() + Duration::from_secs_f64(args.interval));
        if matches!(
            watch.wait(until, &signals, None)?,
            events::Event::Interrupted
        ) {
            return Ok(130);
        }
    }
}
