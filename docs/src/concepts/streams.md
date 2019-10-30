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

## Stream combinators

## Channels


## Stream patterns

### Merging

### Splitting

## Channels


