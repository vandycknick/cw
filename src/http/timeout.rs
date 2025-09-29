/*
 * Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::error::Error;
use std::fmt::Formatter;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use http::Uri;
use pin_project_lite::pin_project;

use aws_smithy_async::future::timeout::{TimedOutError, Timeout};
use aws_smithy_async::rt::sleep::Sleep;
use aws_smithy_async::rt::sleep::{AsyncSleep, SharedAsyncSleep};
use aws_smithy_runtime_api::box_error::BoxError;

#[derive(Debug)]
pub(crate) struct HttpTimeoutError {
    kind: &'static str,
    duration: Duration,
}

impl std::fmt::Display for HttpTimeoutError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} timeout occurred after {:?}",
            self.kind, self.duration
        )
    }
}

impl Error for HttpTimeoutError {
    // We implement the `source` function as returning a `TimedOutError` because when `downcast_error`
    // or `find_source` is called with an `HttpTimeoutError` (or another error wrapping an `HttpTimeoutError`)
    // this method will be checked to determine if it's a timeout-related error.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&TimedOutError)
    }
}

/// Timeout wrapper that will timeout on the initial TCP connection
///
/// # Stability
/// This interface is unstable.
#[derive(Clone, Debug)]
pub(crate) struct ConnectTimeout<I> {
    inner: I,
    timeout: Option<(SharedAsyncSleep, Duration)>,
}

impl<I> ConnectTimeout<I> {
    /// Create a new `ConnectTimeout` around `inner`.
    ///
    /// Typically, `I` will implement [`hyper_util::client::legacy::connect::Connect`].
    pub(crate) fn new(inner: I, sleep: SharedAsyncSleep, timeout: Duration) -> Self {
        Self {
            inner,
            timeout: Some((sleep, timeout)),
        }
    }

    pub(crate) fn no_timeout(inner: I) -> Self {
        Self {
            inner,
            timeout: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HttpReadTimeout<I> {
    inner: I,
    timeout: Option<(SharedAsyncSleep, Duration)>,
}

impl<I> HttpReadTimeout<I> {
    /// Create a new `HttpReadTimeout` around `inner`.
    ///
    /// Typically, `I` will implement [`tower::Service<http::Request<SdkBody>>`].
    pub(crate) fn new(inner: I, sleep: SharedAsyncSleep, timeout: Duration) -> Self {
        Self {
            inner,
            timeout: Some((sleep, timeout)),
        }
    }

    pub(crate) fn no_timeout(inner: I) -> Self {
        Self {
            inner,
            timeout: None,
        }
    }
}

pin_project! {
    /// Timeout future for Tower services
    ///
    /// Timeout future to handle timing out, mapping errors, and the possibility of not timing out
    /// without incurring an additional allocation for each timeout layer.
    #[project = MaybeTimeoutFutureProj]
    pub enum MaybeTimeoutFuture<F> {
        Timeout {
            #[pin]
            timeout: Timeout<F, Sleep>,
            error_type: &'static str,
            duration: Duration,
        },
        NoTimeout {
            #[pin]
            future: F
        }
    }
}

impl<F, T, E> Future for MaybeTimeoutFuture<F>
where
    F: Future<Output = Result<T, E>>,
    E: Into<BoxError>,
{
    type Output = Result<T, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (timeout_future, kind, &mut duration) = match self.project() {
            MaybeTimeoutFutureProj::NoTimeout { future } => {
                return future.poll(cx).map_err(|err| err.into());
            }
            MaybeTimeoutFutureProj::Timeout {
                timeout,
                error_type,
                duration,
            } => (timeout, error_type, duration),
        };
        match timeout_future.poll(cx) {
            Poll::Ready(Ok(response)) => Poll::Ready(response.map_err(|err| err.into())),
            Poll::Ready(Err(_timeout)) => {
                Poll::Ready(Err(HttpTimeoutError { kind, duration }.into()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<I> tower::Service<Uri> for ConnectTimeout<I>
where
    I: tower::Service<Uri>,
    I::Error: Into<BoxError>,
{
    type Response = I::Response;
    type Error = BoxError;
    type Future = MaybeTimeoutFuture<I::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| err.into())
    }

    fn call(&mut self, req: Uri) -> Self::Future {
        match &self.timeout {
            Some((sleep, duration)) => {
                let sleep = sleep.sleep(*duration);
                MaybeTimeoutFuture::Timeout {
                    timeout: Timeout::new(self.inner.call(req), sleep),
                    error_type: "HTTP connect",
                    duration: *duration,
                }
            }
            None => MaybeTimeoutFuture::NoTimeout {
                future: self.inner.call(req),
            },
        }
    }
}

impl<I, B> tower::Service<http::Request<B>> for HttpReadTimeout<I>
where
    I: tower::Service<http::Request<B>>,
    I::Error: Send + Sync + Error + 'static,
{
    type Response = I::Response;
    type Error = BoxError;
    type Future = MaybeTimeoutFuture<I::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| err.into())
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        match &self.timeout {
            Some((sleep, duration)) => {
                let sleep = sleep.sleep(*duration);
                MaybeTimeoutFuture::Timeout {
                    timeout: Timeout::new(self.inner.call(req), sleep),
                    error_type: "HTTP read",
                    duration: *duration,
                }
            }
            None => MaybeTimeoutFuture::NoTimeout {
                future: self.inner.call(req),
            },
        }
    }
}
