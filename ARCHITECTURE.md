# Architecture

The goal of this document is to make it easier for contributors (and anyone
who’s interested!) to understand the architecture of the framework.

> halyard is a fork of Leptos (see `NOTICE`). This document began as upstream's
> architecture overview. The crate layout is halyard's own: four crates, layered
> bottom-up, and two vendored ones.
>
> ```text
> halyard_reactive_graph   signals, effects, owners; executor, hydration_context, throw_error
>         ^
> halyard_tachys           the renderer (views, DOM, HTML, hydration); either, oco
>         ^
> halyard                  components, hydration, router, meta, server (resources),
>         |                config, dom, axum (feature)
>         +-- halyard_macro (proc macros) <- halyard_rstml <- halyard_syn_derive
> ```
>
> Upstream's `leptos_reactive` is today the `reactive_graph` crate
> (`halyard_reactive_graph`) and the renderer is `tachys` (`halyard_tachys`). Code that was
> upstream's separate crates now sits in the lowest crate that needs it: the executor
> (`any_spawner`), `hydration_context`, `throw_error` and `or_poisoned` in the reactive
> graph; `either_of`, `oco_ref`, `next_tuple` and `const_str_slice_concat` in tachys; the
> router, meta, the server integration, resources, configuration and DOM helpers in
> `halyard`; the router's macros in `halyard_macro`. The macros' generated code refers only
> to paths under `::halyard`, so an application depends on `halyard` alone.
>
> halyard supports one way to build a site: the server (`ssr`, served by `halyard::axum`
> with the `axum` feature) renders the page, and the browser build (`hydrate`) hydrates it
> with WebAssembly, optionally as islands. There is no client-side-only mode. Async tasks
> run on Tokio on the server and on wasm-bindgen-futures in the browser (both through
> `halyard_reactive_graph::executor`, which also accepts a custom executor), resources
> travel with the page as serde JSON, and everything builds on stable Rust.

The whole Halyard framework is built from a series of layers. Each of these layers
depends on the one below it, but each can be used independently from the ones
built on top of it. While running a command like `cargo halyard new --git
leptos-rs/start` pulls in the whole framework, it’s important to remember that
none of this is magic: each layer of that onion can be stripped away and
reimplemented, configured, or adapted as needed, incrementally.

> Everything that follows will assume you have a good working understanding
> of the framework. There will be explanations of how some parts of it work
> or fit together, but these are not docs. They assume you know what I’m
> talking about.

## The Reactive System: `halyard_reactive_graph`

The reactive system allows you to define dynamic values (signals),
the relationships between them (derived signals and memos), and the side effects
that run in response to them (effects).

These concepts are completely independent of the DOM and can be used to drive
any kind of reactive updates. The reactive system is based on the assumption
that data is relatively cheap, and side effects are relatively expensive. Its
goal is to minimize those side effects (like updating the DOM or making a network
requests) as infrequently as possible.

The reactive system is implemented as a single data structure that exists at
runtime. In exchange for giving ownership over a value to the reactive system
(by creating a signal), you receive a `Copy + 'static` identifier for its
location in the reactive system. This enables most of the ergonomics of storing
and sharing state, the use of callback closures without lifetime issues, etc.
This is implemented by storing signals in a slotmap arena. The signal, memo,
and scope types that are exposed to users simply carry around an index into that
slotmap.

> Items owned by the reactive system are dropped when the corresponding reactive
> scope is dropped, i.e., when the component or section of the UI they’re
> created in is removed. In a sense, Halyard implements a “garbage collector”
> in which the lifetime of data is tied to the lifetime of the UI, not Rust’s
> lexical scopes.

## The Renderer: `halyard_tachys` (and `halyard::dom`)

The reactive system can be used to drive any kinds of side effects. One very
common side effect is calling an imperative method, for example to update the
DOM.

The entire DOM renderer is built on top of the reactive system. It provides
a builder pattern that can be used to create DOM elements dynamically.

The renderer assumes, as a convention, that dynamic attributes, classes,
styles, and children are defined by being passed a `Fn() -> T`, where their
static equivalents just receive `T`. There’s nothing about this that is
divinely ordained, but it’s a useful convention because it allows us to use
zero-overhead derived signals as one of several ways to indicate dynamic
content.

`halyard_tachys` also contains the code for server-side rendering of the same
UI views to HTML, in out-of-order or in-order streams (`src/ssr/`). `halyard::dom` is
a thin layer of browser helpers (`window()`, `document()`, event listeners, timers).

## The Macros: `halyard_macro`

It’s entirely possible to write Halyard code with no macros at all. The
`view` and `component` macros, the most common, can be replaced by
the builder syntax and simple functions (see the `counter_without_macros`
example). But the macros enable a JSX-like syntax for describing views.

This crate also contains the router's macros (`path!`, `#[lazy_route]`) and the `Params`
derive macro used for typed queries and route params, and the small diagnostics module
that turns macro errors into `compile_error!`s. A proc-macro crate can only export macros,
so its generated code names everything it needs under `::halyard` (`::halyard::tachys`,
`::halyard::reactive`, `::halyard::router`, ...).

### Macro-based Optimizations

Halyard 0.0.x was built much more heavily on macros. Taking its cues  
from SolidJS, the `view` macro emitted different code for CSR, SSR, and
hydration, optimizing each. The CSR/hydrate versions worked by compiling
the view to an HTML template string, cloning that `<template>`, and
traversing the DOM to set up reactivity. The SSR version worked similarly
by compiling the static parts of the view to strings at compile time,
reducing the amount of work that needed to be done on each request.

Proc macros are hard, and this system was brittle. 0.1 introduced a
more robust renderer, including the builder syntax, and rebuilt the `view`
macro to use that builder syntax instead. It moved the optimized-but-buggy
CSR version of the macro to a more-limited `template` macro.

The `view` macro now separately optimizes SSR to use the same static-string
optimizations, which (by our benchmarks) makes Halyard about 3-4x faster
than similar Rust frontend frameworks in its HTML rendering.

> The optimization is pretty straightforward. Consider the following view:
>
> ```rust
> view! { cx,
>   <main class="text-center">
>     <div class="flex-col">
>       <button>"Click me."</button>
>       <p class="italic">"Text."</p>
>     </div>
>   </main>
> }
> ```
>
> Internally, with the builder this is something like
>
> ```rust
> Element {
>   tag: "main",
>   attrs: vec![("class", "text-center")],
>   children: vec![
> 	  Element {
> 		tag: "div",
> 		attrs: vec![("class", "flex-col")],
>       children: vec![
>         Element {
> 	        tag: "button",
> 			attrs: vec![],
> 			children: vec!["Click me"]
>         },
>         Element {
> 	        tag: "p",
> 			attrs: vec![("class", "italic")],
> 			children: vec!["Text"]
>         }
>       ]
> 	  }
>   ]
> }
> ```
>
> This is a _bunch_ of small allocations and separate strings,
> and in early 0.1 versions we used a `SmallVec` for children and
> attributes and actually caused some stack overflows.
>
> But if you look at the view itself you can see that none of this
> will _ever_ change. So we can actually optimize it at compile
> time to a single `&'static str`:
>
> ```rust
> r#"<main class="text-center">
>     <div class="flex-col">
>       <button>"Click me."</button>
>       <p class="italic">"Text."</p>
>     </div>
>   </main>"#
> ```

## `halyard`

This crate is built on the layers already mentioned and re-exports them
(`halyard::reactive`, `halyard::tachys`). It implements the control-flow components
(`<Show/>`, `<ErrorBoundary/>`, `<For/>`, `<Suspense/>`, `<Transition/>`), mounting and
hydration, and the parts of the framework that were upstream's separate crates, each a
module:

- **`halyard::server`**: the resources (`Resource`, `OnceResource`, `LocalResource`) and
  `SharedValue`: data loaded on the server while the page renders and sent to the browser
  with the page, as serde JSON, so that hydration starts from the same data without loading
  it again. halyard has no server functions (upstream's `server_fn` crates were removed):
  an application that needs an HTTP API routes it in axum next to `halyard::axum`'s routes.
- **`halyard::meta`**: tags normally found in the `<head>`, set from within components.
  It stays a module of its own (rather than part of the renderer) on the principle that
  “what can be implemented in userland, should be.”
- **`halyard::router`**: the router originates as a direct port of `solid-router`, which is
  the origin of most of its terminology, architecture, and route-matching logic. Subsequent
  developments (like animated routing, and managing route transitions given the lack of
  `useTransition` in Halyard) have caused it to diverge slightly from Solid’s exact code, but
  it is still very closely related. The core principle here is “nested routing,” dividing a
  single page into independently-rendered parts. Its macros (`path!`, `#[lazy_route]`) are
  in `halyard_macro`.
- **`halyard::axum`** (feature `axum`): the server integration, the most “frameworky” layer
  of the whole framework. It draws routing data from the router, and injects the metadata
  from `meta` into the `<head>` appropriately. It saves applications from including a huge
  amount of boilerplate to connect the other parts correctly. What it shares with any other
  server integration (building a page's response) is a private module,
  `integration_utils`.
- **`halyard::config`** and **`halyard::dom`**: the runtime configuration and the browser
  helpers.

## `cargo-halyard` helpers

`halyard::config` exists to support a feature of `cargo-halyard`, namely its
configuration, which the build tool and the server read alike.

It’s important to say that the main feature `cargo-halyard` remains its ability
to conveniently tie together different build tooling, compiling your app to
WASM for the browser, building the server version, pulling in SASS and
Tailwind, etc. It is an extremely good build tool, not a magic formula. Each
of the examples includes instructions for how to run the examples without
`cargo-halyard`.
