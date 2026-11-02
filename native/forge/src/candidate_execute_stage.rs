use super::*;
use crate::{
    batch_execution::Owners,
    cgroup::Cgroup,
    confined_execution::{self, Cancellation, Execution, Report},
    process_signals::ChildSignalMask,
    sandbox::CommandPlan,
};
use std::{future::Future, sync::Arc};

pub(crate) struct Stage {
    pub workspace: Workspace,
    pub plan: CommandPlan,
    pub limits: Limits,
    pub argv: Vec<String>,
    pub data: Value,
}
struct Launched {
    execution: Execution,
    data: Value,
    argv: Vec<String>,
}
impl Stage {
    pub(crate) fn dry_run(self) -> Result<Value> {
        drop(self.plan);
        self.workspace.discard()?;
        Ok(self.data)
    }
    fn launch(mut self, grants: &Grants, mask: ChildSignalMask) -> Result<Launched> {
        mask.configure(self.plan.command());
        let group = Cgroup::create(&grants.cgroup_parent, grants.resources)?;
        let execution = confined_execution::start_with_resources(
            self.plan,
            group,
            self.limits,
            Box::new(self.workspace),
        )?;
        Ok(Launched {
            execution,
            data: self.data,
            argv: self.argv,
        })
    }
    pub(crate) async fn execute(
        self,
        grants: &Grants,
        interrupts: &Interrupts,
    ) -> Result<(Value, Report, Vec<String>)> {
        let signal = interrupts.register_reactor()?;
        let interrupted = interrupts.interrupted()?;
        let Launched {
            execution,
            mut data,
            argv,
        } = self.launch(grants, interrupts.child_mask())?;
        let wake = if interrupted {
            execution.cancellation().cancel()
        } else {
            Ok(())
        };
        let report = await_report(execution, async {
            let mut bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
            ensure!(
                signal.read(&mut bytes).await? > 0,
                "interrupt descriptor closed"
            );
            Ok(())
        })
        .await?;
        wake.with_context(|| format!("stage cancellation; cleanup: {}", report.errors.join("; ")))?;
        data["report"] = serde_json::to_value(&report)?;
        Ok((data, report, argv))
    }
    pub(crate) async fn execute_batch(
        self,
        grants: &Grants,
        mask: ChildSignalMask,
        global: Arc<Cancellation>,
        owners: Arc<Owners>,
    ) -> Result<(Value, Report, Vec<String>)> {
        ensure!(
            !global.is_cancelled(),
            "batch cancelled before stage launch"
        );
        let Launched {
            execution,
            mut data,
            argv,
        } = self.launch(grants, mask)?;
        // Registration and owner transfer precede any await or worker callback.
        let guard = match owners.register(&execution) {
            Ok(guard) => guard,
            Err(error) => {
                let wake = execution.cancellation().cancel();
                let cleanup = execution.wait().await;
                let report = cleanup?;
                anyhow::bail!(
                    "batch registration: {error:#}; cancellation: {wake:?}; cleanup: {:?}",
                    report.errors
                );
            }
        };
        let result = await_report(execution, global.cancelled()).await;
        let finished = guard.finish();
        let report = result.with_context(|| match &finished {
            Ok(()) => "stage observation failed after cleanup".to_owned(),
            Err(error) => format!("stage observation and owner cleanup failed: {error:#}"),
        })?;
        finished
            .with_context(|| format!("stage resource cleanup: {}", report.errors.join("; ")))?;
        data["report"] = serde_json::to_value(&report)?;
        Ok((data, report, argv))
    }
}
async fn await_report(
    execution: Execution,
    interrupted: impl Future<Output = Result<()>>,
) -> Result<Report> {
    let cancel = execution.cancellation();
    let wait = execution.wait();
    tokio::pin!(wait);
    tokio::select! {
        report = &mut wait => report,
        received = interrupted => {
            let wake = cancel.cancel();
            let report = wait.await?;
            received.with_context(|| format!("stage interrupt observation; cleanup: {}",report.errors.join("; ")))?;
            wake.with_context(|| format!("stage cancellation; cleanup: {}",report.errors.join("; ")))?;
            Ok(report)
        }
    }
}
