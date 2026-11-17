# axum-plugin

A small plugin layer for `axum` applications, inspired by plugin-based frameworks like [rocket](https://rocket.rs/) and [fastify](https://fastify.dev/). Plugins let you conveniently separate concerns and organize your server setup and teardown into plugins.

## Example

See the `examples` folder for full examples.

```rust,no_run
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
    // Create your plugins
    let my_plugin = AdHocPlugin::<AppState, AppConfig>::new()
        .on_init(async |mut app| {
            app.insert(Arc::new(String::from("foo_state")))?;
            Ok(app)
        })
        .global_setup(|app, router| {
            let my_extension = Arc::new(app.config().bar.to_owned());
            Ok(router.layer(axum::Extension(my_extension)))
        });

    // Create or load your configuration
    let config = AppConfig { bar: String::from("bar") };

    // Register the plugins and initialize the app
    let app = App::<AppState, AppConfig>::with_config(config)
        .register(my_plugin)
        .init()
        .await?;

    // Start server
    let addr: std::net::SocketAddr = "127.0.0.1:3000".parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.router())
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
            app.shutdown().await.expect("failed to shut down");
        })
        .await?;

    Ok(())
}
```

## Creating plugins

The simplest way to create a plugin is to use the `AdHocPlugin` helper. The following hooks are available:

- `on_init`: runs during app initialization, allowing state to be built incrementally
- `local_setup`: runs after state is built, allowing a plugin to build its own router
- `global_setup`: runs after the plugin's local router is mounted, allowing the plugin to transform the accumulated app router
- `on_shutdown`: runs during graceful shutdown, in reverse plugin registration order

Use `App::register` to register the plugin, or use `App::register_at(plugin, "/prefix")` to mount the plugin's local routes under a prefix.

For more complex plugins that need their own state, implement the `AppPlugin` trait directly.

## Config extraction

With the `figment` feature enabled, typed config can be conveniently extracted from prefixed environment variables, TOML files, or directly from a `figment::Figment`:

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
            // access extracted config
            let foo: &str = &app.config().foo;

            Ok(app)
        }))
        .init()
        .await
        .unwrap();

    Ok(())
}
```

## Middleware Extractors

Use `AdHocPlugin::local_middleware` or `AdHocPlugin::global_middleware` when request handling needs normal axum extractors or `Next`.

```rust
use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
    routing::get,
};
use axum_plugin::AdHocPlugin;

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
}

#[derive(Clone)]
struct AppConfig {
    api_key: String,
}

async fn auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let _expected_key = &state.config.api_key;
    let _headers = headers;

    next.run(req).await
}

let plugin = AdHocPlugin::<AppState, AppConfig>::named("auth")
    .local_middleware(auth)
    .local_setup(|app| {
        let router = axum::Router::<AppState>::new()
            .route("/", get(async || "protected route"));

        Ok(router)
    });
```

## Plugin Lifecycle

### Setup

1. Application config is loaded.
2. Plugins' `on_init` hooks run in registration order, passing `AppInitContext<C>` with a `TypeMap` to build state.
3. The final app state is built using `S::try_from(TypeMap)`.
4. Each plugin's `local_setup` runs with a fresh local router.
   - The plugin's `local_middleware` hooks are applied to that local router.
   - The local router is merged into the app router, or nested under the prefix from `App::register_at`.
5. The plugin's `global_setup` and `global_middleware` hooks run against the accumulated app router.

### Request

1. Local middleware runs only for requests handled by that plugin's local router.
2. Global middleware runs for routes already present in the accumulated app router when the middleware was applied.
3. The router invokes the matched handler.

### Shutdown

1. `InitializedApp::shutdown()` runs `on_shutdown` consecutively in reverse plugin registration order, passing a `AppContext<S, C>` with access to state and config.
