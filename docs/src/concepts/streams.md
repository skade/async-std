# Streams and Channels

_Streams_ are a second important type in Rusts async story.
Streams are asynchronous sequences - they provide functionality centered to handle many incoming events.
As such, they can be seem as similar to _Iterator_s and share many of their properties.

In difference to _Futures_, they might not always be used in an abstract fashion, but are worth learning in the abstract nonetheless.

## A basic example

To make this more tangible, here's some examples for streams:

```rust
use async_std::TcpListener;

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Listening on {}", listener.local_addr()?);

    let mut incoming = listener.incoming();

    while let Some(stream) = incoming.next().await {
        // handle connected stream
    }
}
```

`incoming()` produces [`async_std::net::Incoming`](https://docs.rs/async-std/latest/async_std/net/struct.Incoming.html), which produces all incoming connections on the socket we bound to, in sequence.
This is exactly the same as [`std::net::Incoming`](https://doc.rust-lang.org/std/net/struct.Incoming.html), except that it doesn't block the thread that calls `next()`.

You can immediately save the "while" loop as the proper way to loop over a stream in your head. As Rust does not have async for loops similar to the for loop for `Iterator`s, it is the best way to express the pattern.

It's important to understand what `Incoming` expresses: it is the type representing _all_ future connections, one after another.
 If `Futures` are types that produce a single value, `Streams` handle sequences.

## The Stream interface

Let's have a look at the `Stream` interface:

```rust
trait Stream {
    /// The type of items yielded by this stream.
    type Item;

    /// Attempts to receive the next item from the stream.
    /// 
    /// There are several possible return values:
    /// 
    /// * Poll::Pending means this stream's next value is not ready yet.
    /// * Poll::Ready(None) means this stream has been exhausted.
    /// * Poll::Ready(Some(Self::Item)) means item was received out of the stream.
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>>;
}
```

This is similar to the `Futures` interface: it only has one method and it has "poll" in its name. This one is a little different, though: while the `poll` method of futures returns `Ready` once and the future is done, this one may become `Ready` multiple times.

The protocol here is similar to `Iterator`, just with `Poll` added: if `Ready` contains a `Some` value, the `Stream` can be polled again, if `Ready` ends up a `None` value, it is done.

To sum it up: `Stream`s give us the ability to check if a new item in the sequence is a available and otherwise wait. After we received an item, they allow us to wait for the next.

So far, so easy. But how do we _drive_ this process? We cannot `.await` streams, as we need a `Future` to use `.await`.

For a detailled overview of the `Stream` API, see [`the Streams chapter in the documentation`](TODO: link).

## The connection between `Stream` and `Future`

To run code in `async-std`, you need a task. Tasks are built around `Future`s, and not around `Stream`s. This means we need a bridge between both worlds. And that one is pretty simple: we need to find actions on streams that are expressable in terms of the `Future` interface. They must produce single values.

Consider this: if a `Stream` is a sequence of many values of the kind `Item`, waiting for the _next_ item in the stream to arrive is a `Future` of the kind `Option<Item>`. This is our simplest bridge. In or example above, we consume incoming connections one-by-one, by always waiting for the `next()` connection.

Other asynchronous functions that can be used on `Stream`s are:
* `collect`: read the full stream into a data structure, e.g. a `Vec`. Similar to the `Iterator` method of the same name.
* `for_each`: apply a closure to every item in the stream, ignoring the result. This is similar to the `Iterator` method of the same name.

## Serial Streams

<img src="/assets/graphs/list.jpg" alt="list" style="max-width:300px">

The most basic way of processing a stream is in a sequence. For each element
produced in the stream we perform an operation. We guaranteed a new operation is
run only after the previous operation has ended.

In today's Rust this is usually written using a `while let Some / await` loop,
`map` or `for_each` combinator:

```rust
let a = stream::repeat(1u8);
while let Some(num) = a.next().await {
    println!("{:?}", num);
}

let b = stream::repeat(1u8)
    .map(|num| dbg!(num));

let c = stream::repeat(1u8)
    .for_each(|num| println!("{:?}", num));
```

The difference between `map` and `for_each` is that the former creates a new
stream of output, while the latter does not accept output. For
[fallible](https://blog.yoshuawuyts.com/contexts/) streams, there exist
`try_map` and `try_for_each` counterparts that are able to short-circuit when an
`Result::Err` is returned.

The `while let Some / await` loop is mostly comparable to the `for_each` /
`try_for_each` combinators (with the biggest difference being able to manually
short-circuit using `break` and `continue`).

Serial parsing of streams is most comparable to synchronous loops. Except
instead of blocking between iterations of the loop, it sleeps the task.
Just like serial processing of iterators is a common operation, so is serial
processing of streams.

## Multiplexed Streams

It's not rare to find yourself in a situation where you have multiple streams,
and you want to act on their combined output. For example there might be a
stream of events from a channel, but also another stream that checks for a
"shutdown" event.

```rust
let events = events.recv();       // stream of events
let shutdown = shutdown.recv();   // stream of shutdown events
```

Perhaps you'd like to expose a combined stream of both streams (as might be the
case for networking protocols), or simply await both streams concurrently. The
solution to that would be to map both streams to a shared enum, and then merge
both streams into a single stream. The output can then be looped and matched
over as you're used to:

[`stream::merge` method]: https://docs.rs/async-std/latest/async_std/stream/trait.Stream.html#method.merge

```rust
enum Event<T> {
    Message<T>,
    Shutdown,
}

let events = rx.recv().map(|ev| Event::Message(ev));
let shutdown = shutdown.recv().map(|_| Event::Shutdown);
let s = events.merge(shutdown); // Combined stream of both

while let Some(ev) = s.next().await {
    match ev {
        Event::Message(msg) => println!("message was: {:?}", msg),
        Event::Shutdown => break,
    }
}
```

In `async-std` we provide the [`stream::merge` method] to merge multiple streams
into a single stream. This is different from `futures-rs` which provides the
Golang-inspired [`select! {}`]. As `async-std` implements a fully `futures-rs` compatible interface, you can opt into using `select` instead.

Both approaches achieve more or less the same goals. But we felt `merge` would
be both simpler in both implementation and usage because it leaves much of the
trickier control flow constructs up to existing keywords such as `match`,
`break`, and `continue`.

## Branching Streams

Sometimes you have a single stream that you want to convert into multiple
streams. The way to do this in `std` is by using [`crossbeam_channel`] (because
don't use std channels).

Similarly in async Rust `channel` is the right abstraction. By creating multiple
channels we can use it to split a single stream into multiple streams based on
its output.

[`crossbeam_channel`]: https://docs.rs/crossbeam-channel/0.3.9/crossbeam_channel/
[`sync::channel`]: https://doc.rust-lang.org/std/sync/mpsc/fn.channel.html

```rust
use async_std::stream;
use async_std::sync;

let a = sync::channel::new();
let b = sync::channel::new();

let s = stream::repeat(10u8).take(20);
while let Some(num) = s.next().await {
    match num % 1 {
        0 => a.send(num).await;
        _ => b.send(num).await;
    }
}
```

Channels are currently [still being
built](https://github.com/async-rs/async-std/issues/212) for `async-std`, but
we expect to have them available in the near future.


## Channels


