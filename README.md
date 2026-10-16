<p align="center">
  <img src="assets/pulpo.svg" width="112" alt="Pulpo logo" />
</p>

<h1 align="center">Pulpo</h1>

<p align="center">
  <strong>Point it at your repo. Your API collection writes itself.</strong><br/>
  A free, open source, multi-protocol API client that lives in git.
</p>

---

> **Status: early days.** Pulpo is being built in the open. Nothing is usable yet; star or watch the repo to follow along.

## Why Pulpo

API clients have gone the wrong way: accounts, forced cloud sync and paid team features. Pulpo goes the other way.

- **Free and open source**, forever. No account, no login, no cloud.
- **Lives in git.** Collections are plain YAML files in your repository. Collaboration means branches and pull requests, with diffs you can actually review.
- **Many arms, many protocols.** REST, GraphQL, gRPC, WebSocket, SOAP and SSE, with plugins for the rest.
- **Your collection comes from your code.** Pulpo scans controllers, routes and contracts (OpenAPI, WSDL, `.proto`, GraphQL SDL), builds the collection grouped by feature, and tells you after every `git pull` which endpoints were added, removed or changed.
- **Secrets never touch the repo.** They live in your OS keychain.
- **Small and fast.** A native desktop app for Windows, macOS and Linux, built with Tauri.

## Roadmap

| Phase | Goal |
|---|---|
| 0. Foundations | Monorepo, desktop shell, CI on Windows, macOS and Linux |
| 1. Everyday REST | HTTP requests, folders, environments, variables, keychain secrets, git-friendly files, import from Postman, OpenAPI and curl |
| 2. Scripts and CLI | Pre-request and test scripts (Postman `pm.*` compatible), `pulpo run` for CI with JUnit reports |
| 3. Collections from code | Scan OpenAPI, Spring and Express; preview; drift detection after `git pull`; automatic variable chaining |
| 4. More protocols | GraphQL, WebSocket, SSE, gRPC (reflection and `.proto`), SOAP (WSDL) |
| 5. Teams and community | Git panel, cross-protocol flows, mock server, plugins, import from Insomnia and Bruno |

## Repository layout

```
crates/pulpo-core     Collection model and engine (Rust)
crates/pulpo-cli      The `pulpo` command line runner (Rust)
apps/desktop          Desktop app: Vue 3 + TypeScript UI, Tauri 2 shell
assets                Logo and artwork
```

## Building from source

Prerequisites: [Rust](https://rustup.rs) (stable), [Node.js](https://nodejs.org) 20+ and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```bash
cd apps/desktop
npm install
npm run tauri dev
```

Run the core tests and the CLI:

```bash
cargo test --workspace
cargo run -p pulpo-api-cli -- version
```

### Without installing Rust (Docker)

Only Docker is needed. The container has Rust, Node and the Linux libraries Tauri needs.

```bash
docker compose -f docker/compose.yml build dev
docker compose -f docker/compose.yml run --rm dev bash -c "cd apps/desktop && npm ci && npm run build"
docker compose -f docker/compose.yml run --rm dev cargo test --workspace
docker compose -f docker/compose.yml run --rm dev bash -c "cd apps/desktop && npx tauri build --bundles deb"
```

- Working on the UI only? `cd apps/desktop && npm install && npm run dev` runs it in the browser, no Rust needed.
- Windows and macOS installers are built by CI on every push to `main` (see the *Installers* artifacts in the Actions tab).

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md). Every commit needs a `Signed-off-by` line (`git commit -s`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
