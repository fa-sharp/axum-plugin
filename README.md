# axum-plugin

A small plugin layer for `axum` applications, inspired by plugin-based frameworks like [rocket](https://rocket.rs/) and [fastify](https://fastify.dev/).

Plugins can:

- read typed application config during every lifecycle hook
- incrementally build axum state during initialization
- add routes, services, and middleware after state is initialized
- run graceful shutdown work in reverse registration order

This crate is intentionally thin. It does not replace axum's router, extractors, or middleware. Rather, it organizes your server setup and teardown into plugins.


## Examples
See the `examples` folder for full examples.

`App::init()` returns an initialized app handle. Use `router()` to get a ready-to-serve `Router<()>`, `state()` to inspect the finalized state, `config()` to inspect the app config, and `shutdown()` from your graceful shutdown path:

```rust
use std::sync::Arc;

use axum_plugin::{AdHocPlugin, App, AppState, Result};

#[derive(AppState, Clone)]
struct AppState {
    foo: String,
}
struct AppConfig {
    bar: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let my_plugin = AdHocPlugin::<AppState, AppConfig>::new()
        .on_init(async |mut app| {
            app.insert(String::from("foo_state"))?;
            Ok(app)
        })
        .on_setup(|app, router| {
            let bar_extension = Arc::new(app.config().bar.to_owned());
            Ok(router.layer(axum::Extension(bar_extension)))
        });
    
    let app = App::<AppState, AppConfig>::with_config(AppConfig { bar: String::from("bar") })
        .register(my_plugin)
        .init()
        .await?;

    // // Start server:
    // let addr: std::net::SocketAddr = "127.0.0.1:3000".parse()?;
    // let listener = tokio::net::TcpListener::bind(addr).await?;
    // axum::serve(listener, app.router())
    //     .with_graceful_shutdown(async move {
    //         tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
    //         app.shutdown().await.expect("failed to shut down");
    //     })
    //     .await?;
    
    Ok(())
}
```

## Lifecycle

1. Application config is loaded.
2. Plugins' `on_init` hooks run in registration order, passing `InitApp<C>` with config plus a `TypeMap` to build state.
3. The final app state is built using `S::try_from(TypeMap)`.
4. `on_setup` runs in registration order, passing `SetupApp<S, C>` and the axum router.
5. `InitializedApp::shutdown()` runs `on_shutdown` consecutively in reverse registration order with `ShutdownApp<S, C>`.

## Config extraction

With the `figment` feature enabled, config can be conveniently extracted from files, environment variables, or directly from a `figment::Figment`:

```rust
use axum_plugin::{App, Result, TypeMapState};
use serde::{Serialize, Deserialize};

#[derive(Default, Serialize, Deserialize)]
struct AppConfig {
    foo: String,
}

pub fn main() -> Result<()> {
    // Load environment variables prefixed with `APP_`, e.g. `APP_FOO`
    let app = App::<TypeMapState, AppConfig>::from_env("APP_")?;
    
    Ok(())
}
```
