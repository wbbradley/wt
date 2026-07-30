use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::app::StatusUpdate;
use crate::git::{self, SystemGit};
use crate::model::WorktreeStatus;

#[derive(Clone, Debug)]
pub struct StatusTask {
    pub generation: u64,
    pub path: PathBuf,
}

type StatusLoader = dyn Fn(&Path) -> Result<WorktreeStatus, String> + Send + Sync + 'static;

pub struct StatusPool {
    sender: Option<SyncSender<StatusTask>>,
    receiver: Receiver<StatusUpdate>,
    workers: Vec<JoinHandle<()>>,
}

impl StatusPool {
    pub fn with_git(worker_count: usize) -> Self {
        Self::new(
            worker_count,
            Arc::new(|path| git::status(&SystemGit, path).map_err(|error| error.to_string())),
        )
    }

    pub fn new(worker_count: usize, loader: Arc<StatusLoader>) -> Self {
        let worker_count = worker_count.clamp(1, 8);
        let (task_sender, task_receiver) = mpsc::sync_channel::<StatusTask>(worker_count * 2);
        let (result_sender, result_receiver) = mpsc::channel();
        let shared_receiver = Arc::new(Mutex::new(task_receiver));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let receiver = Arc::clone(&shared_receiver);
            let sender = result_sender.clone();
            let loader = Arc::clone(&loader);
            workers.push(
                thread::Builder::new()
                    .name(format!("wt-status-worker-{index}"))
                    .spawn(move || {
                        loop {
                            let task = {
                                let receiver = receiver.lock().expect("status queue lock poisoned");
                                receiver.recv()
                            };
                            let Ok(task) = task else {
                                break;
                            };
                            let result = loader(&task.path);
                            if sender
                                .send(StatusUpdate {
                                    generation: task.generation,
                                    path: task.path,
                                    result,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .expect("failed to spawn status worker"),
            );
        }
        drop(result_sender);
        Self {
            sender: Some(task_sender),
            receiver: result_receiver,
            workers,
        }
    }

    pub fn try_submit(&self, task: StatusTask) -> Result<(), StatusTask> {
        match self
            .sender
            .as_ref()
            .expect("status pool is active")
            .try_send(task)
        {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(task) | TrySendError::Disconnected(task)) => Err(task),
        }
    }

    pub fn try_recv(&self) -> Option<StatusUpdate> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for StatusPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn worker_pool_is_bounded_and_preserves_generation_ids() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let loader = {
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            Arc::new(move |_path: &Path| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(WorktreeStatus::default())
            })
        };
        let pool = StatusPool::new(2, loader);
        let mut pending: Vec<StatusTask> = (0..8)
            .map(|index| StatusTask {
                generation: 42,
                path: PathBuf::from(format!("/{index}")),
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut results = Vec::new();
        while results.len() < 8 && Instant::now() < deadline {
            let mut remaining = Vec::new();
            for task in pending.drain(..) {
                if let Err(task) = pool.try_submit(task) {
                    remaining.push(task);
                }
            }
            pending = remaining;
            while let Some(result) = pool.try_recv() {
                results.push(result);
            }
            thread::yield_now();
        }
        assert_eq!(results.len(), 8);
        assert!(results.iter().all(|result| result.generation == 42));
        assert!(maximum.load(Ordering::SeqCst) <= 2);
    }
}
