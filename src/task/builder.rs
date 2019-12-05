use std::future::Future;

use kv_log_macro::trace;

use crate::io;
use crate::runtime::SCHEDULER;
use crate::task::{JoinHandle, Task};
use crate::utils::abort_on_panic;

/// Task builder that configures the settings of a new task.
#[derive(Debug, Default)]
pub struct Builder {
    pub(crate) name: Option<String>,
}

impl Builder {
    /// Creates a new builder.
    #[inline]
    pub fn new() -> Builder {
        Builder { name: None }
    }

    /// Configures the name of the task.
    #[inline]
    pub fn name(mut self, name: String) -> Builder {
        self.name = Some(name);
        self
    }

    /// Spawns a task with the configured settings.
    pub fn spawn<F, T>(self, future: F) -> io::Result<JoinHandle<T>>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // Create a new task handle.
        let task = Task::new(self.name);

        // Log this `spawn` operation.
        trace!("spawn", {
            task_id: task.id().0,
            parent_task_id: Task::get_current(|t| t.id().0).unwrap_or(0),
        });

        let future = async move {
            // Drop task-locals on exit.
            defer! {
                Task::get_current(|t| unsafe { t.drop_locals() });
            }

            // Log completion on exit.
            defer! {
                trace!("completed", {
                    task_id: Task::get_current(|t| t.id().0),
                });
            }

            future.await
        };

        let schedule = move |t| SCHEDULER.schedule(Runnable(t));
        let (task, handle) = async_task::spawn(future, schedule, task);
        task.schedule();
        Ok(JoinHandle::new(handle))
    }
}

/// A runnable task.
pub struct Runnable(async_task::Task<Task>);

impl Runnable {
    /// Runs the task by polling its future once.
    pub fn run(self) {
        unsafe {
            Task::set_current(self.0.tag(), || abort_on_panic(|| self.0.run()));
        }
    }
}
