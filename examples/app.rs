use std::{net::SocketAddr, str, sync::Arc};

use axum::{
    Extension,
    extract::{Request, State},
    middleware::Next,
    response::Response,
    routing::get,
};
use axum_plugin::{AdHocPlugin, App, AppState};

#[derive(Clone, AppState)]
struct AppState {
    config: Arc<Config>,
    metrics: Arc<Metrics>,
}

#[derive(Clone)]
struct Config {
    service_name: String,
}

struct Metrics;

impl Metrics {
    fn new() -> Self {
        Self
    }

    async fn flush(&self) {
        tracing::info!("flushed metrics");
    }
}

async fn health(State(state): State<AppState>) -> String {
    format!("{}:ok", state.config.service_name)
}

async fn metrics_handler(Extension(metrics): Extension<Arc<Metrics>>) -> &'static str {
    let _metrics = metrics;
    "metrics:ok"
}

async fn metrics_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let _metrics = &state.metrics;
    next.run(req).await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config {
        service_name: "api".to_owned(),
    };
    let metrics_registry = Arc::new(Metrics::new());

    // Create your plugins using AdHocPlugin. Can use a type alias here for ease of use.
    type Plugin = AdHocPlugin<AppState, Config>;

    let health_plugin = Plugin::named("Health").global_setup(|app, router| {
        tracing::info!(service = %app.config().service_name, "configuring routes");
        Ok(router.route("/health", get(health)))
    });

    let metrics_plugin = Plugin::named("Metrics")
        .on_init(async |mut app| {
            app.insert(metrics_registry)?;

            Ok(app)
        })
        .global_setup(|app, router| {
            let metrics = Arc::clone(&app.state().metrics);
            let router = router
                .route("/metrics", get(metrics_handler))
                .layer(Extension(metrics));

            Ok(router)
        })
        .global_middleware(metrics_middleware)
        .on_shutdown(async move |app| {
            app.state().metrics.flush().await;
            Ok(())
        });

    // Register your plugins in the desired order, and initialize the app
    let app = App::with_config(config)
        .register(health_plugin)
        .register(metrics_plugin)
        .init()
        .await?;
    tracing::info!(service = %app.config().service_name, "starting server");

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Start the axum server with graceful shutdown
    axum::serve(listener, app.router())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
            app.shutdown()
                .await
                .expect("failed to run graceful shutdown");
        })
        .await?;

    Ok(())
}
