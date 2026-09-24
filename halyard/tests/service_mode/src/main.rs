#[cfg(feature = "ssr")]
mod router {
    use axum::{
        Router,
        http::{HeaderName, HeaderValue},
    };
    use clap::{Parser, Subcommand};
    use halyard::prelude::{get_configuration, provide_context, use_context};
    use halyard::axum::{ErrorHandler, HalyardRoutes, generate_route_list};
    use service_mode::app::{App, shell};

    #[derive(Parser)]
    pub struct Cli {
        #[command(subcommand)]
        mode: Mode,
    }

    #[derive(Subcommand)]
    enum Mode {
        Bare,
        Fallback,
        FallbackWithContext,
        ErrorHandlerService,
        ErrorHandlerServiceFallback,
        RouteSitePkgNoFallback,

        HalyardOptionsCssBase,
    }

    impl From<Cli> for Router {
        fn from(cli: Cli) -> Self {
            let conf = get_configuration(None).unwrap();
            let halyard_options = conf.halyard_options;
            let routes = generate_route_list(App);

            match cli.mode {
                Mode::Bare => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .with_state(halyard_options),
                Mode::Fallback => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .fallback(halyard::axum::file_and_error_handler(shell))
                    .with_state(halyard_options),
                Mode::FallbackWithContext => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .fallback(halyard::axum::file_and_error_handler_with_context(
                        move || {
                            let opts =
                                use_context::<halyard::axum::ResponseOptions>()
                                    .unwrap_or_default();
                            opts.insert_header(
                                HeaderName::from_static(
                                    "cross-origin-opener-policy",
                                ),
                                HeaderValue::from_static("same-origin"),
                            );
                            opts.insert_header(
                                HeaderName::from_static(
                                    "cross-origin-embedder-policy",
                                ),
                                HeaderValue::from_static("require-corp"),
                            );
                            provide_context(opts);
                        },
                        shell,
                    ))
                    .with_state(halyard_options),
                Mode::ErrorHandlerService => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .fallback_service(ErrorHandler::new(
                        shell,
                        halyard_options.clone(),
                    ))
                    .with_state(halyard_options),
                Mode::ErrorHandlerServiceFallback => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .fallback_service(
                        halyard::axum::site_pkg_dir_service(&halyard_options)
                            .fallback(ErrorHandler::new(
                                shell,
                                halyard_options.clone(),
                            )),
                    )
                    .with_state(halyard_options),
                Mode::RouteSitePkgNoFallback => Router::new()
                    .halyard_routes(&halyard_options, routes, {
                        let halyard_options = halyard_options.clone();
                        move || shell(halyard_options.clone())
                    })
                    .route_service(
                        &halyard::axum::site_pkg_dir_service_route_path(
                            &halyard_options,
                        ),
                        halyard::axum::site_pkg_dir_service(&halyard_options),
                    )
                    .fallback_service(ErrorHandler::new(
                        shell,
                        halyard_options.clone(),
                    ))
                    .with_state(halyard_options),

                Mode::HalyardOptionsCssBase => Router::new().nest(
                    &halyard_options.css_path(),
                    Router::new().route_service(
                        "/",
                        tower_http::services::ServeFile::new(
                            &halyard_options.css_file_path(),
                        ),
                    ),
                ),
            }
        }
    }
}

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use clap::Parser;
    use halyard::prelude::get_configuration;

    let app = Router::from(router::Cli::parse());
    let conf = get_configuration(None).unwrap();
    let addr = conf.halyard_options.site_addr;
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    // write out the port from the bounded local_addr to allow the caller to know how to connect.
    println!("{}", listener.local_addr().unwrap().port());
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {}
