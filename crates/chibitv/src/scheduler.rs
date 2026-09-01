//! Starting background tasks at the time they are wanted.
//!
//! A scheduled task is an ordinary [`Task`] in the scheduled state from the
//! moment it is accepted, so it is listed, followed and cancelled like any
//! other; the scheduler only decides when its work begins.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tokio::sync::Notify;
use tokio::time::sleep;
use tracing::info;

use crate::task::{Task, TaskHandle, TaskId, TaskKind, Tasks};

type Work = Box<dyn FnOnce(&TaskHandle) -> anyhow::Result<()> + Send>;

pub struct Scheduler {
    tasks: Arc<Tasks>,
    /// The work waiting to be started, in the order it is to start in.
    pending: Mutex<BTreeMap<(DateTime<Local>, TaskId), Work>>,
    /// Woken when what is to start first may have changed.
    changed: Notify,
}

impl Scheduler {
    /// Starts a scheduler that runs for as long as it is held.
    ///
    /// It drives its own timer on the current runtime, so it is created once
    /// the runtime is up.
    pub fn spawn(tasks: Arc<Tasks>) -> Arc<Self> {
        let scheduler = Arc::new(Self {
            tasks,
            pending: Mutex::default(),
            changed: Notify::new(),
        });

        tokio::spawn({
            let scheduler = Arc::clone(&scheduler);
            async move { scheduler.drive().await }
        });

        scheduler
    }

    /// Adds a task that runs `work` at `at`, or as soon as possible when that
    /// is in the past already.
    pub fn schedule<F>(
        &self,
        kind: TaskKind,
        title: impl Into<String>,
        at: DateTime<Local>,
        work: F,
    ) -> Task
    where
        F: FnOnce(&TaskHandle) -> anyhow::Result<()> + Send + 'static,
    {
        let task = self.tasks.schedule(kind, title, at);
        self.pending
            .lock()
            .unwrap()
            .insert((at, task.id), Box::new(work));
        self.changed.notify_one();

        task
    }

    /// Waits for the next task to be due, and starts everything that is.
    async fn drive(self: Arc<Self>) {
        loop {
            let next = self.pending.lock().unwrap().keys().next().copied();

            match next {
                Some((at, _)) => {
                    let delay = (at - Local::now()).to_std().unwrap_or_default();
                    // A task added while this waits may be due earlier, so the
                    // wait ends on that as well as on the time being reached.
                    tokio::select! {
                        _ = sleep(delay) => self.start_due(),
                        _ = self.changed.notified() => {}
                    }
                }
                None => self.changed.notified().await,
            }
        }
    }

    /// Starts the work of every task whose time has come.
    fn start_due(&self) {
        let due = {
            let mut pending = self.pending.lock().unwrap();
            // Everything up to now is due; the identifier of a task is never
            // zero-based enough to matter, as the time alone decides the split.
            let later = pending.split_off(&(Local::now(), TaskId::MIN));

            std::mem::replace(&mut *pending, later)
        };

        for ((at, id), work) in due {
            info!(task_id = id, %at, "Starting a scheduled task");
            // A task cancelled while it waited is simply dropped here.
            self.tasks.start_scheduled(id, work);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::TimeDelta;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use tokio::time::timeout;

    use crate::task::TaskState;

    use super::*;

    /// How long a test waits for work that should start straight away.
    const PATIENCE: Duration = Duration::from_secs(5);

    async fn wait_until(tasks: &Tasks, id: TaskId, state: TaskState) -> Task {
        for _ in 0..500 {
            let task = tasks.get(id).unwrap();
            if task.state == state {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("the task did not reach {state:?}");
    }

    /// Schedules work that reports having started, and nothing else.
    fn schedule_at(scheduler: &Scheduler, at: DateTime<Local>) -> (Task, UnboundedReceiver<()>) {
        let (started_tx, started_rx) = unbounded_channel();
        let task = scheduler.schedule(TaskKind::Record, "Recording", at, move |_| {
            started_tx.send(()).unwrap();
            Ok(())
        });

        (task, started_rx)
    }

    #[tokio::test]
    async fn runs_the_work_once_its_time_has_come() {
        let tasks = Arc::new(Tasks::default());
        let scheduler = Scheduler::spawn(Arc::clone(&tasks));

        let (task, mut started) =
            schedule_at(&scheduler, Local::now() + TimeDelta::milliseconds(100));

        assert_eq!(task.state, TaskState::Scheduled);
        assert!(task.scheduled_at.is_some());
        timeout(PATIENCE, started.recv()).await.unwrap().unwrap();
        wait_until(&tasks, task.id, TaskState::Succeeded).await;
    }

    #[tokio::test]
    async fn runs_work_that_is_due_already() {
        let tasks = Arc::new(Tasks::default());
        let scheduler = Scheduler::spawn(Arc::clone(&tasks));

        let (_, mut started) = schedule_at(&scheduler, Local::now() - TimeDelta::seconds(30));

        timeout(PATIENCE, started.recv()).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn starts_work_that_was_scheduled_after_a_later_one() {
        let tasks = Arc::new(Tasks::default());
        let scheduler = Scheduler::spawn(Arc::clone(&tasks));

        // The scheduler is already waiting for the later task when the earlier
        // one is added, so it has to give up that wait for the new one.
        let (_, mut later) = schedule_at(&scheduler, Local::now() + TimeDelta::seconds(3600));
        tokio::task::yield_now().await;
        let (_, mut earlier) = schedule_at(&scheduler, Local::now() + TimeDelta::milliseconds(100));

        timeout(PATIENCE, earlier.recv()).await.unwrap().unwrap();
        assert!(later.try_recv().is_err());
    }

    #[tokio::test]
    async fn never_runs_work_that_was_cancelled_while_it_waited() {
        let tasks = Arc::new(Tasks::default());
        let scheduler = Scheduler::spawn(Arc::clone(&tasks));

        let (task, mut started) =
            schedule_at(&scheduler, Local::now() + TimeDelta::milliseconds(100));
        let cancelled = tasks.cancel(task.id).unwrap();

        assert_eq!(cancelled.state, TaskState::Cancelled);
        // The work is dropped instead of run once its time comes, which closes
        // the channel it would have reported having started on.
        let started = timeout(PATIENCE, started.recv()).await.unwrap();
        assert!(started.is_none(), "cancelled work was run anyway");
    }
}
