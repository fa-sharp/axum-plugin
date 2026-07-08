# axum-plugin

A small plugin layer for `axum` applications, inspired by [fastify](https://fastify.dev/) from the Node/JS ecosystem and the [rocket](https://rocket.rs/) framework.

Plugins can:

- read typed application config during every lifecycle hook
- add startup state before the final axum state type exists
- add routes, services, and middleware after state is initialized
- run graceful shutdown work in reverse registration order

This crate is intentionally thin. It does not replace axum's router, extractors, or middleware. Rather, it organizes your server setup and teardown into plugins.

## Lifecycle

`App::init()` runs plugins in this order:

1. Config is loaded with `App::with_config(config)` or `App::load_config_with(...)`.
2. `on_init` runs in registration order, passing `InitApp<C>` with config plus a `TypeMap` to build state.
3. `S::try_from(TypeMap)` builds the final typed app state.
4. `on_setup` runs in registration order, passing `SetupApp<S, C>` with router, state, and config.
5. `InitializedApp::shutdown()` runs `on_shutdown` consecutively in reverse registration order with `ShutdownApp<S, C>`.


## Example
See the `examples` folder for full examples.

`App::init()` returns an initialized app handle. Use `router()` to get a ready-to-serve `Router<()>`, `state()` to inspect the finalized state, `config()` to inspect the app config, and `shutdown()` from your graceful shutdown path:

```rust
let app = App::<AppState, AppConfig>::load_config_with(load_config)?
    .register(metrics_plugin)
    .init()
    .await?;

let router = app.router();
let service_name = &app.config().service_name;

axum::serve(listener, router)
    .with_graceful_shutdown(async move {
        tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
        app.shutdown().await.expect("failed to shut down");
    })
    .await?;
```

With the `figment` feature enabled, config can be extracted directly from a `figment::Figment`:

```rust
let figment = figment::Figment::from(figment::providers::Serialized::defaults(AppConfig::default()))
    .merge(figment::providers::Toml::file("config.toml"))
    .merge(figment::providers::Env::prefixed("APP_").split("__"));

let app = App::<AppState, AppConfig>::from_figment(figment)?
    .register(metrics_plugin)
    .init()
    .await?;
```

For `on_setup` closures that access typed state, construct the plugin as
`AdHocPlugin::<AppState, AppConfig>::new()`. That gives Rust enough context to infer the lifecycle context type:

```rust
let plugin = AdHocPlugin::<AppState, AppConfig>::new()
    .on_setup(|app| {
        let metrics = Arc::clone(&app.state().metrics);
        Ok(app.into_router().layer(axum::Extension(metrics)))
    });
```

## Reusable Plugins

Implement `AppPlugin<S, C>` directly when setup should be reusable across apps:

```rust
use axum::Router;
use axum_plugin::{AppPlugin, InitApp, InitFuture, Result, SetupApp};
use futures::FutureExt;

struct ConfigPlugin {
    config: Config,
}

impl AppPlugin<AppState, AppConfig> for ConfigPlugin {
    fn on_init(&mut self, mut app: InitApp<AppConfig>) -> InitFuture<AppConfig> {
        let config = self.config.clone();
        async move {
            app.insert(config);
            Ok(app)
        }
        .boxed()
    }

    fn on_setup(
        &mut self,
        app: SetupApp<AppState, AppConfig>,
    ) -> Result<Router<AppState>> {
        Ok(app.into_router().route("/health", axum::routing::get(health)))
    }
}
```
