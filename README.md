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

## Collections are files

A workspace is a folder with a `pulpo.yaml`. Folders are folders, requests are YAML files, and bodies live next to them so your editor highlights them and pull requests show clean diffs. See [`examples/jsonplaceholder`](examples/jsonplaceholder).

```
my-api/
├── pulpo.yaml
└── 01-posts/
    ├── _folder.yaml
    ├── 03-create-a-post.pulpo.yaml
    └── 03-create-a-post.body.json
```

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/pulpo-api/pulpo/main/schemas/request.v1.schema.json
version: 1
name: "Create a post"
protocol: "http"
method: "POST"
url: "https://jsonplaceholder.typicode.com/posts"
headers:
  - name: "Content-Type"
    value: "application/json"
body: "json"
```

Forms and uploads live in the request file too. File paths are relative to the workspace (and cannot leave it), so the collection works on every machine and a shared collection cannot upload your other files:

```yaml
body: "multipart"          # or "urlencoded"; "file" + bodyFile sends one file as the whole body
form:
  - name: "title"
    value: "{{title}}"
  - name: "photo"
    file: "files/photo.png"
```

Order lives in the `NN-` prefix. Rename, duplicate, move and reorder by dragging in the app: files are renamed with their body and scripts, and Pulpo keeps every number it can, so a move shows up in git as a few renames.

Pulpo always writes files in one canonical form. The [JSON Schema](schemas/request.v1.schema.json) gives you autocompletion in VS Code and IntelliJ.

```bash
pulpo init my-api --name "My API"   # create a workspace
pulpo ls my-api                     # print the tree
pulpo fmt my-api --check            # fail CI if a file is not canonical
pulpo send GET https://example.com  # send a one-off request
```

## Environments and secrets

Environments live in `environments/<name>.yaml`. Plain values are committed; secrets are only *declared* there and their values stay in your OS keychain (Windows Credential Manager, macOS Keychain, Secret Service on Linux).

```yaml
version: 1
name: "Local"
variables:
  - name: "base"
    value: "http://localhost:8888"
  - name: "token"
    secret: true
```

Use them anywhere as `{{base}}`, plus dynamic values: `{{$guid}}`, `{{$timestamp}}`, `{{$isoTimestamp}}`, `{{$randomInt}}`.

```bash
pulpo env my-api                              # list environments (secrets masked)
pulpo secret set my-api local token           # store a secret, read from stdin
pulpo send GET "{{base}}/health" -w my-api -e local
PULPO_SECRET_TOKEN=... pulpo send ...         # in CI, secrets come from env vars
```

## Auth

Set auth once on a folder (`_folder.yaml`) or the whole workspace (`pulpo.yaml`) and every request inside inherits it. A request or subfolder can set its own, or `type: "none"` to send nothing. Supported: `bearer`, `basic` and `apiKey` (in a header or the query string).

```yaml
version: 1
name: "Orders"
auth:
  type: "bearer"
  token: "{{token}}"
```

Folders and the workspace can also set **common headers** (`headers:`), which Postman lacks: every request inside sends them, a deeper folder replaces one with the same name, a request can override it, or stop it with a disabled header of the same name. In the app, right-click a folder (or ⚙ next to the collection name) to edit its headers, auth, settings and scripts.

Settings inherit the same way, field by field: `settings:` with `timeoutMs`, `verifySsl`, `followRedirects` and `proxy` (`system`, `none` or a URL) on a request, a folder or the workspace. Client certificates (mTLS, PEM or PFX/P12) are set per host on each machine in `.pulpo/certificates.json`, never in git; a PFX password is written as a `{{secret}}` variable so it lives in the OS keychain.

Cookies work like in a browser: each environment has its own jar in `.pulpo/cookies.json` (local, never committed). Cookies set by responses, including redirects and `pm.sendRequest` logins, go back to the same domain on later requests, and scripts read them with `pm.cookies.get(name)`. Turn the jar off for one request with `cookies: false`.

Keep the secret parts as `{{variables}}` so the file can be committed; the app warns when a token or password is typed literally. A header written by hand with the same name wins over the auth.

## Scripts and tests

Pre-request and test scripts are JavaScript files next to the request, with Postman's `pm.*` API, so imported collections keep working:

```
01-orders/
├── _folder.pre.js              runs before every request in the folder (and subfolders)
├── 01-create-order.pulpo.yaml
├── 01-create-order.pre.js
└── 01-create-order.test.js
```

```js
pm.test("created", () => pm.response.to.have.status(201));
pm.environment.set("orderId", pm.response.json().id);   // used later as {{orderId}}
```

Values set by scripts stay on your machine in `.pulpo/` (which ignores itself in git), so tokens never end up in a commit. Scripts run in a sandboxed QuickJS engine with time and memory limits.

Also supported: `pm.sendRequest` (for logins and setup calls, with or without a callback), `CryptoJS` (`SHA256`, `SHA1`, `SHA512`, `MD5` and their `Hmac*`, with `enc.Hex`, `enc.Base64`, `enc.Utf8`; also via `require('crypto-js')`), `atob` and `btoa`.

```bash
pulpo run -w my-api -e local              # run every request in order, fail on a failed test
pulpo run 01-orders -w my-api -e local --bail
pulpo run -w my-api -e ci --junit report.xml   # for GitHub Actions, GitLab, Jenkins
```

## Import what you already have

```bash
pulpo import my-collection.postman_collection.json my-api   # folders, auth, bodies, variables
pulpo import local.postman_environment.json my-api          # secrets go to the keychain
pulpo import openapi.yaml my-api                            # one folder per tag, example bodies
pbpaste | pulpo import - my-api                             # a curl command ("Copy as cURL")
```

Postman v2.1 collections and environments, OpenAPI 3 (JSON or YAML, including `$ref`s to sibling files) and curl commands (Unix and Windows `^` continuations) are recognised automatically. Anything that cannot be imported yet, like pre-request and test scripts, is listed as a warning. In the desktop app, use **Import** in the sidebar.

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
