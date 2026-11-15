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

## GraphQL

Pick **GraphQL** as the body of a request: the query and its variables get their own editors, and **Load schema** reads the endpoint's schema by introspection (with the request's auth, headers and environment).

- Completion (Ctrl+Space), error marks and a **Docs** explorer come from that schema; search any type or field and click through types.
- The schema is cached in `.pulpo/graphql/`, so it is there offline; reading it again tells you what changed (`+User.email`, `−Order.total`).
- It goes out as JSON `{query, variables, operationName}` with `{{variables}}` filled in; a query with several operations lets you pick one.
- **Subscriptions** go over WebSocket at the same address (`https` → `wss`): **Subscribe** speaks `graphql-transport-ws` (the graphql-ws library) or the older `graphql-ws` (Apollo), whichever the server picks, and lists every event until you stop or the server completes it.
- On disk the query is a `.graphql` file and the variables a `.variables.json` file next to the request, so reviews read like code. GraphQL requests imported from Postman, Insomnia, Bruno, curl or HAR become GraphQL requests.

```
orders/
  01-order.pulpo.yaml          protocol: graphql, url, auth...
  01-order.body.graphql        query Order($id: ID!) { order(id: $id) { id total } }
  01-order.variables.json      { "id": "{{orderId}}" }
```

## WebSocket

Write a `ws://` or `wss://` URL and the request becomes a WebSocket: **Connect** opens it with the request's headers and auth (also the ones inherited from folders), and a log shows what you send and receive, with times; JSON opens formatted.

- **Messages** keeps the ones you send often (subscribe, ping…), with `{{variables}}`: one click sends them while connected.
- `pulpo run` and flows connect, send the saved messages in order and collect what comes back until the line goes quiet. Tests read it as a list:

```js
const got = pm.response.json();          // every message received, JSON ones as objects
pm.test("subscribed", () => pm.expect(got[0].type).to.eql("subscribed"));
```

- Saved messages live in the request file as readable YAML blocks:

```yaml
protocol: "websocket"
url: "wss://stream.example.com/live"
messages:
  - name: "Subscribe"
    body: |-
      {
        "type": "subscribe",
        "channel": "{{channel}}"
      }
```

## Socket.IO

Press **SIO** next to the URL (`http://host:port`) to talk Socket.IO v4: **Connect** joins the namespace with your `auth` JSON (`{"token": "{{token}}"}`), the log shows events by name, and **Emit** sends one (an event name and its data). Saved events are one click away; `pulpo run` and flows emit them in order and give the tests what came back:

```js
const got = pm.response.json();   // [{ "event": "welcome", "args": [{ "rooms": 2 }] }, ...]
pm.test("welcomed", () => pm.expect(got[0].event).to.eql("welcome"));
```

Runs listen at most 5 seconds after sending (WebSocket, Socket.IO and SSE alike), so chatty servers do not hold them up.

## MQTT

`mqtt://broker:1883` and `mqtts://broker:8883` URLs talk MQTT 3.1.1 (press **MQTT** for a broker over `ws://`/`wss://`). In **Topics** set what to subscribe to (`orders/#`, `sensors/+/temp`), QoS 0 or 1, retain and the credentials (a `{{secret}}` for the password); **Connect** shows every message with its topic (retained ones marked) and **Publish** sends one. Saved publications (name = topic, `{{variables}}` allowed) go out in order with `pulpo run`, and tests get what arrived:

```js
const got = pm.response.json();   // [{ "topic": "orders/7", "payload": { "status": "paid" }, "qos": 1, "retain": false }]
```

## Server-Sent Events

Press **SSE** next to the URL (it sets `Accept: text/event-stream`) and **Listen**: events arrive as they come, with their name, id and time; JSON opens formatted. Headers, auth, `{{variables}}` and a body work as in any request, so `POST` streams (like text generation APIs) work too.

`pulpo run` and flows listen until the stream ends or goes quiet, and tests get the events:

```js
const events = pm.response.json();   // [{ "event": "tick", "data": { "n": 1 }, "id": "1" }, ...]
pm.test("first tick", () => pm.expect(events[0].data.n).to.eql(1));
```

## gRPC

Write a `grpc://host:port` URL (`grpcs://` for TLS) and the request becomes gRPC. **Load methods** asks the server (reflection) or reads the `.proto` files you list, compiled by Pulpo itself (no `protoc` needed); pick a method and **Use example** writes a message with every field.

- The message is JSON (protobuf's JSON mapping) with `{{variables}}`; headers go as metadata.
- Unary and server streaming send the message; client and bidirectional streaming send the saved messages in order.
- The reply comes back as JSON (a list when the server streams). Errors keep their gRPC code (`NotFound: no such order`) and map to the closest HTTP status, so tests read them as usual; `grpc-status` and the trailers are in the headers.
- `pulpo run` and flows call gRPC methods like any other request.

```yaml
protocol: "grpc"
url: "grpc://localhost:50051"
grpc:
  method: "orders.Orders/Get"
  protoFiles:
    - "protos/orders.proto"     # leave it out to use server reflection
body: "json"                    # the message, in 01-get.body.json
```

## SOAP

Pick **SOAP** as the body, give the WSDL (a workspace file or a URL) and **Load operations**: pick one and Pulpo writes the envelope from the WSDL's schema (SoapUI style: `?` for text, `<!--Optional:-->` marks) and the `Content-Type`/`SOAPAction` headers. Document and rpc styles, SOAP 1.1 and 1.2.

- The envelope is checked as you type: valid XML, a SOAP Envelope of the right version, and the operation's element first in the Body.
- `pulpo import service.wsdl` creates a request per operation; `{{variables}}` work anywhere in the envelope.

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

## The collection follows the code

Your repository already describes its API. Pulpo finds those contracts and keeps the collection in step with them.

```bash
pulpo scan                                   # OpenAPI, .proto, WSDL and GraphQL files, and services in code
pulpo import services/orders/openapi.yaml    # the new folder stays linked to the file
pulpo import services/orders                 # no contract? read the controllers and routes
pulpo sync                                   # after a git pull: what changed in each contract
pulpo sync --apply                           # add new operations, complete what requests miss
pulpo sync "Orders" --from ../api/orders.yaml   # link a folder made by hand or from Postman
pulpo sync --exit-code                       # in CI: fail when the collection is behind the code
```

```
Orders vs ../api/orders.yaml (Orders API): 2 new, 1 gone, 3 to update, 18 in step (+4 repeated calls)
+ POST    Withdraw a proposal  (POST /api/v2/proposals/{}/withdraw)
- GET     Can it be amended?  …/30-can-it-be-amended.pulpo.yaml
      maybe moved to GET /api/v2/agreements/{}/amendment-availability
~ POST    Create proposal  misses required header Idempotency-Key
```

- Requests are matched by method and path, whatever the server variable or parameter style (`{id}`, `:id`, `{{id}}`), so hand-made collections link too. Calling an endpoint twice (a lifecycle folder) is fine.
- Applying only adds: new operations land in a folder per tag, and existing requests get the required query params, headers or body they miss. Names, scripts and values you wrote are never changed. Gone requests are deleted only when you tick them (or `--remove`).
- No OpenAPI file? Pulpo reads the code itself: Spring (`@RequestMapping`, `@Tag`, `@Operation`, required params, headers and body), NestJS, Express and Fastify (following `app.use('/prefix', router)` across files), FastAPI (`APIRouter` prefixes and tags), Next.js and Nuxt route files. Requests are grouped in a folder per tag or controller, and `{{baseUrl}}` comes from the port and context path in the config (`application.yml`, `.env`…). The folder stays linked to the service, so `pulpo sync` tells you when a developer adds an endpoint.
- In the app, **⟲** next to the collection name lists the contracts of the repository and shows a badge when a linked folder is out of step (checked on opening and after a pull).

## Flows: features made of requests you already have

A folder groups endpoints; a **flow** tells a feature: log in, pick a doctor, see the free hours, book. Flows reference the requests of the collection (they never copy them), so changing a request changes every flow that uses it, and a flow can reuse another one.

```yaml
# flows/book-a-visit.flow.yaml
version: 1
name: "Book a visit"
steps:
  - flow: "log-in"                                   # another flow, reused
  - request: "02-visits/01-doctors.pulpo.yaml"
    save:
      doctorId: "$.data[0].id"                       # JSONPath into the response
  - request: "02-visits/02-free-hours.pulpo.yaml"    # uses {{doctorId}}
    expect:
      status: 200
```

When a feature branches or waits, a step can say so. The flow stays a list of steps, not a language: anything more complex belongs in the request's scripts.

```yaml
steps:
  - request: "03-orders/01-list.pulpo.yaml"
    save:
      ids: "$.items[*].id"                           # several matches make a list
  - request: "03-orders/02-cancel.pulpo.yaml"
    forEach: "{{ids}}"                               # once per item; also "a, b, c"
    as: "orderId"
    if: "{{role}} == admin"                          # ==, !=, >, <, >=, <=, contains, or just {{var}}
  - wait: 2s                                         # 500ms, 2s, 1m
  - request: "04-payments/03-status.pulpo.yaml"
    save:
      state: "$.state"
    retry:                                           # poll until it settles
      until: "{{state}} == PAID"                     # {{$status}} is the last HTTP status
      times: 10
      every: 2s
```

- **Let Pulpo chain them.** `pulpo chain <folder> --flow "Order an item"` (or right-click a folder → *Make a flow from this folder…*) finds which response feeds which request: `{{orderId}}` ← `id` of `POST /orders`, `{{token}}` ← `access_token` of the login. The evidence comes from saved examples, the folder's OpenAPI contract and, with `--run`, real responses. Variables already set by request or folder scripts are left to them; the ones nothing provides are listed, with the request in another folder that sets them.
- In the app, **Flows** in the sidebar shows each flow as a tree; ▶ Run lights each step green or red with its status and time (and the round of a `forEach` or the attempts of a `retry`), and a click shows its response and the values it saved. Drag a step to move it. Right-click a request → *Add to flow…*; a request used by flows shows *In N flows*.
- Values a flow saves live only while it runs; the environment is not touched. Request scripts keep working as usual. The flow stops at the first failing step; a step whose `if` does not hold is skipped.
- `pulpo flow ls`, `pulpo flow show <flow>` and `pulpo flow run <flow> -e Local --junit report.xml` for CI. Through `pulpo mcp` your AI can list, create and run flows too.

## Let your AI use the collection (MCP)

`pulpo mcp` serves the collection to your AI assistant over the Model Context Protocol. It runs on your machine, on the files of the repo.

```bash
claude mcp add pulpo -- pulpo mcp          # Claude Code, from the repository folder
```

Cursor, VS Code (Copilot) and other MCP clients take the same command (`pulpo mcp`, optionally `-w <folder>`). Then you can ask things like *"run the orders folder against Local and tell me what fails"* or *"add a request for the endpoint I just wrote, with a test for 201"*.

| Tool | What it does |
|---|---|
| `list_requests` | Requests of the collection or a folder, filtered by words |
| `get_request` | One request as stored, with the auth it inherits |
| `run_request`, `run_folder` | Send with scripts and tests; response, test results and logs |
| `create_request`, `update_request` | Change the collection files (you review them in git) |
| `list_environments` | Environments and variables |
| `import_collection`, `export_collection` | curl, Postman, Insomnia, OpenAPI, HAR, `.http`… in and out |
| `collection_diff` | What changed between git revisions, request by request |

Secret values never reach the AI: they are replaced by `••••` in everything a tool returns, and requests keep `{{variables}}`. Environments named like production (`prod`, `prd`, `production`, `live`) are refused unless the AI passes `confirmProduction`, which it should only do after you say yes.

## Share a link to a request

Right-click a request → *Copy link*, or in a terminal:

```bash
pulpo link orders/01-create-order.pulpo.yaml
# pulpo://open?repo=github.com/acme/shop&ws=api&request=orders/01-create-order.pulpo.yaml&name=Shop
```

Paste it in a chat. Clicking it opens Pulpo on that request, in your teammate's own clone: the link names the git repository (its `origin` remote, so SSH host aliases still match) and the folder inside it, not a server. The first time, Pulpo asks for the folder of that clone and then remembers it. You can also paste a link in the search box (Ctrl+K), or run it with `pulpo run <link>`.

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
