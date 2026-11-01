use super::*;
use crate::{
    cgroup::Cgroup,
    confined_execution::{self, Report},
    sandbox::CommandPlan,
};

pub(super) struct Stage {
    pub workspace: Workspace,
    pub plan: CommandPlan,
    pub limits: Limits,
    pub argv: Vec<String>,
    pub data: Value,
}
impl Stage {
    pub(super) fn dry_run(self) -> Result<Value> {
        drop(self.plan);
        self.workspace.discard()?;
        Ok(self.data)
    }
    pub(super) async fn execute(
        mut self,
        grants: &Grants,
        interrupts: &Interrupts,
    ) -> Result<(Value, confined_execution::Report, Vec<String>)> {
        let signal = interrupts.register_reactor()?;
        let already_interrupted = interrupts.interrupted()?;
        interrupts.configure_child(self.plan.command());
        let group = Cgroup::create(&grants.cgroup_parent, grants.resources)?;
        let execution = confined_execution::start_with_resources(
            self.plan,
            group,
            self.limits,
            Box::new(self.workspace),
        )?;
        let cancel = execution.cancellation();
        let initial_cancel = if already_interrupted {
            cancel.cancel()
        } else {
            Ok(())
        };
        let wait = execution.wait();
        tokio::pin!(wait);
        let mut signal_bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
        let report: Report = tokio::select! {
            report = &mut wait => report?,
            received = signal.read(&mut signal_bytes) => {
                // Finish the independent resource owner even if signal observation
                // fails. Returning early would shut down this CLI's runtime.
                let cancellation = cancel.cancel();
                let mut report = wait.await?;
                received.context("observe candidate interrupt")?;
                cancellation?;
                if report.outcome == confined_execution::Outcome::Exited {
                    report.outcome = confined_execution::Outcome::Cancelled;
                }
                report
            }
        };
        initial_cancel?;
        self.data["report"] = serde_json::to_value(&report)?;
        Ok((self.data, report, self.argv))
    }
}
