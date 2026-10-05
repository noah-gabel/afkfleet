//! An "MC host thread" (P1.2): a dedicated OS thread with a current-thread
//! tokio runtime and a `LocalSet`, which is what azalea needs. Work arrives
//! over a bounded queue as `Send` closures; each closure builds a (possibly
//! `!Send`) future that is `spawn_local`ed on the host thread, and its result
//! goes back over a oneshot.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use tokio::{
    runtime,
    sync::{mpsc, oneshot},
    task::{self, LocalSet},
};
use tracing::{error, info};

use crate::Res;

type LocalFuture = Pin<Box<dyn Future<Output = ()>>>;
type Job = Box<dyn FnOnce() -> LocalFuture + Send>;

/// Handle to one host thread. Dropping it closes the job queue; the thread
/// then stops once its current jobs are done.
pub struct Host {
    pub name: String,
    jobs: mpsc::Sender<Job>,
    /// Our own count of live `spawn_local` tasks: tokio's `num_alive_tasks`
    /// doesn't see tasks inside a `LocalSet`.
    pub tasks: Arc<AtomicUsize>,
    thread: Option<thread::JoinHandle<()>>,
}

/// Decrements the task counter when a host task ends, even by panic.
struct TaskGuard(Arc<AtomicUsize>);
impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Host {
    pub fn spawn(name: &str) -> Res<Host> {
        let (jobs, mut rx) = mpsc::channel::<Job>(64);
        let tasks = Arc::new(AtomicUsize::new(0));
        let thread_tasks = tasks.clone();
        let thread_name = name.to_owned();
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let rt = match runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        error!(host = %thread_name, error = %e, "runtime build failed");
                        return;
                    }
                };
                LocalSet::new().block_on(&rt, async move {
                    while let Some(job) = rx.recv().await {
                        thread_tasks.fetch_add(1, Ordering::SeqCst);
                        let guard = TaskGuard(thread_tasks.clone());
                        task::spawn_local(async move {
                            let _guard = guard;
                            job().await;
                        });
                    }
                });
                info!(host = %thread_name, "host thread stopped");
            })?;
        Ok(Host {
            name: name.to_owned(),
            jobs,
            tasks,
            thread: Some(thread),
        })
    }

    /// Runs `f()` on the host thread and returns its output.
    pub async fn run<F, Fut, T>(&self, f: F) -> Res<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move || {
            Box::pin(async move {
                let _ = tx.send(f().await);
            })
        });
        self.jobs
            .try_send(job)
            .map_err(|_| format!("{}: job queue full or closed", self.name))?;
        Ok(rx
            .await
            .map_err(|_| format!("{}: job dropped (panicked?)", self.name))?)
    }

    /// Whether the OS thread has ended.
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }
}
