# axum-app-wrapper

A small plugin layer for `axum` applications, inspired by [fastify](https://fastify.dev/) from the Node/JS ecossytem.

Plugins can:

- add startup state before the final axum state type exists
- add routes, services, and middleware after state is initialized
- run graceful shutdown work in reverse registration order

This crate is intentionally thin. It does not replace axum's router, extractors, or middleware. Rather, it organizes your server setup and teardown into plugins.

## Lifecycle

`App::init()` runs plugins in this order:

1. `on_init` in registration order, passing a `TypeMap` to build state.
2. `S::try_from(TypeMap)` to build the final typed app state.
3. `on_setup` in registration order, passing `Router<S>` and `&S`.
4. `on_shutdown` consecutively in reverse registration order

## Example

```rust
use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::State,
    routing::get,
};
use axum_app_wrapper::{AdHocPlugin, App, AppState};

#[derive(Clone, AppState)]
struct AppState {
    config: Arc<Config>,
}

#[derive(Clone)]
struct Config {
    service_name: String,
}

async fn health(State(state): State<AppState>) -> String {
    format!("{}:ok", state.config.service_name)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Arc::new(Config {
        service_name: "api".to_owned(),
    });

    let app = App::<AppState>::new().register(
        AdHocPlugin::<AppState>::new()
            .on_init(async move |mut state| {
                state.insert(config);
                Ok(state)
            })
            .on_setup(|router, _state| Ok(router.route("/health", get(health))))
            .on_shutdown(|state| {
                let service_name = state.config.service_name.clone();
                async move {
                    tracing::info!(%service_name, "shutting down");
                }
            }),
    );

    let (router, state, on_shutdown) = app.init().await?;
    let router = router.with_state(state);

    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
            on_shutdown.await;
        })
        .await?;

    Ok(())
}
```

For `on_setup` closures that access typed state, construct the plugin as
`AdHocPlugin::<AppState>::new()`. That gives Rust enough context to infer the `state` parameter:

```rust
let plugin = AdHocPlugin::<AppState>::new()
    .on_setup(|router, state| {
        let config = Arc::clone(&state.config);
        Ok(router.layer(axum::Extension(config)))
    });
```

## Reusable Plugins

Implement `AppPlugin<S>` directly when setup should be reusable across apps:

```rust
use axum::Router;
use axum_app_wrapper::{AppPlugin, TypeMap};
use futures::{future::BoxFuture, FutureExt};

struct ConfigPlugin {
    config: Config,
}

impl AppPlugin<AppState> for ConfigPlugin {
    fn on_init(&mut self, mut state: TypeMap) -> BoxFuture<'static, anyhow::Result<TypeMap>> {
        let config = self.config.clone();
        async move {
            state.insert(config);
            Ok(state)
        }
        .boxed()
    }

    fn on_setup(
        &mut self,
        router: Router<AppState>,
        _state: &AppState,
    ) -> anyhow::Result<Router<AppState>> {
        Ok(router.route("/health", axum::routing::get(health)))
    }
}
```

Shutdown hooks run like stack unwinding: the last registered plugin shuts down first, and each hook
finishes before the next one starts.
