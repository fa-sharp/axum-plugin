# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/fa-sharp/axum-plugin/releases/tag/axum-plugin-v0.3.0) - 2026-11-17

### Other

- add release workflow
- add license and repo url
- add macros feature
- Update Cargo.toml
- rename config feature
- Update README.md
- update docs
- Update README.md
- local_setup does't need router parameter, can return own router
- update docs
- clear separation of local and global setup
- add local on_setup
- add middleware hook
- clippy
- implement convenient trait for on_request and on_response returns
- add on_request and on_response hooks
- load config tweak
- store() returns result for clarity , update docs
- tweak shutdown typing for clarity
- auto insert config into state on init
- final touches
- add axum extractor for config
- update docs
- re-org
- add structured config init and access
- doc tweak
- Update lib.rs
- Update ci.yml
- fix action
- change name, bump version
- cargo update
- update examples
- add plugin naming
- API improvements
- Update ci.yml
- enforce frozen lockfile
- update rust version
- Update ci.yml
- add CI
- add comments to example
- bump version
- add example
- Update README.md
- add docs
- Run shutdown hooks sequentially in reverse
- AdHocPlugin on_init and on_setup accept capturing closures
- bump version
- add AppState derive macro for convenient state extraction
- don't add state to router in init()
- add library from private project
- first commit
