//! Confined launch ownership: a child and its async metadata broker live together.
use super::broker::Broker;
use super::transport;
use crate::process_group::ProcessGroup;
use crate::pty::Terminal;
use anyhow::{Context, Result};
use cap_std::fs::Dir;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use tokio::task::JoinHandle;

pub struct CommandPlan {
    pub(super) command: Command,
    pub(super) receiver: UnixDatagram,
    pub(super) root: Dir,
    pub(super) scratch: PathBuf,
}

pub struct ConfinedProcess {
    group: ProcessGroup,
    broker: Option<JoinHandle<Result<()>>>,
}

impl CommandPlan {
    /// Configure literal argv and standard streams before launch.
    pub fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Requires a Tokio reactor. The payload cannot complete a brokered operation
    /// until its listener has been received and the broker starts servicing it.
    pub fn spawn(mut self) -> Result<ConfinedProcess> {
        tokio::runtime::Handle::try_current().context("sandbox launch requires a reactor")?;
        let group = ProcessGroup::spawn(&mut self.command)?;
        self.attach(group)
    }

    pub fn spawn_terminal(self, rows: u16, columns: u16) -> Result<(Terminal, ConfinedProcess)> {
        tokio::runtime::Handle::try_current().context("sandbox launch requires a reactor")?;
        let Self {
            command,
            receiver,
            root,
            scratch,
        } = self;
        let (terminal, group) = Terminal::spawn(command, rows, columns)?;
        Ok((terminal, attach(group, receiver, root, scratch)?))
    }

    fn attach(self, group: ProcessGroup) -> Result<ConfinedProcess> {
        attach(group, self.receiver, self.root, self.scratch)
    }
}

fn attach(
    group: ProcessGroup,
    receiver: UnixDatagram,
    root: Dir,
    scratch: PathBuf,
) -> Result<ConfinedProcess> {
    // The trusted pre-exec closure sends before exec; successful spawn therefore
    // guarantees a queued datagram. Nonblocking receive fails closed otherwise.
    let listener = transport::receive(receiver.as_raw_fd()).context("receive sandbox listener")?;
    let broker = Broker::new(listener, root, scratch)?;
    Ok(ConfinedProcess {
        group,
        broker: Some(tokio::spawn(broker.run())),
    })
}

impl ConfinedProcess {
    pub fn child(&self) -> &Child {
        self.group.child()
    }
    pub fn id(&self) -> u32 {
        self.group.id()
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.group.take_stdout()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.group.take_stderr()
    }

    /// Stop owned cgroups before this call when descendants can use job control.
    /// Aborts and joins the broker, surfacing broker errors rather than hiding them.
    pub async fn stop(&mut self) -> Result<ExitStatus> {
        let status = self.group.stop();
        if let Some(broker) = self.broker.take() {
            if !broker.is_finished() {
                broker.abort();
            }
            match broker.await {
                Ok(result) => result.context("sandbox metadata broker")?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error).context("sandbox metadata broker task"),
            }
        }
        Ok(status?)
    }
}

impl Drop for ConfinedProcess {
    fn drop(&mut self) {
        if let Err(error) = self.group.stop() {
            eprintln!("forge: confined process cleanup failed: {error}");
        }
        if let Some(broker) = self.broker.take() {
            broker.abort();
        }
    }
}
