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

In the app, `{{variables}}` are coloured by where their value comes from (environment, set by a script, dynamic) and missing ones are underlined in red; hover one to see its value (never a secret's) and type `{{` to complete. Use them anywhere as `{{base}}`, plus dynamic values: `{{$guid}}`, `{{$timestamp}}`, `{{$isoTimestamp}}`, `{{$randomInt}}`.

```bash
pulpo env my-api                              # list environments (secrets masked)
pulpo secret set my-api local token           # store a secret, read from stdin
pulpo send GET "{{base}}/health" -w my-api -e local
PULPO_SECRET_TOKEN=... pulpo send ...         # in CI, secrets come from env vars
```

## Auth

Set auth once on a folder (`_folder.yaml`) or the whole workspace (`pulpo.yaml`) and every request inside inherits it. A request or subfolder can set its own, or `type: "none"` to send nothing. Supported: `bearer`, `basic`, `apiKey` (in a header or the query string), `oauth2`, `digest`, `awsV4` (AWS Signature v4), `hawk` and `ntlm`.

```yaml
version: 1
name: "Orders"
auth:
  type: "bearer"
  token: "{{token}}"
```

### OAuth 2.0

Pulpo gets the token for you and sends it as `Authorization: Bearer …`. Grants: `authorizationCode` (with PKCE, through your browser), `clientCredentials` and `password`.

```yaml
auth:
  type: "oauth2"
  grant: "authorizationCode"
  authUrl: "{{idp}}/authorize"
  tokenUrl: "{{idp}}/token"
  clientId: "{{clientId}}"
  scope: "openid api"
```

- **Client credentials and password**: the token is requested when you send, cached and reused until it expires.
- **Authorization code**: press *Get new access token* in the Auth tab (or run `pulpo auth login <request>`). Your browser opens the login page and comes back to `http://127.0.0.1:<free port>/callback`. Register that redirect URI with your provider, or set a fixed one such as `http://127.0.0.1:8765/callback` in `redirectUri`.
- Expired tokens are renewed with the refresh token when there is one.
- Tokens are kept per environment in the OS keychain, split into parts because Windows caps a credential at 2.5 KB and tokens from Azure AD and others are longer. Only their expiry, scope and grant go to `.pulpo/tokens.json` (local, never committed). `pulpo auth clear` forgets them all.
- *Discover* fills the URLs from an issuer (OpenID Connect discovery), the revocation URL included.
- The **🔑 tokens panel** lists the tokens of the environment: how long each one has left, which requests use it, and buttons to **Renew** it now, **Revoke** it at the identity provider (when the auth has a `revokeUrl`) or **Forget** it on this machine. In the terminal: `pulpo auth ls`, `pulpo auth renew <KEY>` and `pulpo auth revoke <KEY>`.
- Imported from Postman collections and from OpenAPI `oauth2` security schemes.

### Digest, AWS Signature v4, Hawk and NTLM

These are computed when the request is sent, so they only work from the desktop app and the CLI (not the browser preview).

```yaml
auth:
  type: "awsV4"
  accessKey: "{{awsAccessKey}}"
  secretKey: "{{awsSecretKey}}"
  region: "eu-west-1"
  service: "execute-api"
```

- **Digest** (`username`, `password`): Pulpo reads the server's `WWW-Authenticate: Digest` challenge and sends the request again with the answer. MD5, SHA-256 and their `-sess` variants.
- **AWS Signature v4** (`accessKey`, `secretKey`, `region`, `service`, optional `sessionToken`): signs method, URL, headers and body. For `s3` it also sends `x-amz-content-sha256`.
- **Hawk** (`id`, `key`, `algorithm` `sha256` or `sha1`, optional `ext`, `payload: true` to sign the body).
- **NTLM** (`username`, `password`, optional `domain` and `workstation`): NTLMv2 handshake on one HTTP/1.1 connection.
- Imported from Postman collections (and Digest from OpenAPI `http` schemes).

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

Query responses with JSONPath or jq, the same engines as the response filter in the app (where "Create test" writes the test for you):

```js
pm.test("first order", () => pm.expect(pm.response.jsonPath("$.data[0].id")).to.eql([1]));
pm.test("total", () => pm.expect(pm.response.jq("[.data[].price] | add")).to.eql([42]));
```

Also supported: `pm.sendRequest` (for logins and setup calls, with or without a callback), `CryptoJS` (`SHA256`, `SHA1`, `SHA512`, `MD5` and their `Hmac*`, with `enc.Hex`, `enc.Base64`, `enc.Utf8`; also via `require('crypto-js')`), `atob` and `btoa`.

Contract and snapshot tests work the same way. Requests imported from OpenAPI get the contract test written for them (documented status and response schema):

```js
pm.test("matches the schema", () => pm.response.to.have.jsonSchema(orderSchema));
pm.response.to.matchSnapshot({ ignore: ["$.updatedAt", "$..id"] });   // kept in <request>.snapshot.json
```

## Run collections

In the app, **▶** (or right-click a folder → Run folder…) opens the runner: pick requests, iterations, a CSV or JSON data file, delay, retries; watch the progress, re-run only the failed ones, export HTML or JUnit, and accept a changed snapshot with one click. From the terminal:

```bash
pulpo run -w my-api -e local              # run every request in order, fail on a failed test
pulpo run 01-orders -w my-api -e local --bail
pulpo run -w my-api -e local --data users.csv --retries 2      # one iteration per row: {{user}}
pulpo run -w my-api -e ci --junit report.xml --html report.html --summary summary.md
pulpo load 01-orders -w my-api -e staging -u 20 -d 1m          # light load test with percentiles
```

In GitHub Actions, [`actions/run`](actions/run) runs a collection and comments the summary on the pull request:

```yaml
- uses: pulpo-api/pulpo/actions/run@main
  with:
    workspace: api-tests
    environment: Staging
```

## Share a request as code

Any request becomes curl, JavaScript (fetch or axios), Python requests, Java HttpClient, Go, C# or PowerShell, with what it inherits from its folders. Variables stay as `{{name}}` unless you resolve them, and secrets (plus values set by scripts, usually tokens) only go in when you ask. In the app: the `</>` button, or right-click → Copy as curl.

```bash
pulpo code 01-orders/01-create-order.pulpo.yaml -w my-api -l python
pulpo code 01-orders/01-create-order.pulpo.yaml -w my-api -e local --resolve
```

CI runs the generated code in all eight languages against a local server (`tests/codegen/run.sh`).

## Import what you already have

```bash
pulpo import my-collection.postman_collection.json my-api   # folders, auth, bodies, variables
pulpo import local.postman_environment.json my-api          # secrets go to the keychain
pulpo import openapi.yaml my-api                            # one folder per tag, example bodies
pbpaste | pulpo import - my-api                             # a curl command ("Copy as cURL")
pulpo import Insomnia_2026.json my-api                      # Insomnia v4 (JSON) or v5 (YAML)
pulpo import ~/bruno/shop/bruno.json my-api                 # a Bruno collection folder
pulpo import requests.http my-api                           # VS Code REST Client / JetBrains
pulpo import session.har my-api                             # browser DevTools: API calls only
```

Recognised automatically:

| From | What comes in |
|---|---|
| Postman v2.1 | Collections (folders, auth, bodies, scripts) and environments |
| Insomnia v4 / v5 | Folders, requests, auth, base and sub-environments; `insomnia.*` scripts become `pm.*` |
| Bruno | The `.bru` folder or its JSON export, with environments (secret values stay on each machine) |
| Hoppscotch | Collections, `<<variables>>` become `{{variables}}` |
| OpenAPI 3 | JSON or YAML, including `$ref`s to sibling files |
| HAR | XHR and fetch calls grouped by host; cookies left to the jar |
| `.http` / `.rest` | Requests, `@variables` as an environment, `# @name`, `< file` bodies |
| curl | Unix and Windows `^` continuations |

Scripts written for Bruno (`bru.*`) or Hoppscotch (`pw.*`) are kept as a comment to rewrite, so nothing breaks when they run. Anything that cannot be imported is listed as a warning. In the desktop app, use **Import** in the sidebar.

## Review collection changes like code

```bash
pulpo diff                       # HEAD → the files on disk
pulpo diff main                  # what this branch changes
pulpo diff main HEAD --markdown  # for a pull request comment
```

```
Collection changes (main → working tree): 1 added, 0 removed, 1 changed
+ POST    Orders / Create order  {{base}}/orders
~ GET     Users / List users
      URL `{{base}}/users` → `{{base}}/users?page=1`
      header X-Tenant added
```

Request by request, not line by line: moved and renamed requests are recognised, and folder auth, headers and environment variables are compared too (secret values are never in git). `--exit-code` fails a CI step when the collection changed.

In the desktop app, **⑂** next to the collection name opens the git panel: the same request-by-request changes since the last commit (click one to open it), commit, switch or create a branch, pull and push. It uses your own `git` and its credentials, and only ever commits files of the workspace.

## Export: nobody is tied to Pulpo

```bash
pulpo export postman -o shop.postman_collection.json   # folders, auth, scripts, examples
pulpo export openapi orders -o orders.openapi.yaml      # one folder, as OpenAPI 3
pulpo export http > requests.http                       # VS Code REST Client / JetBrains
pulpo export env Local -o local.postman_environment.json
```

In the app: **⇪** next to the collection name, or right-click a folder → *Export folder…*. Postman exports also open in Insomnia and Bruno. Folder headers go on each request (Postman has none), variables stay as `{{name}}` and secret values are never written.

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
