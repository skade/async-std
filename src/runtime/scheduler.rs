use std::cell::Cell;
use std::sync::atomic::{self, AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use once_cell::unsync::OnceCell;

use crate::runtime::Reactor;
use crate::task::Runnable;
use crate::utils::{abort_on_panic, random, Spinlock};

thread_local! {
    /// Worker thread state.
    static MACHINE: OnceCell<Arc<Machine>> = OnceCell::new();
}

/// A scheduler.
pub struct Scheduler {
    state: Arc<State>,
}

impl Scheduler {
    /// Creates a new scheduler.
    pub fn new() -> Scheduler {
        let cpus = num_cpus::get().max(1);
        let processors: Vec<_> = (0..cpus).map(|_| Processor::new()).collect();
        let stealers = processors.iter().map(|p| p.worker.stealer()).collect();

        Scheduler {
            state: Arc::new(State {
                reactor: Reactor::new().unwrap(),
                injector: Injector::new(),
                stealers,
                processors: Mutex::new(processors),
                machines: Mutex::new(Vec::new()),
                progress: AtomicBool::new(false),
                polling: AtomicBool::new(false),
            }),
        }
    }

    /// Returns a reference to the reactor.
    pub fn reactor(&self) -> &Reactor {
        &self.state.reactor
    }

    /// Runs the scheduler on the current thread.
    pub fn run(&self) {
        self.spin_up();

        let mut idle = 0;
        let mut delay = 0;

        loop {
            if idle > 10 {
                delay = (delay * 2).min(10_000);
            } else {
                idle += 1;
                delay = 500;
            }

            thread::sleep(Duration::from_micros(delay));

            if self.check_machines() {
                idle = 0;
            }

            if !self.state.progress.swap(false, Ordering::SeqCst) {
                if !self.state.polling.load(Ordering::SeqCst) {
                    idle = 0;
                    self.spin_up();
                }
            }
        }
    }

    /// Schedules a task.
    pub fn schedule(&self, task: Runnable) {
        MACHINE.with(|machine| {
            // If the current thread is a worker thread, store it into its task slot or push it into
            // its local task queue. Otherwise, push it into the global task queue.
            match machine.get() {
                None => {
                    self.state.injector.push(task);
                    self.state.notify();
                }
                Some(m) => m.schedule(&self.state, task),
            }
        });
    }

    /// Takes an idle processor and spins up a machine to run it.
    fn spin_up(&self) {
        if let Some(p) = self.state.processors.lock().unwrap().pop() {
            let m = Arc::new(Machine::new(p));
            self.state.machines.lock().unwrap().push(m.clone());
            self.start(m);
        }
    }

    /// Checks the status of running machines.
    ///
    /// If there is a machine that is stuck on a task and not making any progress, its processor
    /// will be stolen and a new machine will then start running it.
    fn check_machines(&self) -> bool {
        for m in self.state.machines.lock().unwrap().iter_mut() {
            if !m.polling.load(Ordering::SeqCst) && !m.progress.swap(false, Ordering::SeqCst) {
                let opt_p = m.processor.lock().take();

                if let Some(p) = opt_p {
                    *m = Arc::new(Machine::new(p));
                    // println!("STEAL");
                    self.start(m.clone());
                    return true;
                }
            }
        }
        false
    }

    fn start(&self, m: Arc<Machine>) {
        let state = self.state.clone();

        thread::Builder::new()
            .name("async-std/machine".to_string())
            .spawn(move || {
                abort_on_panic(|| {
                    MACHINE.with(|machine| {
                        let _ = machine.set(m);
                        let m = machine.get().unwrap();

                        if m.run(&state) {
                            state.spin_down(m.clone());
                        }
                    })
                })
            })
            .expect("cannot start a machine thread");
    }
}

/// Shared scheduler state.
struct State {
    /// The reactor.
    reactor: Reactor,

    /// The global queue of tasks.
    injector: Injector<Runnable>,

    /// Handles to local queues for stealing work.
    stealers: Vec<Stealer<Runnable>>,

    /// Set to `true` every time before a thread blocks on the reactor.
    progress: AtomicBool,

    /// Set to `true` while a thread is blocking on the reactor.
    polling: AtomicBool,

    /// Idle processors.
    processors: Mutex<Vec<Processor>>,

    /// Machines with assigned processors.
    machines: Mutex<Vec<Arc<Machine>>>,
}

impl State {
    /// Notifies the reactor.
    fn notify(&self) {
        if self.polling.load(Ordering::SeqCst) {
            atomic::fence(Ordering::SeqCst);
            self.reactor.notify().unwrap();
        }
    }

    /// Spins down a machine.
    fn spin_down(&self, m: Arc<Machine>) {
        if let Some(p) = m.processor.lock().take() {
            self.processors.lock().unwrap().push(p);

            let mut machines = self.machines.lock().unwrap();
            if let Some(i) = machines.iter().position(|elem| Arc::ptr_eq(elem, &m)) {
                machines.swap_remove(i);
            }
        }
    }
}

struct Machine {
    processor: Spinlock<Option<Processor>>,
    progress: AtomicBool,
    polling: AtomicBool,
}

impl Machine {
    fn new(p: Processor) -> Machine {
        Machine {
            processor: Spinlock::new(Some(p)),
            progress: AtomicBool::new(true),
            polling: AtomicBool::new(false),
        }
    }

    fn schedule(&self, state: &Arc<State>, task: Runnable) {
        match self.processor.lock().as_ref() {
            None => {
                state.injector.push(task);
                state.notify();
            }
            Some(p) => p.schedule(state, task),
        }
    }

    /// Finds the next runnable task.
    fn find_task(&self, state: &Arc<State>) -> Option<Runnable> {
        if let Some(p) = self.processor.lock().as_ref() {
            if let Some(task) = p.find_task(state, false) {
                return Some(task);
            }
        }

        state.reactor.poll_quick().unwrap();

        if let Some(p) = self.processor.lock().as_ref() {
            if let Some(task) = p.find_task(state, true) {
                return Some(task);
            }
        }

        None
    }

    fn flush(&self, state: &Arc<State>) {
        state.reactor.poll_quick().unwrap();

        if let Some(p) = self.processor.lock().as_ref() {
            while let Steal::Retry = state.injector.steal_batch(&p.worker) {}

            if let Some(task) = p.slot.take() {
                p.worker.push(task);
            }
        }
    }

    fn run(&self, state: &Arc<State>) -> bool {
        /// Number of yields when no runnable task is found.
        const YIELDS: u32 = 3;
        /// Number of short sleeps when no runnable task in found.
        const SLEEPS: u32 = 1;

        // The number of times the thread found work in a row.
        let mut runs = 0;
        // The number of times the thread didn't find work in a row.
        let mut fails = 0;

        loop {
            self.progress.store(true, Ordering::SeqCst);

            if runs >= 64 {
                self.flush(state);
                runs = 0;
            }

            // Try to find a runnable task.
            if let Some(task) = self.find_task(state) {
                task.run();
                runs += 1;
                fails = 0;
                continue;
            }

            // Check if the processor was stolen.
            if self.processor.lock().is_none() {
                return false;
            }

            runs = 0;
            fails += 1;

            // Yield the current thread a few times.
            if fails <= YIELDS {
                thread::yield_now();
                continue;
            }

            // Put the current thread to sleep a few times.
            if fails <= YIELDS + SLEEPS {
                thread::sleep(Duration::from_micros(10));
                continue;
            }

            if state.polling.swap(true, Ordering::SeqCst) {
                return true;
            }

            if let Some(task) = self.find_task(&state) {
                self.schedule(state, task);
            } else {
                self.polling.store(true, Ordering::SeqCst);

                state.progress.store(true, Ordering::SeqCst);
                state.reactor.poll().unwrap();

                self.polling.store(false, Ordering::SeqCst);
            }

            state.polling.store(false, Ordering::SeqCst);
        }
    }
}

/// The state of a worker thread.
struct Processor {
    /// The local task queue.
    worker: Worker<Runnable>,

    /// Contains the next task to run as an optimization that skips queues.
    slot: Cell<Option<Runnable>>,
}

impl Processor {
    /// Creates a new processor.
    fn new() -> Processor {
        Processor {
            worker: Worker::new_fifo(),
            slot: Cell::new(None),
        }
    }

    /// Schedules a task to run on this processor.
    fn schedule(&self, state: &Arc<State>, task: Runnable) {
        match self.slot.replace(Some(task)) {
            None => {}
            Some(task) => {
                self.worker.push(task);
                state.notify();
            }
        }
    }

    /// Finds a runnable task.
    ///
    /// This method might stealing some tasks from the global queue or from sibling processors.
    fn find_task(&self, state: &Arc<State>, steal_from_siblings: bool) -> Option<Runnable> {
        // Try taking the task from the slot.
        if let Some(task) = self.slot.take() {
            return Some(task);
        }

        // Try popping a task from the local queue.
        if let Some(task) = self.worker.pop() {
            return Some(task);
        }

        // Try stealing from the global queue.
        loop {
            match state.injector.steal_batch_and_pop(&self.worker) {
                Steal::Retry => {}
                Steal::Empty => break,
                Steal::Success(task) => return Some(task),
            }
        }

        if steal_from_siblings {
            // First, pick a random starting point in the list of queues.
            let len = state.stealers.len();
            let start = random(len as u32) as usize;

            loop {
                let mut retry = false;

                // Try stealing a batch of tasks from each queue starting from the chosen point.
                let (l, r) = state.stealers.split_at(start);
                let stealers = r.iter().chain(l.iter());

                for s in stealers {
                    match s.steal_batch_and_pop(&self.worker) {
                        Steal::Retry => retry = true,
                        Steal::Empty => {}
                        Steal::Success(task) => return Some(task),
                    }
                }

                if !retry {
                    break;
                }
            }
        }

        None
    }
}
