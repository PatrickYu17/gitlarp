//! The platform seam. Core is runtime-agnostic: it never touches an
//! HTTP client, a timer, or an OS directly. Shells inject one trait,
//! `Runtime`, and get the whole engine.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::Error;

pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub trait Runtime: Send + Sync {
    fn fetch(&self, req: HttpRequest) -> BoxFut<Result<HttpResponse, Error>>;
    fn sleep(&self, ms: u64) -> BoxFut<()>;
    fn random(&self, buf: &mut [u8]);
}

/// Minimal executor for shells whose Runtime is fully synchronous
/// (e.g. the CLI's gh-subprocess transport): every leaf future it
/// returns is already resolved, so a spin-poll is enough. If a
/// Runtime ever returns genuinely pending futures, swap this for a
/// real executor before using it there.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
