use std::pin::Pin;

use async_macros::MaybeDone;
use pin_project_lite::pin_project;

use std::future::Future;
use crate::task::{Context, Poll};

pin_project! {
    #[allow(missing_docs)]
    #[allow(missing_debug_implementations)]
    pub struct TrySelect<L, R> where L: Future, R: Future<Output = L::Output> {
        #[pin] left: MaybeDone<L>,
        #[pin] right: MaybeDone<R>,
    }
}

impl<L, R> TrySelect<L, R>
where
    L: Future,
    R: Future<Output = L::Output>,
{
    pub(crate) fn new(left: L, right: R) -> Self {
        Self {
            left: MaybeDone::new(left),
            right: MaybeDone::new(right),
        }
    }
}

impl<L, R, T, E> Future for TrySelect<L, R>
where
    L: Future<Output = Result<T, E>>,
    R: Future<Output = L::Output>,
{
    type Output = L::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut left_errored = false;

        // Check if the left future is ready & successful. Continue if not.
        let mut left = this.left;
        if Future::poll(Pin::new(&mut left), cx).is_ready() {
            if left.as_ref().output().unwrap().is_ok() {
                return Poll::Ready(left.take().unwrap());
            } else {
                left_errored = true;
            }
        }

        // Check if the right future is ready & successful. Return err if left
        // future also resolved to err. Continue if not.
        let mut right = this.right;
        if Future::poll(Pin::new(&mut right), cx).is_ready() {
            if right.as_ref().output().unwrap().is_ok() {
                return Poll::Ready(right.take().unwrap());
            } else if left_errored {
                return Poll::Ready(right.take().unwrap());
            }
        }

        Poll::Pending
    }
}
