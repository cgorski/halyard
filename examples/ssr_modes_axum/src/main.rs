#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::{
        http::{HeaderName, HeaderValue},
        Router,
    };
    use halyard::axum::{generate_route_list, HalyardRoutes};
    use halyard::{logging::log, prelude::*};
    use ssr_modes_axum::app::*;

    let conf = get_configuration(None).unwrap();
    let addr = conf.halyard_options.site_addr;
    let halyard_options = conf.halyard_options;
    // Generate the list of routes in your Halyard App
    let routes = generate_route_list(App);

    let app = Router::new()
        .halyard_routes(&halyard_options, routes, {
            let halyard_options = halyard_options.clone();
            move || shell(halyard_options.clone())
        })
        .fallback(halyard::axum::file_and_error_handler_with_context(
            move || {
                // if you want to add custom headers to the static file handler response,
                // you can do that by providing `ResponseOptions` via context
                let opts = use_context::<halyard::axum::ResponseOptions>()
                    .unwrap_or_default();
                opts.insert_header(
                    HeaderName::from_static("cross-origin-opener-policy"),
                    HeaderValue::from_static("same-origin"),
                );
                opts.insert_header(
                    HeaderName::from_static("cross-origin-embedder-policy"),
                    HeaderValue::from_static("require-corp"),
                );
                provide_context(opts);
            },
            shell,
        ))
        .with_state(halyard_options);

    // run our app with hyper
    // `axum::Server` is a re-export of `hyper::Server`
    log!("listening on http://{}", &addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {
    // no client-side main function: the browser build hydrates the
    // server-rendered page, see lib.rs for the hydration function
}
