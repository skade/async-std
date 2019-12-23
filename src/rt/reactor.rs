use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mio::{self, Evented};
use slab::Slab;

use crate::io;
use crate::rt::RUNTIME;
use crate::task::{Context, Poll, Waker};

/// Data associated with a registered I/O handle.
#[derive(Debug)]
struct Entry {
    /// A unique identifier.
    token: mio::Token,

    /// Tasks that are blocked on reading from this I/O handle.
    readers: Mutex<Vec<Waker>>,

    /// Tasks that are blocked on writing to this I/O handle.
    writers: Mutex<Vec<Waker>>,
}

/// The state of a networking driver.
pub struct Reactor {
    /// A mio instance that polls for new events.
    poller: mio::Poll,

    /// A list into which mio stores events.
    events: Mutex<mio::Events>,

    /// A collection of registered I/O handles.
    entries: Mutex<Slab<Arc<Entry>>>,

    /// Dummy I/O handle that is only used to wake up the polling thread.
    notify_reg: (mio::Registration, mio::SetReadiness),

    /// An identifier for the notification handle.
    notify_token: mio::Token,
}

impl Reactor {
    /// Creates a new reactor for polling I/O events.
    pub fn new() -> io::Result<Reactor> {
        let poller = mio::Poll::new()?;
        let notify_reg = mio::Registration::new2();

        let mut reactor = Reactor {
            poller,
            events: Mutex::new(mio::Events::with_capacity(1000)),
            entries: Mutex::new(Slab::new()),
            notify_reg,
            notify_token: mio::Token(0),
        };

        // Register a dummy I/O handle for waking up the polling thread.
        let entry = reactor.register(&reactor.notify_reg.0)?;
        reactor.notify_token = entry.token;

        Ok(reactor)
    }

    /// Registers an I/O event source and returns its associated entry.
    fn register(&self, source: &dyn Evented) -> io::Result<Arc<Entry>> {
        let mut entries = self.entries.lock().unwrap();

        // Reserve a vacant spot in the slab and use its key as the token value.
        let vacant = entries.vacant_entry();
        let token = mio::Token(vacant.key());

        // Allocate an entry and insert it into the slab.
        let entry = Arc::new(Entry {
            token,
            readers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
        });
        vacant.insert(entry.clone());

        // Register the I/O event source in the poller.
        let interest = mio::Ready::all();
        let opts = mio::PollOpt::edge();
        self.poller.register(source, token, interest, opts)?;

        Ok(entry)
    }

    /// Deregisters an I/O event source associated with an entry.
    fn deregister(&self, source: &dyn Evented, entry: &Entry) -> io::Result<()> {
        // Deregister the I/O object from the mio instance.
        self.poller.deregister(source)?;

        // Remove the entry associated with the I/O object.
        self.entries.lock().unwrap().remove(entry.token.0);

        Ok(())
    }

    /// Notifies the reactor so that polling stops blocking.
    pub fn notify(&self) -> io::Result<()> {
        self.notify_reg.1.set_readiness(mio::Ready::readable())
    }

    /// Waits on the poller for new events and wakes up tasks blocked on I/O handles.
    pub fn poll(&self, timeout: Option<Duration>) -> io::Result<()> {
        let mut events = self.events.lock().unwrap();

        // Block on the poller until at least one new event comes in.
        self.poller.poll(&mut events, timeout)?;

        // Lock the entire entry table while we're processing new events.
        let entries = self.entries.lock().unwrap();

        for event in events.iter() {
            let token = event.token();

            if token == self.notify_token {
                // If this is the notification token, we just need the notification state.
                self.notify_reg.1.set_readiness(mio::Ready::empty())?;
            } else {
                // Otherwise, look for the entry associated with this token.
                if let Some(entry) = entries.get(token.0) {
                    // Set the readiness flags from this I/O event.
                    let readiness = event.readiness();

                    // Wake up reader tasks blocked on this I/O handle.
                    let reader_interests = mio::Ready::all() - mio::Ready::writable();
                    if !(readiness & reader_interests).is_empty() {
                        for w in entry.readers.lock().unwrap().drain(..) {
                            w.wake();
                        }
                    }

                    // Wake up writer tasks blocked on this I/O handle.
                    let writer_interests = mio::Ready::all() - mio::Ready::readable();
                    if !(readiness & writer_interests).is_empty() {
                        for w in entry.writers.lock().unwrap().drain(..) {
                            w.wake();
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// An I/O handle powered by the networking driver.
///
/// This handle wraps an I/O event source and exposes a "futurized" interface on top of it,
/// implementing traits `AsyncRead` and `AsyncWrite`.
pub struct Watcher<T: Evented> {
    /// Data associated with the I/O handle.
    entry: Arc<Entry>,

    /// The I/O event source.
    source: Option<T>,
}

impl<T: Evented> Watcher<T> {
    /// Creates a new I/O handle.
    ///
    /// The provided I/O event source will be kept registered inside the reactor's poller for the
    /// lifetime of the returned I/O handle.
    pub fn new(source: T) -> Watcher<T> {
        Watcher {
            entry: RUNTIME
                .reactor()
                .register(&source)
                .expect("cannot register an I/O event source"),
            source: Some(source),
        }
    }

    /// Returns a reference to the inner I/O event source.
    pub fn get_ref(&self) -> &T {
        self.source.as_ref().unwrap()
    }

    /// Polls the inner I/O source for a non-blocking read operation.
    ///
    /// If the operation returns an error of the `io::ErrorKind::WouldBlock` kind, the current task
    /// will be registered for wakeup when the I/O source becomes readable.
    pub fn poll_read_with<'a, F, R>(&'a self, cx: &mut Context<'_>, mut f: F) -> Poll<io::Result<R>>
    where
        F: FnMut(&'a T) -> io::Result<R>,
    {
        // If the operation isn't blocked, return its result.
        match f(self.source.as_ref().unwrap()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Lock the waker list.
        let mut list = self.entry.readers.lock().unwrap();

        // Try running the operation again.
        match f(self.source.as_ref().unwrap()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Register the task if it isn't registered already.
        if list.iter().all(|w| !w.will_wake(cx.waker())) {
            list.push(cx.waker().clone());
        }

        Poll::Pending
    }

    /// Polls the inner I/O source for a non-blocking write operation.
    ///
    /// If the operation returns an error of the `io::ErrorKind::WouldBlock` kind, the current task
    /// will be registered for wakeup when the I/O source becomes writable.
    pub fn poll_write_with<'a, F, R>(
        &'a self,
        cx: &mut Context<'_>,
        mut f: F,
    ) -> Poll<io::Result<R>>
    where
        F: FnMut(&'a T) -> io::Result<R>,
    {
        // If the operation isn't blocked, return its result.
        match f(self.source.as_ref().unwrap()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Lock the waker list.
        let mut list = self.entry.writers.lock().unwrap();

        // Try running the operation again.
        match f(self.source.as_ref().unwrap()) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            res => return Poll::Ready(res),
        }

        // Register the task if it isn't registered already.
        if list.iter().all(|w| !w.will_wake(cx.waker())) {
            list.push(cx.waker().clone());
        }

        Poll::Pending
    }

    /// Deregisters and returns the inner I/O source.
    ///
    /// This method is typically used to convert `Watcher`s to raw file descriptors/handles.
    #[allow(dead_code)]
    pub fn into_inner(mut self) -> T {
        let source = self.source.take().unwrap();
        RUNTIME
            .reactor()
            .deregister(&source, &self.entry)
            .expect("cannot deregister I/O event source");
        source
    }
}

impl<T: Evented> Drop for Watcher<T> {
    fn drop(&mut self) {
        if let Some(ref source) = self.source {
            RUNTIME
                .reactor()
                .deregister(source, &self.entry)
                .expect("cannot deregister I/O event source");
        }
    }
}

impl<T: Evented + fmt::Debug> fmt::Debug for Watcher<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watcher")
            .field("entry", &self.entry)
            .field("source", &self.source)
            .finish()
    }
}
