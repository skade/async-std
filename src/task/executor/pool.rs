use std::cell::Cell;
use std::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_queue::ArrayQueue;
use once_cell::sync::Lazy;
use once_cell::unsync::OnceCell;

use crate::task::Runnable;
use crate::utils::{abort_on_panic, random, Spinlock};

/// The state of an executor.
struct Pool {
    /// The global queue of tasks.
    injector: Injector<Runnable>,

    /// Handles to local queues for stealing work from worker threads.
    stealers: Vec<Stealer<Runnable>>,

    num_procs: usize,
    procs: ArrayQueue<Processor>,
    sleeps: AtomicU64,
    deep_sleep: AtomicBool,
}

/// Global executor that runs spawned tasks.
static POOL: Lazy<Pool> = Lazy::new(|| {
    let num_procs = num_cpus::get().max(1);
    let mut stealers = Vec::new();
    let procs = ArrayQueue::new(num_procs);

    // Spawn worker threads.
    for _ in 0..num_procs {
        let worker = Worker::new_fifo();
        stealers.push(worker.stealer());

        let proc = Processor {
            tick: AtomicU64::new(0),
            worker,
            slot: Cell::new(None),
        };
        procs.push(proc).unwrap();
    }

    thread::Builder::new()
        .name("async-std/sysmon".to_string())
        .spawn(|| abort_on_panic(sysmon))
        .expect("cannot start a sysmon thread");

    Pool {
        injector: Injector::new(),
        stealers,
        num_procs,
        procs,
        sleeps: AtomicU64::new(0),
        deep_sleep: AtomicBool::new(false),
    }
});

/// The state of a worker thread.
struct Processor {
    tick: AtomicU64,

    /// The local task queue.
    worker: Worker<Runnable>,

    /// Contains the next task to run as an optimization that skips queues.
    slot: Cell<Option<Runnable>>,
}

thread_local! {
    /// Worker thread state.
    static PROCESSOR: OnceCell<Arc<Spinlock<Option<Processor>>>> = OnceCell::new();
}

/// Schedules a new runnable task for execution.
pub(crate) fn schedule(task: Runnable) {
    let mut notify = false;

    PROCESSOR.with(|proc| {
        // If the current thread is a worker thread, store it into its task slot or push it into
        // its local task queue. Otherwise, push it into the global task queue.
        match proc.get() {
            Some(proc) => {
                if let Some(proc) = proc.lock().as_ref() {
                    // Replace the task in the slot.
                    if let Some(task) = proc.slot.replace(Some(task)) {
                        // If the slot already contained a task, push it into the local task queue.
                        proc.worker.push(task);
                        notify = true;
                    }
                }
                return;
            }
            None => {}
        }

        POOL.injector.push(task);
        notify = true;
    });

    // TODO: if there is stealable work (in proc.worker or injector), notify mio
    if notify && POOL.deep_sleep.load(Ordering::SeqCst) {
        atomic::fence(Ordering::SeqCst);
        crate::net::driver::notify();
    }
}

// TODO: rename sysmon to scheduler?
fn sysmon() {
    let mut sleeps = 0;
    loop {
        if POOL.procs.len() == POOL.num_procs {
            start_worker(POOL.procs.pop().unwrap());
        }

        // XXX: check if worker threads can keep up, if not, spawn another one
        thread::sleep(Duration::from_millis(10));

        let s = POOL.sleeps.load(Ordering::SeqCst);
        if sleeps != s {
            sleeps = s;
        } else {
            if !POOL.deep_sleep.load(Ordering::SeqCst) {
                if let Ok(proc) = POOL.procs.pop() {
                    start_worker(proc);
                }
            }
        }

        crate::net::driver::poll_quick();
    }
}

fn start_worker(proc: Processor) {
    // println!("START");
    thread::Builder::new()
        .name("async-std/worker".to_string())
        .spawn(|| {
            let _ = PROCESSOR.with(|p| p.set(Arc::new(Spinlock::new(Some(proc)))));
            abort_on_panic(main_loop);
        })
        .expect("cannot start a worker thread");
}

fn stop_worker() {
    // println!("STOP");

    PROCESSOR.with(|proc| {
        POOL.procs
            .push(proc.get().unwrap().lock().take().unwrap())
            .unwrap();
    });
}

// TODO: rename to worker() or executor()?
/// Main loop running a worker thread.
fn main_loop() {
    /// Number of yields when no runnable task is found.
    const YIELDS: u32 = 3;
    /// Number of short sleeps when no runnable task in found.
    const SLEEPS: u32 = 1;

    // The number of times the thread found work in a row.
    let mut runs = 0;

    // The number of times the thread didn't find work in a row.
    let mut fails = 0;

    loop {
        if runs >= 64 {
            runs = 0;

            crate::net::driver::poll_quick();

            PROCESSOR.with(|proc| {
                let proc = proc.get().unwrap().lock();
                let proc = proc.as_ref().unwrap();

                while let Steal::Retry = POOL.injector.steal_batch(&proc.worker) {}

                if let Some(task) = proc.slot.take() {
                    proc.worker.push(task);
                }
            });
        }

        // Try to find a runnable task.
        match find_runnable() {
            Some(task) => {
                runs += 1;
                fails = 0;

                // Run the found task.
                task.run();
            }
            None => {
                runs = 0;
                fails += 1;

                // Yield the current thread or put it to sleep.
                if fails <= YIELDS {
                    thread::yield_now();
                } else if fails <= YIELDS + SLEEPS {
                    thread::sleep(Duration::from_micros(10));
                } else {
                    if POOL.deep_sleep.swap(true, Ordering::SeqCst) {
                        stop_worker();
                        return;
                    } else {
                        POOL.sleeps.fetch_add(1, Ordering::SeqCst);
                        crate::net::driver::poll();
                        POOL.deep_sleep.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

/// Find the next runnable task.
fn find_runnable() -> Option<Runnable> {
    PROCESSOR
        .with(|proc| {
            let proc = proc.get().unwrap().lock();
            let proc = proc.as_ref()?;

            if let Some(task) = proc.slot.take() {
                return Some(task);
            }

            if let Some(task) = proc.worker.pop() {
                return Some(task);
            }

            loop {
                match POOL.injector.steal_batch_and_pop(&proc.worker) {
                    Steal::Retry => {}
                    Steal::Empty => break,
                    Steal::Success(task) => return Some(task),
                }
            }

            None
        })
        .or_else(|| {
            crate::net::driver::poll_quick();

            PROCESSOR.with(|proc| {
                let proc = proc.get().unwrap().lock();
                let proc = proc.as_ref()?;

                if let Some(task) = proc.slot.take() {
                    return Some(task);
                }

                // First, pick a random starting point in the list of local queues.
                let len = POOL.stealers.len();
                let start = random(len as u32) as usize;

                let mut retry = true;
                while retry {
                    retry = false;

                    // Try stealing a batch of tasks from each local queue starting from the
                    // chosen point.
                    let (l, r) = POOL.stealers.split_at(start);
                    let stealers = r.iter().chain(l.iter());

                    for s in stealers {
                        match s.steal_batch_and_pop(&proc.worker) {
                            Steal::Retry => retry = true,
                            Steal::Empty => {}
                            Steal::Success(task) => return Some(task),
                        }
                    }
                }

                None
            })
        })
}
