use std::cell::Cell;
use std::sync::atomic::{self, AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use once_cell::sync::Lazy;
use once_cell::unsync::OnceCell;

use crate::task::Runnable;
use crate::utils::{abort_on_panic, random, Spinlock};

/// The state of an scheduler.
struct Scheduler {
    /// The global queue of tasks.
    injector: Injector<Runnable>,

    /// Handles to local queues for stealing work from worker threads.
    stealers: Vec<Stealer<Runnable>>,

    ticker: AtomicUsize,

    // TODO: We need something like Reactor::is_active()
    deep_sleep: AtomicBool,

    pool: Mutex<Pool>,
}

impl Scheduler {
    fn new() -> Scheduler {
        let mut processors = Vec::new();
        let mut stealers = Vec::new();

        // Create processors.
        for _ in 0..num_cpus::get().max(1) {
            let worker = Worker::new_fifo();
            stealers.push(worker.stealer());

            processors.push(Processor {
                worker,
                slot: Cell::new(None),
            });
        }

        Scheduler {
            injector: Injector::new(),
            stealers,
            pool: Mutex::new(Pool {
                processors,
                machines: Vec::new(),
            }),
            ticker: AtomicUsize::new(0),
            deep_sleep: AtomicBool::new(false),
        }
    }

    fn run(&self) {
        self.pool.lock().unwrap().spin_up();

        let mut tick = 0;

        loop {
            thread::sleep(Duration::from_millis(10));

            self.pool.lock().unwrap().check_stuck_machines();

            let t = self.ticker.load(Ordering::SeqCst);
            if tick != t {
                tick = t;
            } else {
                if !self.deep_sleep.load(Ordering::SeqCst) {
                    self.pool.lock().unwrap().spin_up();
                }
            }

            crate::net::driver::poll_quick();
        }
    }
}

struct Pool {
    processors: Vec<Processor>,
    machines: Vec<Arc<Machine>>,
}

impl Pool {
    fn spin_up(&mut self) {
        if let Some(p) = self.processors.pop() {
            let m = Arc::new(Machine::new(p));
            self.machines.push(m.clone());
            Self::start_machine(m);
        }
    }

    fn check_stuck_machines(&mut self) {
        for m in &mut self.machines {
            if !m.sleeping.load(Ordering::SeqCst) && !m.progress.swap(false, Ordering::SeqCst) {
                let opt_p = m.processor.lock().take();
                if let Some(p) = opt_p {
                    *m = Arc::new(Machine::new(p));
                    // println!("STEAL");
                    Self::start_machine(m.clone());
                }
            }
        }
    }

    fn start_machine(m: Arc<Machine>) {
        thread::Builder::new()
            .name("async-std/machine".to_string())
            .spawn(|| {
                MACHINE.with(|machine| {
                    let _ = machine.set(m);
                    let m = machine.get().unwrap();

                    if m.run() {
                        let mut opt_p = m.processor.lock();

                        if let Some(p) = opt_p.take() {
                            let mut pool = SCHEDULER.pool.lock().unwrap();
                            pool.processors.push(p);

                            if let Some(i) =
                                pool.machines.iter().position(|elem| Arc::ptr_eq(elem, m))
                            {
                                pool.machines.swap_remove(i);
                            }
                        }
                    }
                })
            })
            .expect("cannot start a machine thread");
    }
}

/// The state of a worker thread.
struct Processor {
    /// The local task queue.
    worker: Worker<Runnable>,
    /// Contains the next task to run as an optimization that skips queues.
    slot: Cell<Option<Runnable>>,
}

/// Global scheduler that runs spawned tasks.
static SCHEDULER: Lazy<Scheduler> = Lazy::new(|| {
    thread::Builder::new()
        .name("async-std/scheduler".to_string())
        .spawn(|| SCHEDULER.run())
        .expect("cannot start a scheduler thread");

    Scheduler::new()
});

struct Machine {
    processor: Spinlock<Option<Processor>>,
    sleeping: AtomicBool,
    progress: AtomicBool,
}

impl Machine {
    fn new(p: Processor) -> Machine {
        Machine {
            processor: Spinlock::new(Some(p)),
            sleeping: AtomicBool::new(false),
            progress: AtomicBool::new(true),
        }
    }

    fn run(&self) -> bool {
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

                if let Some(p) = self.processor.lock().as_ref() {
                    while let Steal::Retry = SCHEDULER.injector.steal_batch(&p.worker) {}

                    if let Some(task) = p.slot.take() {
                        p.worker.push(task);
                    }
                }
            }

            // Try to find a runnable task.
            if let Some(task) = self.find_runnable() {
                task.run();
                runs += 1;
                fails = 0;
                continue;
            }

            if self.processor.lock().is_none() {
                // println!("STOLEN");
                // NOTE: return instead of break
                return false;
            }

            runs = 0;
            fails += 1;

            // Yield the current thread or put it to sleep.
            if fails <= YIELDS {
                thread::yield_now();
                continue;
            }

            if fails <= YIELDS + SLEEPS {
                thread::sleep(Duration::from_micros(10));
                continue;
            }

            // TODO: can this be a lock over mio?
            if SCHEDULER.deep_sleep.swap(true, Ordering::SeqCst) {
                return true;
            }

            if let Some(task) = self.find_runnable() {
                SCHEDULER.deep_sleep.store(false, Ordering::SeqCst);
                task.run();
                runs += 1;
                fails = 0;
                continue;
            }

            SCHEDULER.ticker.fetch_add(1, Ordering::SeqCst);

            self.sleeping.store(true, Ordering::SeqCst);
            crate::net::driver::poll();
            self.sleeping.store(false, Ordering::SeqCst);

            SCHEDULER.deep_sleep.store(false, Ordering::SeqCst);
        }
    }

    /// Finds the next runnable task.
    fn find_runnable(&self) -> Option<Runnable> {
        {
            let opt_p = self.processor.lock();
            let p = opt_p.as_ref()?;

            self.progress.store(true, Ordering::SeqCst);

            if let Some(task) = p.slot.take() {
                return Some(task);
            }

            if let Some(task) = p.worker.pop() {
                return Some(task);
            }

            loop {
                match SCHEDULER.injector.steal_batch_and_pop(&p.worker) {
                    Steal::Retry => {}
                    Steal::Empty => break,
                    Steal::Success(task) => return Some(task),
                }
            }
        }

        crate::net::driver::poll_quick();

        let opt_p = self.processor.lock();
        let p = opt_p.as_ref()?;

        if let Some(task) = p.slot.take() {
            return Some(task);
        }

        // First, pick a random starting point in the list of local queues.
        let len = SCHEDULER.stealers.len();
        let start = random(len as u32) as usize;

        let mut retry = true;
        while retry {
            retry = false;

            // Try stealing a batch of tasks from each local queue starting from the
            // chosen point.
            let (l, r) = SCHEDULER.stealers.split_at(start);
            let stealers = r.iter().chain(l.iter());

            for s in stealers {
                match s.steal_batch_and_pop(&p.worker) {
                    Steal::Retry => retry = true,
                    Steal::Empty => {}
                    Steal::Success(task) => return Some(task),
                }
            }
        }

        None
    }
}

thread_local! {
    /// Worker thread state.
    static MACHINE: OnceCell<Arc<Machine>> = OnceCell::new();
}

/// Schedules a new runnable task for execution.
pub(crate) fn schedule(task: Runnable) {
    let mut notify = false;

    MACHINE.with(|machine| {
        // If the current thread is a worker thread, store it into its task slot or push it into
        // its local task queue. Otherwise, push it into the global task queue.
        match machine.get() {
            Some(m) => {
                if let Some(p) = m.processor.lock().as_ref() {
                    // Replace the task in the slot.
                    if let Some(task) = p.slot.replace(Some(task)) {
                        // If the slot already contained a task, push it into the local task queue.
                        p.worker.push(task);
                        notify = true;
                    }
                    return;
                }
            }
            None => {}
        }

        SCHEDULER.injector.push(task);
        notify = true;
    });

    // TODO: just notify the driver, which will hold deep_sleep
    if notify && SCHEDULER.deep_sleep.load(Ordering::SeqCst) {
        atomic::fence(Ordering::SeqCst);
        crate::net::driver::notify();
    }
}
