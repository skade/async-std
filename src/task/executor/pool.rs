use std::cell::Cell;
use std::sync::atomic::{self, AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use once_cell::sync::Lazy;
use once_cell::unsync::OnceCell;

use crate::task::Runnable;
use crate::utils::{abort_on_panic, random, Spinlock};

/// The state of an executor.
struct Executor {
    /// The global queue of tasks.
    injector: Injector<Runnable>,

    /// Handles to local queues for stealing work from worker threads.
    stealers: Vec<Stealer<Runnable>>,

    sleeps: AtomicU64,

    deep_sleep: AtomicBool,

    pool: Mutex<Pool>,
}

struct Pool {
    procs: Vec<Processor>,

    machs: Vec<Arc<Machine>>,
}

/// The state of a worker thread.
struct Processor {
    /// The local task queue.
    worker: Worker<Runnable>,

    /// Contains the next task to run as an optimization that skips queues.
    slot: Cell<Option<Runnable>>,
}

struct Machine {
    proc: Spinlock<Option<Processor>>,
    sleeping: AtomicBool,
    progress: AtomicBool,
}

impl Machine {
    fn new(proc: Processor) -> Machine {
        Machine {
            proc: Spinlock::new(Some(proc)),
            sleeping: AtomicBool::new(false),
            progress: AtomicBool::new(true),
        }
    }
}

/// Global executor that runs spawned tasks.
static EXECUTOR: Lazy<Executor> = Lazy::new(|| {
    let num_procs = num_cpus::get().max(1);
    let mut stealers = Vec::new();
    let mut procs = Vec::new();

    // Spawn worker threads.
    for _ in 0..num_procs {
        let worker = Worker::new_fifo();
        stealers.push(worker.stealer());

        let proc = Processor {
            worker,
            slot: Cell::new(None),
        };
        procs.push(proc);
    }

    thread::Builder::new()
        .name("async-std/sysmon".to_string())
        .spawn(|| abort_on_panic(sysmon))
        .expect("cannot start a sysmon thread");

    let proc = procs.pop().unwrap();
    let mach = Arc::new(Machine::new(proc));
    start_worker(mach.clone());

    Executor {
        injector: Injector::new(),
        stealers,
        pool: Mutex::new(Pool {
            procs: procs,
            machs: vec![mach],
        }),
        sleeps: AtomicU64::new(0),
        deep_sleep: AtomicBool::new(false),
    }
});

thread_local! {
    /// Worker thread state.
    static MACHINE: OnceCell<Arc<Machine>> = OnceCell::new();
}

/// Schedules a new runnable task for execution.
pub(crate) fn schedule(task: Runnable) {
    let mut notify = false;

    MACHINE.with(|mach| {
        // If the current thread is a worker thread, store it into its task slot or push it into
        // its local task queue. Otherwise, push it into the global task queue.
        match mach.get() {
            Some(mach) => {
                if let Some(proc) = mach.proc.lock().as_ref() {
                    // Replace the task in the slot.
                    if let Some(task) = proc.slot.replace(Some(task)) {
                        // If the slot already contained a task, push it into the local task queue.
                        proc.worker.push(task);
                        notify = true;
                    }
                    return;
                }
            }
            None => {}
        }

        EXECUTOR.injector.push(task);
        notify = true;
    });

    if notify && EXECUTOR.deep_sleep.load(Ordering::SeqCst) {
        atomic::fence(Ordering::SeqCst);
        crate::net::driver::notify();
    }
}

fn sysmon() {
    let mut sleeps = 0;
    loop {
        thread::sleep(Duration::from_millis(10));

        {
            let mut pool = EXECUTOR.pool.lock().unwrap();

            for m in &mut pool.machs {
                if !m.sleeping.load(Ordering::SeqCst) && !m.progress.swap(false, Ordering::SeqCst) {
                    let proc = m.proc.lock().take();
                    if let Some(proc) = proc {
                        *m = Arc::new(Machine::new(proc));
                        // println!("STEAL");
                        start_worker(m.clone());
                    }
                }
            }

            let s = EXECUTOR.sleeps.load(Ordering::SeqCst);
            if sleeps != s {
                sleeps = s;
            } else {
                if !EXECUTOR.deep_sleep.load(Ordering::SeqCst) {
                    if let Some(proc) = pool.procs.pop() {
                        let mach = Arc::new(Machine::new(proc));
                        pool.machs.push(mach.clone());
                        // println!("START");
                        start_worker(mach);
                    }
                }
            }
        }

        crate::net::driver::poll_quick();
    }
}

fn start_worker(mach: Arc<Machine>) {
    thread::Builder::new()
        .name("async-std/worker".to_string())
        .spawn(|| {
            let _ = MACHINE.with(|p| p.set(mach));
            abort_on_panic(worker);
        })
        .expect("cannot start a worker thread");
}

/// Main loop running a worker thread.
fn worker() {
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

            MACHINE.with(|mach| {
                let mach = mach.get().unwrap();

                if let Some(proc) = mach.proc.lock().as_ref() {
                    while let Steal::Retry = EXECUTOR.injector.steal_batch(&proc.worker) {}

                    if let Some(task) = proc.slot.take() {
                        proc.worker.push(task);
                    }
                }
            });
        }

        // Try to find a runnable task.
        if let Some(task) = find_runnable() {
            task.run();
            runs += 1;
            fails = 0;
            continue;
        }

        {
            let is_stolen = MACHINE.with(|mach| {
                let mach = mach.get().unwrap().proc.lock();
                mach.is_none()
            });
            if is_stolen {
                // println!("STOLEN");
                // NOTE: return instead of break
                return;
            }
        }

        runs = 0;
        fails += 1;

        // Yield the current thread or put it to sleep.
        if fails <= YIELDS {
            thread::yield_now();
        } else if fails <= YIELDS + SLEEPS {
            thread::sleep(Duration::from_micros(10));
        } else {
            // TODO: can this be a lock over mio?
            if EXECUTOR.deep_sleep.swap(true, Ordering::SeqCst) {
                break;
            }

            if let Some(task) = find_runnable() {
                EXECUTOR.deep_sleep.store(false, Ordering::SeqCst);
                task.run();
                runs += 1;
                fails = 0;
                continue;
            }

            EXECUTOR.sleeps.fetch_add(1, Ordering::SeqCst);

            MACHINE.with(|mach| {
                let mach = mach.get().unwrap();
                mach.sleeping.store(true, Ordering::SeqCst);
                crate::net::driver::poll();
                mach.sleeping.store(false, Ordering::SeqCst);
            });

            EXECUTOR.deep_sleep.store(false, Ordering::SeqCst);
        }
    }

    MACHINE.with(|mach| {
        let arc = mach.get().unwrap();
        let mut proc = arc.proc.lock();

        if let Some(proc) = proc.take() {
            let mut pool = EXECUTOR.pool.lock().unwrap();
            pool.procs.push(proc);
            pool.machs.retain(|p| !Arc::ptr_eq(p, arc));
        }
    });
}

/// Find the next runnable task.
fn find_runnable() -> Option<Runnable> {
    MACHINE.with(|mach| {
        {
            let mach = mach.get().unwrap();
            let proc = mach.proc.lock();
            let proc = proc.as_ref()?;

            mach.progress.store(true, Ordering::SeqCst);

            if let Some(task) = proc.slot.take() {
                return Some(task);
            }

            if let Some(task) = proc.worker.pop() {
                return Some(task);
            }

            loop {
                match EXECUTOR.injector.steal_batch_and_pop(&proc.worker) {
                    Steal::Retry => {}
                    Steal::Empty => break,
                    Steal::Success(task) => return Some(task),
                }
            }
        }

        crate::net::driver::poll_quick();

        let mach = mach.get().unwrap().proc.lock();
        let proc = mach.as_ref()?;

        if let Some(task) = proc.slot.take() {
            return Some(task);
        }

        // First, pick a random starting point in the list of local queues.
        let len = EXECUTOR.stealers.len();
        let start = random(len as u32) as usize;

        let mut retry = true;
        while retry {
            retry = false;

            // Try stealing a batch of tasks from each local queue starting from the
            // chosen point.
            let (l, r) = EXECUTOR.stealers.split_at(start);
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
}
