//! What the unit tests share: an executor for the tasks that resources spawn, and the data
//! the server streams for a page.

use halyard_any_spawner::{
    CustomExecutor, Executor, PinnedFuture, PinnedLocalFuture,
};
#[cfg(feature = "ssr")]
use halyard_hydration_context::{SharedContext, SsrSharedContext};
#[cfg(feature = "ssr")]
use halyard_reactive_graph::owner::Owner;
#[cfg(feature = "ssr")]
use std::sync::Arc;

/// Runs each spawned task to completion on a thread of its own. Local (`!Send`) tasks are
/// kept but never run (no test here needs one to finish), and never dropped, so that no
/// destructor runs on a thread that is shutting down.
struct ThreadPerTask;

impl CustomExecutor for ThreadPerTask {
    fn spawn(&self, fut: PinnedFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(fut));
    }

    fn spawn_local(&self, fut: PinnedLocalFuture<()>) {
        std::mem::forget(fut);
    }

    fn poll_local(&self) {}
}

/// Sets the executor for the test binary (the first test to call this sets it).
pub(crate) fn init_executor() {
    _ = Executor::init_custom_executor(ThreadPerTask);
}

/// A server request's root owner and shared context, as a server integration creates them.
#[cfg(feature = "ssr")]
pub(crate) fn server_request() -> (Owner, Arc<SsrSharedContext>) {
    let context = Arc::new(SsrSharedContext::new());
    let shared: Arc<dyn SharedContext + Send + Sync> = context.clone();
    (Owner::new_root(Some(shared)), context)
}

/// Everything the server's data scripts carry for the page, once every resource has
/// resolved.
#[cfg(feature = "ssr")]
pub(crate) fn page_data(context: &SsrSharedContext) -> String {
    use futures::StreamExt;

    let Some(stream) = context.pending_data() else {
        panic!("the server's shared context always has data scripts");
    };
    futures::executor::block_on(stream.collect::<String>())
}
