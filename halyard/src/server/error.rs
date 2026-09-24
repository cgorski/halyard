//! What a resource could not do, and how it recovered (README, "Project policy": no panics,
//! ever). Each of these is logged once, where it happens, and the application carries on.

use std::{fmt, panic::Location};
use thiserror::Error;

/// A failure that a resource recovered from.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub(crate) enum ResourceError {
    /// The server could not serialize a resource's value for the page.
    #[cfg(any(feature = "ssr", test))]
    #[error(
        "resource {id} (created at {created_at}): the server could not serialize its \
         value ({reason}), so the page carries none and the browser loads it itself"
    )]
    Encode {
        id: usize,
        created_at: &'static Location<'static>,
        reason: String,
    },
    /// A resource finished loading on the server without a value (it was cleared).
    #[cfg(any(feature = "ssr", test))]
    #[error(
        "resource {id} (created at {created_at}) had no value when it finished loading \
         on the server, so the page carries none and the browser loads it itself"
    )]
    NoValue {
        id: usize,
        created_at: &'static Location<'static>,
    },
    /// The page says that the server sent no value for a resource.
    #[cfg(any(feature = "hydration", test))]
    #[error(
        "resource {id} (created at {created_at}): the server sent no value for it; \
         loading it in the browser"
    )]
    NotSent {
        id: usize,
        created_at: &'static Location<'static>,
    },
    /// A resource's data in the page is not in the form its codec reads (e.g. not base64).
    #[cfg(any(feature = "hydration", test))]
    #[error(
        "resource {id} (created at {created_at}): its data in the page is not in the \
         resource's encoding ({reason}); loading it in the browser"
    )]
    Unreadable {
        id: usize,
        created_at: &'static Location<'static>,
        reason: String,
    },
    /// A resource's data in the page could not be deserialized.
    #[cfg(any(feature = "hydration", test))]
    #[error(
        "resource {id} (created at {created_at}): its data in the page could not be \
         deserialized ({reason}); loading it in the browser"
    )]
    Decode {
        id: usize,
        created_at: &'static Location<'static>,
        reason: String,
    },
    /// A local resource was awaited on the server outside `<Suspense/>`/`<Transition/>`.
    #[error(
        "a local resource{} was awaited at {awaited_at} on the server, outside \
         <Suspense/> or <Transition/>. Local resources load only in the browser, so on \
         the server this await never finishes, and neither does whatever waits for it. \
         Read it under <Suspense/> or <Transition/>: on the server they render their \
         fallback, and the browser renders the rest",
        created(.created_at)
    )]
    LocalResourceAwaitedOnServer {
        awaited_at: &'static Location<'static>,
        created_at: Option<&'static Location<'static>>,
    },
    /// A resource was used after its reactive owner was disposed, so its value is gone.
    #[error(
        "a resource{} was used at {used_at} after its reactive owner was disposed: {what}",
        created(.created_at)
    )]
    Disposed {
        what: DisposedUse,
        used_at: &'static Location<'static>,
        created_at: Option<&'static Location<'static>>,
    },
}

/// What was done with a resource whose value was gone, and what happens instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposedUse {
    /// `.await` (or `by_ref().await`).
    Await,
    /// `ready().await`.
    Ready,
    /// Subscribing to it as a reactive source.
    Subscribe,
}

impl fmt::Display for DisposedUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Await => "awaiting it never finishes",
            Self::Ready => "it is never ready",
            Self::Subscribe => "as a reactive source it never changes",
        })
    }
}

/// Logs that a resource created at `created_at` was used at `used_at` after its owner was
/// disposed, and what happens instead.
pub(crate) fn warn_disposed(
    what: DisposedUse,
    used_at: &'static Location<'static>,
    created_at: Option<&'static Location<'static>>,
) {
    warn(&ResourceError::Disposed {
        what,
        used_at,
        created_at,
    });
}

/// ` (created at file:line:column)`, where that is known (in debug builds).
fn created(created_at: &Option<&'static Location<'static>>) -> String {
    created_at
        .map(|at| format!(" (created at {at})"))
        .unwrap_or_default()
}

/// Logs a failure that a resource recovered from: with `tracing` when that feature is on,
/// otherwise in the browser's console, or on standard error on the server.
pub(crate) fn warn(error: &ResourceError) {
    #[cfg(feature = "tracing")]
    tracing::warn!("{error}");
    #[cfg(all(
        not(feature = "tracing"),
        target_arch = "wasm32",
        target_os = "unknown"
    ))]
    halyard_reactive_graph::log_warning(format_args!("[halyard] {error}"));
    #[cfg(all(
        not(feature = "tracing"),
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    {
        use std::io::Write;
        // `eprintln!` panics if standard error is closed; with nowhere left to report the
        // error, it is dropped
        _ = writeln!(std::io::stderr(), "[halyard] {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log names where the resource was created and what happens instead, and says
    /// nothing about a creation site it does not know (release builds).
    #[test]
    fn messages_say_where_and_what_happens_instead() {
        let here = Location::caller();
        let disposed = ResourceError::Disposed {
            what: DisposedUse::Await,
            used_at: here,
            created_at: Some(here),
        }
        .to_string();
        assert_eq!(
            disposed,
            format!(
                "a resource (created at {here}) was used at {here} after its \
                 reactive owner was disposed: awaiting it never finishes"
            )
        );

        let local = ResourceError::LocalResourceAwaitedOnServer {
            awaited_at: here,
            created_at: None,
        }
        .to_string();
        assert!(
            local.starts_with(&format!(
                "a local resource was awaited at {here} on the server"
            )),
            "{local}"
        );
    }
}
