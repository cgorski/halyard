This crate makes it easier to write asynchronous code that is executor-agnostic, by providing a
utility that can be used to spawn tasks in a variety of executors.

It only supports single executor per program, but that executor can be set at runtime, anywhere
in your crate (or an application that depends on it).

Two executors are built in: Tokio (the `tokio` feature, for the server) and
`wasm-bindgen-futures` (the `wasm-bindgen` feature, for the browser). Any other executor or
runtime that supports spawning [`Future`]s can be plugged in by implementing `CustomExecutor`.

This is a least common denominator implementation in many ways. Limitations include:

- setting an executor is a one-time, global action
- no "join handle" or other result is returned from the spawn
- the `Future` must output `()`

Spawning never panics: a task that cannot be spawned (for example, before any executor is
set) is dropped without running, and the first one dropped for each reason is logged.

```rust
use halyard_any_spawner::{Executor, ExecutorError};

match Executor::init_tokio() {
    // `AlreadySet`: an executor was set before, and it stays
    Ok(()) | Err(ExecutorError::AlreadySet) => {}
    Err(error) => eprintln!("no executor: {error}"),
}

// spawn a thread-safe Future (with Tokio: from inside the runtime)
Executor::spawn(async { /* ... */ });

// spawn a Future that is !Send (with Tokio: inside a `LocalSet`)
Executor::spawn_local(async { /* ... */ });
```
