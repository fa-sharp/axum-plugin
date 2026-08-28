# axum-plugin

A small plugin layer for `axum` applications, inspired by plugin-based frameworks like [rocket](https://rocket.rs/) and [fastify](https://fastify.dev/). Plugins let you conveniently separate concerns and organize your server setup and teardown into plugins.

## Examples

See the `examples` folder for full examples.

```rust
use std::sync::Arc;

use axum_plugin::{AdHocPlugin, App, AppState, Result};

// Define your app state and config
#[derive(AppState, Clone)]
struct AppState {
    foo: Arc<String>,
}
struct AppConfig {
    bar: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let my_plugin = AdHocPlugin::<AppState, AppConfig>::new()
        .on_init(async |mut app| {
            app.insert(Arc::new(String::from("foo_state")))?;
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

    // Start server:
    let addr: std::net::SocketAddr = "127.0.0.1:3000".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.router())
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
            app.shutdown().await.expect("failed to shut down");
        }); // .await.unwrap();

    Ok(())
}
```

## Config extraction

With the `figment` feature enabled, typed config can be conveniently extracted from files, environment variables, or directly from a `figment::Figment`:

```rust
use axum_plugin::{AdHocPlugin, App, Result, TypeMapState};
use serde::{Serialize, Deserialize};

#[derive(Default, Serialize, Deserialize)]
struct AppConfig {
    foo: String,
    bar: String,
}

#[tokio::main]
pub async fn main() -> Result<()> {
    // Load environment variables prefixed with `APP_`, e.g. `APP_FOO`, `APP_BAR`
    let app = App::<TypeMapState, AppConfig>::from_env("APP_")?
        .register(AdHocPlugin::<_, AppConfig>::named("my_plugin").on_init(async |app| {
            // Now you can access your config
            let foo = &app.config().foo;

            Ok(app)
        }))
        .init()
        .await
        .unwrap();

    Ok(())
}
```

## Available Hooks

- `on_init`: runs during app initialization, allowing state to be built incrementally
- `on_setup`: runs after state is built, allowing you to add routes and middleware
- `on_shutdown`: runs during graceful shutdown, in reverse plugin registration order
- `on_request`: runs before every request for all previously registered routes
- `on_response`: runs after the router is invoked for all previously registered routes

## Plugin Lifecycle

### Setup

1. Application config is loaded.
2. Plugins' `on_init` hooks run in registration order, passing `InitApp<C>` with a `TypeMap` to build state.
3. The final app state is built using `S::try_from(TypeMap)`.
4. `on_setup` runs in plugin registration order, passing `SetupApp<S, C>` with access to state and config, along with the axum router.

### Request

1. `on_request` runs before every request for all previously registered routes, with access to state and config.
2. Any middleware in `on_setup` is run
3. The router is invoked
4. `on_response` runs after the router is invoked, with access to state and config.

### Shutdown

1. `InitializedApp::shutdown()` runs `on_shutdown` consecutively in reverse plugin registration order, passing a `ShutdownApp<S, C>` with access to state and config.
