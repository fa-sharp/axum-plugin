# axum-app-wrapper

A small plugin layer for `axum` applications, inspired by [fastify](https://fastify.dev/) from the Node/JS ecosystem.

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
4. `InitializedApp::shutdown()` runs `on_shutdown` consecutively in reverse registration order: the last registered plugin shuts down first, and each hook finishes before the next one starts.


## Example
See the `examples` folder for full examples.

`App::init()` returns an initialized app handle. Use `router()` to get a ready-to-serve `Router<()>`, `state()` to inspect the finalized state, and `shutdown()` from your graceful shutdown path:

```rust
let app = App::<AppState>::new()
    .store(config)
    .route("/health", axum::routing::get(health))
    .register(metrics_plugin)
    .init()
    .await?;

let router = app.router();
let service_name = &app.state().config.service_name;

axum::serve(listener, router)
    .with_graceful_shutdown(async move {
        tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
        app.shutdown().await.expect("failed to shut down");
    })
    .await?;
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
use axum_app_wrapper::{AppPlugin, InitFuture, Result, TypeMap};
use futures::FutureExt;

struct ConfigPlugin {
    config: Config,
}

impl AppPlugin<AppState> for ConfigPlugin {
    fn on_init(&mut self, mut state: TypeMap) -> InitFuture {
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
    ) -> Result<Router<AppState>> {
        Ok(router.route("/health", axum::routing::get(health)))
    }
}
```
