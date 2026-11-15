//! Background tasks.
//!
//! Minuet let you fetch mail, read news and browse at the same time. In Alouette every
//! network job is a [`Task`](TaskInfo): it reports progress, can be cancelled or retried,
//! and shows up in the Tasks window. Tasks run on the tokio runtime and never block the UI.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::event::{AppEvent, EventSender};

/// Identifies a task for its whole life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Where a task is in its life.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

impl TaskState {
    pub fn is_finished(&self) -> bool {
        !matches!(self, TaskState::Running)
    }
    pub fn label(&self) -> &'static str {
        match self {
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed(_) => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
}

/// A snapshot of one task, as the Tasks window shows it.
#[derive(Debug, Clone)]
pub struct TaskInfo {
    pub id: TaskId,
    /// Module id that started it, such as `mail` or `demo`.
    pub module: String,
    pub title: String,
    /// A short line about what it is doing right now.
    pub detail: String,
    /// 0.0 to 1.0 when the task knows how far along it is.
    pub progress: Option<f32>,
    pub state: TaskState,
    pub started: Instant,
    pub finished: Option<Instant>,
}

impl TaskInfo {
    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now).duration_since(self.started)
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type Factory = Arc<dyn Fn(TaskCtx) -> BoxFuture + Send + Sync>;

struct Entry {
    info: TaskInfo,
    cancel: CancellationToken,
    factory: Factory,
}

struct Inner {
    next_id: u64,
    entries: VecDeque<Entry>,
}

/// Keeps every task, finished or not, up to a history limit.
#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Mutex<Inner>>,
    events: EventSender,
    history_limit: usize,
}

/// Handed to each running task so it can report progress and notice cancellation.
#[derive(Clone)]
pub struct TaskCtx {
    id: TaskId,
    manager: TaskManager,
    cancel: CancellationToken,
}

impl TaskCtx {
    pub fn id(&self) -> TaskId {
        self.id
    }

    /// Report progress from 0.0 to 1.0. Values outside that range are clamped.
    pub fn progress(&self, fraction: f32) {
        let fraction = fraction.clamp(0.0, 1.0);
        self.manager.update(self.id, |info| info.progress = Some(fraction));
    }

    /// Replace the status line under the task's title.
    pub fn detail(&self, text: impl Into<String>) {
        let text = text.into();
        self.manager.update(self.id, |info| info.detail = text);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when the user cancels this task. Use it in `tokio::select!`.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Send any event to the app, for example a module payload with fetched data.
    pub fn send(&self, event: AppEvent) {
        let _ = self.manager.events.send(event);
    }
}

impl TaskManager {
    /// The app's event sender, for background work that is not a task.
    pub fn events(&self) -> EventSender {
        self.events.clone()
    }

    pub fn new(events: EventSender) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { next_id: 1, entries: VecDeque::new() })), events, history_limit: 200 }
    }

    /// Start a task. `make` builds the future and is kept so the task can be retried.
    ///
    /// Must be called from inside a tokio runtime.
    pub fn spawn<F, Fut>(&self, module: &str, title: impl Into<String>, make: F) -> TaskId
    where
        F: Fn(TaskCtx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let factory: Factory = Arc::new(move |ctx| Box::pin(make(ctx)));
        let id = {
            let mut inner = self.inner.lock().expect("task lock");
            let id = TaskId(inner.next_id);
            inner.next_id += 1;
            inner.entries.push_back(Entry {
                info: TaskInfo {
                    id,
                    module: module.to_string(),
                    title: title.into(),
                    detail: String::new(),
                    progress: None,
                    state: TaskState::Running,
                    started: Instant::now(),
                    finished: None,
                },
                cancel: CancellationToken::new(),
                factory: factory.clone(),
            });
            self.trim(&mut inner);
            id
        };
        self.run(id, factory);
        id
    }

    fn run(&self, id: TaskId, factory: Factory) {
        let Some(cancel) = self.with_entry(id, |e| e.cancel.clone()) else { return };
        let ctx = TaskCtx { id, manager: self.clone(), cancel: cancel.clone() };
        let manager = self.clone();
        let _ = self.events.send(AppEvent::TaskUpdated(id));
        tokio::spawn(async move {
            let fut = factory(ctx);
            let result = tokio::select! {
                r = fut => Some(r),
                _ = cancel.cancelled() => None,
            };
            manager.update(id, |info| {
                info.finished = Some(Instant::now());
                info.state = match result {
                    Some(Ok(())) => {
                        if info.progress.is_some() {
                            info.progress = Some(1.0);
                        }
                        TaskState::Done
                    }
                    Some(Err(e)) => TaskState::Failed(e),
                    None => TaskState::Cancelled,
                };
            });
        });
    }

    /// Ask a running task to stop.
    pub fn cancel(&self, id: TaskId) -> bool {
        self.with_entry(id, |e| {
            if e.info.state == TaskState::Running {
                e.cancel.cancel();
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
    }

    /// Run a finished task again with the same job. Returns false if it is still running.
    pub fn retry(&self, id: TaskId) -> bool {
        let factory = self.with_entry(id, |e| {
            if e.info.state.is_finished() {
                e.info.state = TaskState::Running;
                e.info.progress = None;
                e.info.detail.clear();
                e.info.started = Instant::now();
                e.info.finished = None;
                e.cancel = CancellationToken::new();
                Some(e.factory.clone())
            } else {
                None
            }
        });
        match factory.flatten() {
            Some(f) => {
                self.run(id, f);
                true
            }
            None => false,
        }
    }

    /// Remove finished tasks from the list.
    pub fn clear_finished(&self) {
        let mut inner = self.inner.lock().expect("task lock");
        inner.entries.retain(|e| !e.info.state.is_finished());
    }

    /// A snapshot of every task, oldest first.
    pub fn list(&self) -> Vec<TaskInfo> {
        let inner = self.inner.lock().expect("task lock");
        inner.entries.iter().map(|e| e.info.clone()).collect()
    }

    pub fn get(&self, id: TaskId) -> Option<TaskInfo> {
        self.with_entry(id, |e| e.info.clone())
    }

    /// Number of running tasks.
    pub fn running(&self) -> usize {
        let inner = self.inner.lock().expect("task lock");
        inner.entries.iter().filter(|e| e.info.state == TaskState::Running).count()
    }

    fn update(&self, id: TaskId, f: impl FnOnce(&mut TaskInfo)) {
        if self.with_entry(id, |e| f(&mut e.info)).is_some() {
            let _ = self.events.send(AppEvent::TaskUpdated(id));
        }
    }

    fn with_entry<R>(&self, id: TaskId, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut inner = self.inner.lock().expect("task lock");
        inner.entries.iter_mut().find(|e| e.info.id == id).map(f)
    }

    fn trim(&self, inner: &mut Inner) {
        while inner.entries.len() > self.history_limit {
            // Drop the oldest finished task; never drop running ones.
            match inner.entries.iter().position(|e| e.info.state.is_finished()) {
                Some(pos) => {
                    inner.entries.remove(pos);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event;

    async fn wait_until(m: &TaskManager, id: TaskId, pred: impl Fn(&TaskState) -> bool) -> TaskInfo {
        for _ in 0..200 {
            let info = m.get(id).unwrap();
            if pred(&info.state) {
                return info;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("task {id} never reached the expected state");
    }

    #[tokio::test]
    async fn task_runs_to_done_with_progress() {
        let (tx, mut rx) = event::channel();
        let m = TaskManager::new(tx);
        let id = m.spawn("test", "count", |ctx| async move {
            ctx.progress(0.5);
            ctx.detail("halfway");
            Ok(())
        });
        let info = wait_until(&m, id, |s| *s == TaskState::Done).await;
        assert_eq!(info.progress, Some(1.0));
        assert_eq!(info.detail, "halfway");
        assert!(matches!(rx.recv().await, Some(AppEvent::TaskUpdated(_))));
    }

    #[tokio::test]
    async fn task_failure_is_recorded_and_retry_reruns() {
        let (tx, _rx) = event::channel();
        let m = TaskManager::new(tx);
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let a = attempts.clone();
        let id = m.spawn("test", "flaky", move |_ctx| {
            let a = a.clone();
            async move {
                let n = a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 { Err("first try fails".into()) } else { Ok(()) }
            }
        });
        let info = wait_until(&m, id, |s| s.is_finished()).await;
        assert_eq!(info.state, TaskState::Failed("first try fails".into()));
        assert!(m.retry(id));
        wait_until(&m, id, |s| *s == TaskState::Done).await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancel_stops_a_running_task() {
        let (tx, _rx) = event::channel();
        let m = TaskManager::new(tx);
        let id = m.spawn("test", "forever", |_ctx| async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        });
        assert_eq!(m.running(), 1);
        assert!(m.cancel(id));
        wait_until(&m, id, |s| *s == TaskState::Cancelled).await;
        assert_eq!(m.running(), 0);
        m.clear_finished();
        assert!(m.list().is_empty());
    }
}
