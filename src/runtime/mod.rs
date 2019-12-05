//! The runtime.

use std::thread;

use once_cell::sync::Lazy;

use crate::utils::abort_on_panic;

pub use reactor::{Reactor, Watcher};
pub use scheduler::Scheduler;

mod reactor;
mod scheduler;

/// Global scheduler that runs spawned tasks.
pub static SCHEDULER: Lazy<Scheduler> = Lazy::new(|| {
    thread::Builder::new()
        .name("async-std/scheduler".to_string())
        .spawn(|| abort_on_panic(|| SCHEDULER.run()))
        .expect("cannot start a scheduler thread");

    Scheduler::new()
});
