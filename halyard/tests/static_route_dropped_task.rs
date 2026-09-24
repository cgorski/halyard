//! `ResolvedStaticPath::build` waits for its render task's answer; a task that ended without
//! one (dropped, or panicked in the render function) used to be unwrapped. Nothing is written,
//! and the server answers as for a missing page.
//!
//! A test binary of its own: it sets a global executor that drops every task.

use halyard::{
    reactive::{
        executor::{CustomExecutor, Executor, PinnedFuture, PinnedLocalFuture},
        owner::Owner,
    },
    router::static_routes::ResolvedStaticPath,
};

/// Drops every task, as an executor that is shutting down does.
struct DropsTasks;

impl CustomExecutor for DropsTasks {
    fn spawn(&self, _fut: PinnedFuture<()>) {}

    fn spawn_local(&self, _fut: PinnedLocalFuture<()>) {}

    fn poll_local(&self) {}
}

#[test]
fn static_route_whose_render_task_is_dropped_writes_nothing() {
    _ = Executor::init_custom_executor(DropsTasks);
    let (_owner, html) =
        futures::executor::block_on(ResolvedStaticPath::new("/posts/1").build(
            |_: &ResolvedStaticPath| async { (Owner::new(), String::new()) },
            |_: &ResolvedStaticPath, _: &Owner, _: String| async {
                Ok::<(), std::io::Error>(())
            },
            |_: &Owner| false,
            Vec::new(),
        ));
    assert_eq!(html, None);
}
