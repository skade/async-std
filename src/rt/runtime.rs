use std::cell::Cell;
use std::ptr;
use std::sync::atomic::{self, AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_utils::thread::scope;
use once_cell::unsync::OnceCell;

use crate::rt::Reactor;
use crate::task::Runnable;
use crate::utils::{abort_on_panic, random, Spinlock};

thread_local! {
    static MACHINE: OnceCell<Arc<Machine>> = OnceCell::new();
    static YIELD_NOW: Cell<bool> = Cell::new(false);
}

/// Scheduler state.
struct Scheduler {
    /// Set to `true` every time before a machine blocks polling the reactor.
    progress: bool,

    /// Set to `true` while a machine is polling the reactor.
    polling: bool,

    /// Idle processors.
    processors: Vec<Processor>,

    /// Running machines.
    machines: Vec<Arc<Machine>>,
}

/// An async runtime.
pub struct Runtime {
    /// The reactor.
    reactor: Reactor,

    /// The global queue of tasks.
    injector: Injector<Runnable>,

    /// Handles to local queues for stealing work.
    stealers: Vec<Stealer<Runnable>>,

    /// The scheduler state.
    sched: Mutex<Scheduler>,
}

impl Runtime {
    /// Creates a new runtime.
    pub fn new() -> Runtime {
        let cpus = num_cpus::get().max(1);
        let processors: Vec<_> = (0..cpus).map(|_| Processor::new()).collect();
        let stealers = processors.iter().map(|p| p.worker.stealer()).collect();

        Runtime {
            reactor: Reactor::new().unwrap(),
            injector: Injector::new(),
            stealers,
            sched: Mutex::new(Scheduler {
                processors,
                machines: Vec::new(),
                progress: false,
                polling: false,
            }),
        }
    }

    /// Returns a reference to the reactor.
    pub fn reactor(&self) -> &Reactor {
        &self.reactor
    }

    pub fn yield_now(&self) {
        YIELD_NOW.with(|flag| flag.set(true));
    }

    /// Schedules a task.
    pub fn schedule(&self, task: Runnable) {
        MACHINE.with(|machine| {
            // If the current thread is a worker thread, store it into its task slot or push it
            // into its local task queue. Otherwise, push it into the global task queue.
            match machine.get() {
                None => {
                    self.injector.push(task);
                    self.notify();
                }
                Some(m) => m.schedule(&self, task),
            }
        });
    }

    /// Drives the runtime on the current thread.
    pub fn run(&self) {
        scope(|s| {
            let mut idle = 0;
            let mut delay = 0;

            loop {
                for m in self.make_machines() {
                    idle = 0;

                    s.builder()
                        .name("async-std/machine".to_string())
                        .spawn(move |_| {
                            abort_on_panic(|| {
                                let _ = MACHINE.with(|machine| machine.set(m.clone()));
                                m.run(self);
                            })
                        })
                        .expect("cannot start a machine thread");
                }

                if idle > 10 {
                    delay = (delay * 2).min(10_000);
                } else {
                    idle += 1;
                    delay = 1000;
                }

                thread::sleep(Duration::from_micros(delay));
            }
        })
        .unwrap();
    }

    fn make_machines(&self) -> Vec<Arc<Machine>> {
        let mut sched = self.sched.lock().unwrap();
        let mut to_start = Vec::new();

        // If there is a machine that is stuck on a task and not making any progress, steal
        // its processor and start a new machine to take over.
        for m in &mut sched.machines {
            if !m.progress.swap(false, Ordering::SeqCst) {
                let opt_p = m.processor.try_lock().and_then(|mut p| p.take());

                if let Some(p) = opt_p {
                    *m = Arc::new(Machine::new(p));
                    to_start.push(m.clone());
                }
            }
        }

        if !sched.polling {
            if !sched.progress {
                if let Some(p) = sched.processors.pop() {
                    let m = Arc::new(Machine::new(p));
                    to_start.push(m.clone());
                    sched.machines.push(m.clone());
                }
            }

            sched.progress = false;
        }

        to_start
    }

    /// Notifies the reactor.
    fn notify(&self) {
        atomic::fence(Ordering::SeqCst);
        self.reactor.notify().unwrap();
    }

    fn quick_poll(&self) {
        if let Ok(sched) = self.sched.try_lock() {
            if !sched.polling {
                self.reactor.poll(Some(Duration::from_secs(0))).unwrap();
            }
        }
    }
}

struct Machine {
    processor: Spinlock<Option<Processor>>,
    progress: AtomicBool,
}

impl Machine {
    fn new(p: Processor) -> Machine {
        Machine {
            processor: Spinlock::new(Some(p)),
            progress: AtomicBool::new(true),
        }
    }

    fn schedule(&self, rt: &Runtime, task: Runnable) {
        match self.processor.lock().as_mut() {
            None => {
                rt.injector.push(task);
                rt.notify();
            }
            Some(p) => p.schedule(rt, task),
        }
    }

    /// Finds the next runnable task.
    fn find_task(&self, rt: &Runtime) -> Option<Runnable> {
        if let Some(p) = self.processor.lock().as_mut() {
            if let Some(task) = p.pop_task().or_else(|| p.steal_from_global(rt)) {
                return Some(task);
            }
        }

        rt.quick_poll();

        if let Some(p) = self.processor.lock().as_mut() {
            if let Some(task) = p.pop_task().or_else(|| p.steal_from_others(rt)) {
                return Some(task);
            }
        }

        None
    }

    fn run(&self, rt: &Runtime) {
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

            YIELD_NOW.with(|flag| {
                if flag.replace(false) {
                    if let Some(p) = self.processor.lock().as_mut() {
                        p.flush_slot(rt);
                    }
                }
            });

            if runs >= 64 {
                rt.quick_poll();

                if let Some(p) = self.processor.lock().as_mut() {
                    if let Some(task) = p.steal_from_global(rt) {
                        p.schedule(rt, task);
                    }

                    p.flush_slot(rt);
                }
                runs = 0;
            }

            // Try to find a runnable task.
            if let Some(task) = self.find_task(rt) {
                task.run();
                runs += 1;
                fails = 0;
                continue;
            }

            // Check if the processor was stolen.
            if self.processor.lock().is_none() {
                break;
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

            fails = 0;
            let mut sched = rt.sched.lock().unwrap();

            if let Some(task) = self.find_task(rt) {
                self.schedule(rt, task);
                continue;
            }

            if sched.polling {
                break;
            }

            let m = match sched
                .machines
                .iter()
                .position(|elem| ptr::eq(&**elem, self))
            {
                None => break, // The processor was stolen.
                Some(pos) => sched.machines.swap_remove(pos),
            };

            sched.polling = true;
            drop(sched);

            rt.reactor.poll(None).unwrap();

            sched = rt.sched.lock().unwrap();
            sched.polling = false;
            sched.machines.push(m);
            sched.progress = true;
        }

        let opt_p = self.processor.lock().take();

        if let Some(p) = opt_p {
            let mut sched = rt.sched.lock().unwrap();
            sched.processors.push(p);
            sched.machines.retain(|elem| !ptr::eq(&**elem, self));
        }
    }
}

struct Processor {
    /// The local task queue.
    worker: Worker<Runnable>,

    /// Contains the next task to run as an optimization that skips queues.
    slot: Option<Runnable>,
}

impl Processor {
    /// Creates a new processor.
    fn new() -> Processor {
        Processor {
            worker: Worker::new_fifo(),
            slot: None,
        }
    }

    /// Schedules a task to run on this processor.
    fn schedule(&mut self, rt: &Runtime, task: Runnable) {
        match self.slot.replace(task) {
            None => {}
            Some(task) => {
                self.worker.push(task);
                rt.notify();
            }
        }
    }

    /// Flushes a task from the slot into the local queue.
    fn flush_slot(&mut self, rt: &Runtime) {
        if let Some(task) = self.slot.take() {
            self.worker.push(task);
            rt.notify();
        }
    }

    /// Pops a task from this processor.
    fn pop_task(&mut self) -> Option<Runnable> {
        self.slot.take().or_else(|| self.worker.pop())
    }

    /// Steals a task from the global queue.
    fn steal_from_global(&mut self, rt: &Runtime) -> Option<Runnable> {
        // Try stealing from the global queue.
        loop {
            match rt.injector.steal_batch_and_pop(&self.worker) {
                Steal::Retry => {}
                Steal::Empty => return None,
                Steal::Success(task) => return Some(task),
            }
        }
    }

    /// Steals a task from other processors.
    fn steal_from_others(&mut self, rt: &Runtime) -> Option<Runnable> {
        // Pick a random starting point in the list of queues.
        let len = rt.stealers.len();
        let start = random(len as u32) as usize;

        loop {
            let mut retry = false;

            // Create an iterator over stealers that starts from the chosen point.
            let (l, r) = rt.stealers.split_at(start);
            let stealers = r.iter().chain(l.iter());

            // Try stealing a batch of tasks from each queue.
            for s in stealers {
                match s.steal_batch_and_pop(&self.worker) {
                    Steal::Retry => retry = true,
                    Steal::Empty => {}
                    Steal::Success(task) => return Some(task),
                }
            }

            if !retry {
                return None;
            }
        }
    }
}
