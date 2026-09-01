//! Long running work the server performs in the background.
//!
//! A task is started by an RPC and outlives the call that asked for it: the
//! caller is handed a snapshot of it and follows the rest through
//! [`Tasks::subscribe`]. Work that a client should be able to stop while it
//! runs — crawling the programme guide today, recording later on — belongs
//! here rather than in the handler of the call.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tokio::sync::broadcast::{Receiver, Sender, channel as broadcast_channel};
use tracing::{error, info};

pub type TaskId = u64;

/// How many updates a subscriber may fall behind before it misses one.
const UPDATE_CAPACITY: usize = 64;

/// How many finished tasks are kept around for clients to look at.
const FINISHED_HISTORY: usize = 32;

/// The work a task performs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskKind {
    /// Collects the programme guide from every configured channel.
    RefreshEvents,
}

impl TaskKind {
    /// Whether a task of this kind stops when it is asked to.
    fn is_cancellable(self) -> bool {
        match self {
            // The guide is refreshed whenever it suits the viewer, and giving
            // up halfway only leaves the events already collected in place.
            Self::RefreshEvents => true,
        }
    }

    /// Whether at most one task of this kind may be running at a time.
    fn is_exclusive(self) -> bool {
        match self {
            // Crawling takes a tuner and walks every channel with it, so a
            // second crawl would only fight the first one over the tuner.
            Self::RefreshEvents => true,
        }
    }
}

/// Where a task is in its lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    /// Accepted, but not started yet.
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// A snapshot of one background task.
#[derive(Clone, Debug)]
pub struct Task {
    pub id: TaskId,
    pub kind: TaskKind,
    pub state: TaskState,
    /// What the task does, as shown to the viewer.
    pub title: String,
    /// What the task is doing right now, empty until it reports something.
    pub message: String,
    /// How much of the work is done, between 0 and 1, when the task can tell.
    pub progress: Option<f32>,
    pub cancellable: bool,
    /// Why the task failed, set in the failed state only.
    pub error: Option<String>,
    pub created_at: DateTime<Local>,
    pub started_at: Option<DateTime<Local>>,
    pub finished_at: Option<DateTime<Local>>,
}

#[derive(Debug)]
pub enum SpawnError {
    /// A task of the same kind is already running, and the kind allows only one.
    AlreadyRunning,
}

#[derive(Debug)]
pub enum CancelError {
    NotFound,
    /// The task runs to its end once started.
    NotCancellable,
}

/// The flag a task watches to learn that it should stop.
#[derive(Clone, Default)]
struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// The side of a task the work itself sees: it reports what it is doing
/// through this, and asks whether it should stop.
pub struct TaskHandle {
    id: TaskId,
    tasks: Arc<Tasks>,
    cancellation: Cancellation,
}

impl TaskHandle {
    /// Whether the task has been asked to stop.
    ///
    /// Cancellation is cooperative: work is expected to check this often
    /// enough that stopping feels immediate, and to leave what it has already
    /// done in place.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Reports how far the work has got, and what it is doing right now.
    pub fn report(&self, progress: Option<f32>, message: impl Into<String>) {
        let message = message.into();
        self.tasks.update(self.id, |task| {
            task.progress = progress;
            task.message = message;
        });
    }
}

struct Entry {
    task: Task,
    cancellation: Cancellation,
}

#[derive(Default)]
struct State {
    next_id: TaskId,
    entries: BTreeMap<TaskId, Entry>,
}

/// Every background task of the server, running and recently finished alike.
pub struct Tasks {
    state: Mutex<State>,
    updates: Sender<Task>,
}

impl Default for Tasks {
    fn default() -> Self {
        let (updates, _) = broadcast_channel(UPDATE_CAPACITY);

        Self {
            state: Mutex::default(),
            updates,
        }
    }
}

impl Tasks {
    /// Starts `run` on a thread of its own as a new task.
    ///
    /// The work is given a [`TaskHandle`] to report through, and its return
    /// value decides how the task finishes. Blocking work is what a task is
    /// for — the pipelines it drives read from a tuner — so it is run on a
    /// thread rather than on the async runtime.
    pub fn spawn_blocking<F>(
        self: &Arc<Self>,
        kind: TaskKind,
        title: impl Into<String>,
        run: F,
    ) -> Result<Task, SpawnError>
    where
        F: FnOnce(&TaskHandle) -> anyhow::Result<()> + Send + 'static,
    {
        let handle = {
            let mut state = self.state.lock().unwrap();
            if kind.is_exclusive()
                && state
                    .entries
                    .values()
                    .any(|entry| entry.task.kind == kind && !entry.task.state.is_finished())
            {
                return Err(SpawnError::AlreadyRunning);
            }

            let id = state.next_id;
            state.next_id += 1;

            let cancellation = Cancellation::default();
            state.entries.insert(
                id,
                Entry {
                    task: Task {
                        id,
                        kind,
                        state: TaskState::Pending,
                        title: title.into(),
                        message: String::new(),
                        progress: None,
                        cancellable: kind.is_cancellable(),
                        error: None,
                        created_at: Local::now(),
                        started_at: None,
                        finished_at: None,
                    },
                    cancellation: cancellation.clone(),
                },
            );

            TaskHandle {
                id,
                tasks: Arc::clone(self),
                cancellation,
            }
        };

        let task = self
            .broadcast(handle.id)
            .expect("the task was just created");
        info!(task_id = task.id, kind = ?task.kind, "Task queued");

        let tasks = Arc::clone(self);
        std::thread::spawn(move || {
            tasks.update(handle.id, |task| {
                task.state = TaskState::Running;
                task.started_at = Some(Local::now());
            });

            let result = run(&handle);
            tasks.finish(handle.id, result, handle.is_cancelled());
        });

        Ok(task)
    }

    /// Every task, oldest first.
    pub fn list(&self) -> Vec<Task> {
        let state = self.state.lock().unwrap();
        state
            .entries
            .values()
            .map(|entry| entry.task.clone())
            .collect()
    }

    pub fn get(&self, id: TaskId) -> Option<Task> {
        let state = self.state.lock().unwrap();
        state.entries.get(&id).map(|entry| entry.task.clone())
    }

    /// Follows every change to any task from now on.
    pub fn subscribe(&self) -> Receiver<Task> {
        self.updates.subscribe()
    }

    /// Asks a task to stop, which it may take a moment to act on.
    ///
    /// Cancelling a task that has already finished changes nothing and
    /// returns it as it is, so that a client racing the end of a task does not
    /// have to tell the two apart.
    pub fn cancel(&self, id: TaskId) -> Result<Task, CancelError> {
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&id).ok_or(CancelError::NotFound)?;
        if entry.task.state.is_finished() {
            return Ok(entry.task.clone());
        }
        if !entry.task.cancellable {
            return Err(CancelError::NotCancellable);
        }

        entry.cancellation.cancel();
        info!(task_id = id, "Task cancellation requested");

        Ok(entry.task.clone())
    }

    fn update(&self, id: TaskId, change: impl FnOnce(&mut Task)) {
        {
            let mut state = self.state.lock().unwrap();
            let Some(entry) = state.entries.get_mut(&id) else {
                return;
            };
            change(&mut entry.task);
        }

        self.broadcast(id);
    }

    fn finish(&self, id: TaskId, result: anyhow::Result<()>, is_cancelled: bool) {
        let state = match (&result, is_cancelled) {
            (Ok(_), false) => TaskState::Succeeded,
            // Work that gave up because it was asked to may well report an
            // error on the way out, so cancellation is what it is reported as.
            (_, true) => TaskState::Cancelled,
            (Err(error), false) => {
                error!(task_id = id, %error, "Task failed");
                TaskState::Failed
            }
        };

        // The error explains a failure, so work that gave up on being asked to
        // stop is reported as cancelled and nothing else.
        let error = match state {
            TaskState::Failed => result.err().map(|error| format!("{error:#}")),
            _ => None,
        };

        self.update(id, |task| {
            task.state = state;
            task.error = error;
            task.progress = match state {
                TaskState::Succeeded => Some(1.0),
                _ => task.progress,
            };
            task.finished_at = Some(Local::now());
        });

        info!(task_id = id, ?state, "Task finished");
        self.prune();
    }

    /// Publishes the task as it stands, and returns that snapshot.
    fn broadcast(&self, id: TaskId) -> Option<Task> {
        let task = self.get(id)?;
        // Nobody watching is the common case, and no client is required for a
        // task to run, so a failed send is not an error.
        let _ = self.updates.send(task.clone());

        Some(task)
    }

    /// Forgets the oldest of the finished tasks, so that a server left running
    /// does not keep every task it has ever run.
    fn prune(&self) {
        let mut state = self.state.lock().unwrap();
        let finished = state
            .entries
            .values()
            .filter(|entry| entry.task.state.is_finished())
            .count();

        for id in state
            .entries
            .iter()
            .filter(|(_, entry)| entry.task.state.is_finished())
            .map(|(id, _)| *id)
            .take(finished.saturating_sub(FINISHED_HISTORY))
            .collect::<Vec<_>>()
        {
            state.entries.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender, channel};
    use std::time::Duration;

    use super::*;

    /// Runs a task that blocks until it is told to return, so that it can be
    /// looked at while it is running.
    fn spawn_blocked(
        tasks: &Arc<Tasks>,
    ) -> (Task, StdSender<anyhow::Result<()>>, StdReceiver<bool>) {
        let (result_tx, result_rx) = channel::<anyhow::Result<()>>();
        let (cancelled_tx, cancelled_rx) = channel();
        let task = tasks
            .spawn_blocking(TaskKind::RefreshEvents, "Refreshing", move |handle| {
                let result = result_rx.recv().unwrap();
                cancelled_tx.send(handle.is_cancelled()).unwrap();
                result
            })
            .unwrap();

        (task, result_tx, cancelled_rx)
    }

    fn wait_until(tasks: &Tasks, id: TaskId, state: TaskState) -> Task {
        for _ in 0..500 {
            let task = tasks.get(id).unwrap();
            if task.state == state {
                return task;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        panic!("the task did not reach {state:?}");
    }

    #[test]
    fn reports_a_task_until_it_succeeds() {
        let tasks = Arc::new(Tasks::default());

        let (task, result_tx, _cancelled) = spawn_blocked(&tasks);
        assert!(task.cancellable);
        let running = wait_until(&tasks, task.id, TaskState::Running);
        assert!(running.started_at.is_some());

        result_tx.send(Ok(())).unwrap();

        let finished = wait_until(&tasks, task.id, TaskState::Succeeded);
        assert_eq!(finished.progress, Some(1.0));
        assert!(finished.error.is_none());
        assert!(finished.finished_at.is_some());
    }

    #[test]
    fn keeps_the_error_of_a_failed_task() {
        let tasks = Arc::new(Tasks::default());

        let (task, result_tx, _cancelled) = spawn_blocked(&tasks);
        wait_until(&tasks, task.id, TaskState::Running);
        result_tx.send(Err(anyhow::anyhow!("no tuner"))).unwrap();

        let finished = wait_until(&tasks, task.id, TaskState::Failed);
        assert_eq!(finished.error.as_deref(), Some("no tuner"));
    }

    #[test]
    fn cancelling_a_task_tells_the_work_to_stop() {
        let tasks = Arc::new(Tasks::default());

        let (task, result_tx, cancelled) = spawn_blocked(&tasks);
        wait_until(&tasks, task.id, TaskState::Running);
        tasks.cancel(task.id).unwrap();
        result_tx.send(Ok(())).unwrap();

        assert!(cancelled.recv().unwrap());
        let finished = wait_until(&tasks, task.id, TaskState::Cancelled);
        assert!(finished.error.is_none());
    }

    #[test]
    fn cancelling_a_finished_task_leaves_it_as_it_is() {
        let tasks = Arc::new(Tasks::default());

        let (task, result_tx, _cancelled) = spawn_blocked(&tasks);
        wait_until(&tasks, task.id, TaskState::Running);
        result_tx.send(Ok(())).unwrap();
        wait_until(&tasks, task.id, TaskState::Succeeded);

        let cancelled = tasks.cancel(task.id).unwrap();

        assert_eq!(cancelled.state, TaskState::Succeeded);
    }

    #[test]
    fn cancelling_an_unknown_task_fails() {
        let tasks = Arc::new(Tasks::default());

        assert!(matches!(tasks.cancel(42), Err(CancelError::NotFound)));
    }

    #[test]
    fn refuses_a_second_task_of_an_exclusive_kind() {
        let tasks = Arc::new(Tasks::default());

        let (task, result_tx, _cancelled) = spawn_blocked(&tasks);
        wait_until(&tasks, task.id, TaskState::Running);

        let result = tasks.spawn_blocking(TaskKind::RefreshEvents, "Refreshing", |_| Ok(()));
        assert!(matches!(result, Err(SpawnError::AlreadyRunning)));

        // Once the first one is out of the way, the next one is accepted.
        result_tx.send(Ok(())).unwrap();
        wait_until(&tasks, task.id, TaskState::Succeeded);
        assert!(
            tasks
                .spawn_blocking(TaskKind::RefreshEvents, "Refreshing", |_| Ok(()))
                .is_ok()
        );
    }

    #[test]
    fn publishes_every_change_to_subscribers() {
        let tasks = Arc::new(Tasks::default());
        let mut updates = tasks.subscribe();

        let task = tasks
            .spawn_blocking(TaskKind::RefreshEvents, "Refreshing", |handle| {
                handle.report(Some(0.5), "Crawling UHF 20");
                Ok(())
            })
            .unwrap();

        let states = std::iter::from_fn(|| updates.blocking_recv().ok())
            .take_while(|task| !task.state.is_finished())
            .map(|update| (update.state, update.progress, update.message))
            .collect::<Vec<_>>();

        assert!(states.contains(&(TaskState::Running, Some(0.5), "Crawling UHF 20".to_string())));
        assert_eq!(
            wait_until(&tasks, task.id, TaskState::Succeeded).id,
            task.id
        );
    }

    #[test]
    fn forgets_the_oldest_finished_tasks() {
        let tasks = Arc::new(Tasks::default());

        for _ in 0..FINISHED_HISTORY + 8 {
            let task = tasks
                .spawn_blocking(TaskKind::RefreshEvents, "Refreshing", |_| Ok(()))
                .unwrap();
            wait_until(&tasks, task.id, TaskState::Succeeded);
        }

        assert_eq!(tasks.list().len(), FINISHED_HISTORY);
    }
}
