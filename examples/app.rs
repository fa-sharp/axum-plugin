use std::{net::SocketAddr, sync::Arc};

use axum::{Extension, extract::State, routing::get};
use axum_app_wrapper::{AdHocPlugin, App, AppState};

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Arc::new(Config {
        service_name: "api".to_owned(),
    });
    let metrics_registry = Arc::new(Metrics::new());

    // Create your plugins using AdHocPlugin
    let config_plugin = AdHocPlugin::<AppState>::new()
        .on_init(async |mut state| {
            state.insert(config);
            Ok(state)
        })
        .on_setup(|router, state| {
            tracing::info!(service = %state.config.service_name, "configuring routes");
            Ok(router.route("/health", get(health)))
        });

    let metrics_plugin = AdHocPlugin::<AppState>::new()
        .on_init(async |mut state| {
            state.insert(metrics_registry);
            Ok(state)
        })
        .on_setup(|router, state| {
            Ok(router
                .route("/metrics", get(metrics_handler))
                .layer(Extension(Arc::clone(&state.metrics))))
        })
        .on_shutdown(|state| {
            let metrics = Arc::clone(&state.metrics);
            async move {
                metrics.flush().await;
            }
        });

    // Register your plugins in the desired order
    let app = App::<AppState>::new()
        .register(config_plugin)
        .register(metrics_plugin);

    let (router, state, on_shutdown) = app.init().await?;
    let router = router.with_state(state);

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Start the axum server with graceful shutdown
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
            on_shutdown.await; // Run the on_shutdown future for graceful shutdown
        })
        .await?;

    Ok(())
}
