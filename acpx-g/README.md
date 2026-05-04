# acpx-g

DAG workflow engine — YAML-defined workflows, web UI, REST API, SQLite persistence, directory watcher.

## Quick Start

```bash
# Start the server
DATABASE_URL="sqlite:acpx-g.db?mode=rwc" cargo run -p acpx-g

# Or with default settings
cargo run -p acpx-g

# Watch a directory for workflow YAML files (auto-submit on version change)
cargo run -p acpx-g -- --workflow-dir ./workflows
```

Server starts on `http://0.0.0.0:3000` (configurable via `PORT` env). Open in browser for the built-in Web UI.

## Web UI

The server serves a single-page application from `static/` with:

- **DAG visualization** — interactive node graph with zoom/pan, color-coded statuses
- **Run list** — recent workflow runs with duration and status
- **Node logs** — click any node to view stdout/stderr
- **Template runner** — browse discovered templates, fill inputs, and run
- **API docs** — built-in endpoint reference (also at `GET /api/v1/docs`)

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/workflows` | Submit and execute a workflow |
| `GET` | `/api/v1/workflows` | List workflow runs (paginated) |
| `GET` | `/api/v1/workflows/{run_id}` | Get run details + all node statuses |
| `DELETE` | `/api/v1/workflows/{run_id}` | Delete a completed workflow run |
| `POST` | `/api/v1/workflows/{run_id}/cancel` | Cancel a running/pending workflow |
| `POST` | `/api/v1/workflows/{run_id}/rerun` | Re-run a workflow with optional input overrides |
| `GET` | `/api/v1/workflows/{run_id}/nodes/{node_id}/logs` | Get node stdout/stderr |
| `GET` | `/api/v1/templates` | List discovered workflow templates |
| `POST` | `/api/v1/templates/{name}/run` | Run a template by name (with inputs) |
| `GET` | `/api/v1/docs` | API documentation |

### Submit a Workflow

```bash
curl -X POST http://localhost:3000/api/v1/workflows \
  -H "Content-Type: application/json" \
  -d '{"yaml": "name: hello\nversion: 1.0\nnodes:\n  - id: greet\n    type: shell\n    run: echo hello world"}'
```

Response:
```json
{ "run_id": "0193...", "status": "pending" }
```

### Run a Template

```bash
# List available templates
curl http://localhost:3000/api/v1/templates

# Run a template with inputs
curl -X POST http://localhost:3000/api/v1/templates/hello/run \
  -H "Content-Type: application/json" \
  -d '{"inputs": {"tag": "v1.0.0", "env": "production"}}'
```

### Check Run Status

```bash
curl http://localhost:3000/api/v1/workflows/0193...
```

Response:
```json
{
  "id": "0193...",
  "workflow_name": "hello",
  "workflow_version": "1.0",
  "status": "success",
  "node_count": 1,
  "started_at": "2026-05-03T10:00:00+00:00",
  "finished_at": "2026-05-03T10:00:01+00:00",
  "nodes": [
    { "node_id": "greet", "node_type": "shell", "status": "success", "stdout": "hello world\n" }
  ]
}
```

### Cancel a Running Workflow

```bash
curl -X POST http://localhost:3000/api/v1/workflows/0193.../cancel
```

Response:
```json
{ "status": "cancelled", "run_id": "0193..." }
```

### Re-run a Workflow

```bash
# Re-run with original inputs
curl -X POST http://localhost:3000/api/v1/workflows/0193.../rerun

# Re-run with input overrides
curl -X POST http://localhost:3000/api/v1/workflows/0193.../rerun \
  -H "Content-Type: application/json" \
  -d '{"inputs": {"env": "production"}}'
```

Response:
```json
{ "run_id": "0194...", "status": "pending", "rerun_of": "0193..." }
```

### Delete a Workflow Run

```bash
curl -X DELETE http://localhost:3000/api/v1/workflows/0193...
```

Response:
```json
{ "deleted": "0193..." }
```

## Workflow Schema

```yaml
name: "example-workflow"
version: "1.0"
description: "Build, test, and deploy"

defaults:
  retry: 0
  timeout: 300
  shell: "bash -c"

inputs:
  tag:
    type: string
    required: true
  env:
    type: string
    default: "production"

env:
  RUST_BACKTRACE: "1"

references:
  notify: "./notify.yaml"
  remote: "https://example.com/workflows/remote.yaml"

nodes:
  # Shell: inline script
  - id: checkout
    type: shell
    run: |
      git clone https://github.com/org/repo.git repo
      cd repo && git checkout {{ inputs.tag }}
    outputs:
      repo_dir: "./repo"
    env:
      GIT_TERMINAL_PROMPT: "0"

  # Shell: external file
  - id: build
    type: shell
    run: { file: "./scripts/build.sh" }
    depends: [checkout]
    timeout: 600
    retry: 1
    outputs:
      artifact_path: "./repo/target/release/app"

  # Shell: platform-specific scripts
  - id: deploy
    type: shell
    run:
      linux: "./scripts/deploy-linux.sh"
      macos: "./scripts/deploy-macos.sh"
      windows: "./scripts/deploy.ps1"
      default: "./scripts/deploy.sh"
    depends: [build]

  # Conditional execution: only run when env is "production"
  - id: deploy-prod
    type: shell
    if: "{{ inputs.env }} == production"
    depends: [deploy]
    run: "./scripts/deploy-prod.sh"

  # Continue even if this node fails
  - id: notify
    type: shell
    depends: [deploy-prod]
    continue_on_error: true
    run: "echo 'Deployment complete'"

  # Agent: wraps acpx CLI (default subcommand: "peri")
  - id: review
    type: agent
    prompt: "Review changes for tag {{ inputs.tag }}"
    agent: peri                     # CLI subcommand (peri / claude / codex, default: peri)
    model: sonnet
    cwd: "{{ needs.checkout.outputs.repo_dir }}"
    depends: [build]

  # Agent: external prompt file
  - id: summarize
    type: agent
    prompt: { file: "./prompts/summarize.md" }
    depends: [review]
    continue_on_error: true

  # Reference: call another workflow
  - id: call-notify
    type: reference
    ref: notify
    with:
      channel: "#deploy"
    depends: [deploy]
```

### Input Validation

Workflow inputs support type checking:

| Type | Validation |
|------|------------|
| `string` | Accepted as-is |
| `number` | Parsed as f64, rejects non-numeric |
| `boolean` | Accepts `true`/`false`/`yes`/`no`/`1`/`0` (case-insensitive) |

Inputs with `required: true` and no `default` must be provided when submitting. Missing required inputs return a `400` error.

## Node Types

| Type | Description |
|------|-------------|
| `shell` | Execute inline script, external file, or platform-specific scripts |
| `agent` | Wrap `acpx` CLI as a node. Fields: `prompt` (required), `agent` (CLI subcommand, default `"peri"`), `model`, `cwd` |
| `reference` | Call another workflow by alias (local path or HTTPS URL) |

### Node Execution Options

All node types support these optional execution controls:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `timeout` | integer | — | Max execution time in seconds. Exceeded → node fails |
| `retry` | integer | `0` | Number of retries on failure (exponential backoff: 1s, 2s, 4s...) |
| `if` | string | — | Conditional execution. Template expression evaluated at runtime; falsy → node is skipped |
| `continue_on_error` | boolean | `false` | If `true`, downstream nodes execute even when this node fails |
| `depends` | string[] | `[]` | List of node IDs that must complete before this node runs |
| `shell` | string | `"sh"` | Shell command for `run` (e.g. `"bash -c"`) |

**Conditional execution** (`if` field): supports truthiness evaluation and comparison operators:
- Truthy: any non-empty value except `false`, `0`, `no`
- Comparisons: `{{ inputs.env }} == production`, `{{ inputs.tag }} != v0`
- Unresolved template variables (missing inputs) evaluate to empty string (falsy)

**Example** (`examples/error-handling.yaml`):
```yaml
defaults:
  retry: 0
  timeout: 60

nodes:
  - id: checkout
    type: shell
    retry: 2              # retry up to 2 times for flaky networks

  - id: build
    type: shell
    depends: [checkout]
    timeout: 30           # 30-second timeout

  - id: test
    type: shell
    depends: [build]
    continue_on_error: true  # downstream nodes still run if tests fail

  - id: deploy-prod
    type: shell
    if: "{{ inputs.deploy_env }} == production"  # only in production
    depends: [test]
```

### Script / Prompt Sources

Every `run` (shell) and `prompt` (agent) field supports three forms in a single field:

```yaml
# 1. Inline
run: "echo hello"

# 2. Single file
run: { file: "./scripts/build.sh" }

# 3. Platform-specific (current OS → default fallback)
run:
  linux: "./scripts/linux.sh"
  macos: "./scripts/macos.sh"
  default: "./scripts/default.sh"
```

### Platform Resolution

- `cfg!(target_os)` at compile time, `std::env::consts::OS` as fallback
- Priority: exact OS match → `default` → error
- Supported platforms: `linux`, `macos`, `windows`

### Data Flow

```
{{ inputs.<key> }}                 # Workflow inputs (from API or parent's with)
{{ needs.<node_id>.outputs.<key> }} # Upstream node outputs
{{ env.<KEY> }}                    # Environment variables
```

Template variables `{{ }}` are resolved at execution time (not load time). They can appear in `run`, `prompt`, `env` values, and `cwd` fields.

## Workflow References

Workflow references let you decompose complex pipelines into reusable sub-workflows. A parent workflow declares aliases for external YAML files, then invokes them via `type: reference` nodes.

### Three Core Concepts

1. **Declaration** — `references` block maps aliases to paths/URLs
2. **Invocation** — `type: reference` nodes call a sub-workflow
3. **Inline expansion** — at load time, reference nodes are replaced by the child's nodes (with prefixed IDs), producing a single flat DAG

### Declaration

```yaml
references:
  build: "./build-lib.yaml"           # local file (relative to this YAML)
  notify: "../shared/notify.yaml"     # local file (relative path)
  remote: "https://example.com/wf.yaml"  # remote URL
```

**Path resolution**:
- Local paths are relative to the **declaring file's directory**, not the working directory
- Remote URLs are fetched via HTTP GET at load time
- A child's own `references` use paths relative to the child's file location

### Invocation

```yaml
nodes:
  - id: do-build                     # reference node ID → becomes prefix for child nodes
    type: reference
    ref: build                       # must match a key in references
    with:                            # parameters → bound to child's inputs
      repo_url: "https://github.com/org/repo.git"
      branch: "main"
    depends: [checkout]              # boundary dependency (see below)
    continue_on_error: false
    timeout: 300
    retry: 0
```

### Resolution Process

When `load_workflow()` encounters a `type: reference` node:

```
1. Look up `ref` in references map → get path/URL
2. Fetch & parse child workflow YAML
3. Resolve `with` template values (using parent's inputs/env context)
4. Bind `with` → child's inputs
5. Prefix all child node IDs with "{reference_node_id}/"
6. Rewire child internal depends with prefixed IDs
7. Wire boundary deps:
   - reference node's depends → child entry nodes (no internal deps)
   - parent depends-on-reference → child exit nodes (no internal dependents)
8. Replace reference node with inlined child nodes
9. Recurse if child also has reference nodes
10. Detect circular references via canonical path tracking
```

### Parameter Passing: `with` → `inputs`

The `with` map on a reference node becomes the child workflow's resolved `inputs`:

```yaml
# Parent
- id: do-build
  type: reference
  ref: build
  with:
    repo_url: "{{ inputs.repo }}"    # parent's input
    branch: "{{ inputs.tag }}"       # parent's input
```

```yaml
# Child (build-lib.yaml)
inputs:
  repo_url:
    type: string
    required: true
  branch:
    type: string
    default: "main"
nodes:
  - id: checkout
    type: shell
    run: "git clone {{ inputs.repo_url }} repo && cd repo && git checkout {{ inputs.branch }}"
```

After binding: child nodes see `{{ inputs.repo_url }}` = the value from parent's `with`.

**`with` template resolution timing**: `with` values containing `{{ }}` are resolved at execution time using the parent's context (`inputs`, `env`). This allows passing dynamic values derived from upstream outputs:

```yaml
with:
  message: "Deploy {{ inputs.tag }} done"        # parent input
  artifact: "{{ needs.build.outputs.path }}"      # upstream output
```

### Dependency Wiring

Reference nodes create a **boundary** between parent and child DAGs. After inline expansion, the boundary is wired automatically:

```
Parent DAG:          After expansion:

checkout             checkout
  |                    |
do-build ───┐        do-build/checkout → do-build/build → do-build/test
(ref node)   │        (entry nodes get parent deps)        (exit nodes)
  |          │                                            |
deploy       │                                          deploy
             └─────────── depends wired to exit nodes ──┘
```

**Rules**:
- `depends: [do-build]` → expands to ALL child exit nodes
- `depends: [do-build/test]` → depends on a specific child node (fine-grained)
- Reference node's `depends: [checkout]` → added to ALL child entry nodes

### Output Propagation

After expansion, parent nodes reference child outputs using the **prefixed path**:

```yaml
# Reference a specific child node's output
- id: deploy
  type: shell
  run: "deploy {{ needs.do-build/build.artifact_path }}"
  depends: [do-build/build]         # fine-grained: only wait for build
```

```yaml
# Use a child node's output as cwd
- id: review
  type: agent
  prompt: "Review the code"
  cwd: "{{ needs.do-build/checkout.repo_dir }}"
  depends: [do-build/checkout]
```

**Key principle**: after expansion, child nodes are first-class nodes in the flat DAG. Any field that supports `{{ }}` can reference them via `needs.{prefix}/{child_id}.outputs.{key}`.

### Circular Detection

Load time tracks all canonical file paths. If a file appears again during recursive resolution, the load fails with a clear error:

```
error: circular reference detected: ./ci.yaml → ./build.yaml → ./ci.yaml
```

### Complete Example

See `examples/` directory:

| File | Description |
|------|-------------|
| `simple-ci.yaml` | Parent workflow referencing setup + agent node |
| `setup.yml` | Simple setup sub-workflow (single shell node) |
| `build-lib.yaml` | Reusable build sub-workflow (checkout → build → test with outputs) |
| `notify.yaml` | Reusable notification sub-workflow (parameterized inputs) |

**Before expansion** (`simple-ci.yaml`):

```
do-setup ───┐    (reference node → setup.yml)
             │
acpx ───────┘    (agent node, depends: do-setup)
```

**After expansion** (flat DAG):

```
do-setup/shell_test ──→ acpx
```

**More complex example** — a parent using `build-lib.yaml` + `notify.yaml`:

```yaml
# Parent workflow
name: ci-pipeline
version: "1.0"

references:
  build: "./build-lib.yaml"
  notify: "./notify.yaml"

inputs:
  repo:
    type: string
    required: true
  tag:
    type: string
    required: true

nodes:
  - id: do-build
    type: reference
    ref: build
    with:
      repo_url: "{{ inputs.repo }}"
      branch: "{{ inputs.tag }}"

  - id: notify-ok
    type: reference
    ref: notify
    with:
      channel: "#deploy"
      message: "Build {{ inputs.tag }} succeeded"
    depends: [do-build]

  - id: deploy
    type: shell
    run: "deploy {{ needs.do-build/build.artifact_path }}"
    depends: [do-build/build]

  - id: notify-done
    type: reference
    ref: notify
    with:
      channel: "#deploy"
      message: "Deploy done"
    depends: [deploy, notify-ok]
```

**Before expansion:**

```
do-build ─────────────────────┐    (reference node)
notify-ok ─────────────┐      │    (reference node)
deploy ────────────────┼──────┤    (depends: do-build/build)
notify-done ───────────┴──────┘    (depends: deploy, notify-ok)
```

**After expansion** (flat DAG):

```
do-build/checkout → do-build/build → do-build/test
       │                  │
       │                  ├─→ deploy
       │                  │
notify-ok/send ←──────────┼─────── (depends: do-build exit nodes)
       │
notify-done/send ←────────┴─────── (depends: deploy, notify-ok/send)
```

### Summary: Reference Node Behavior

| Aspect | Behavior |
|--------|----------|
| ID prefix | `{reference_id}/` prepended to all child node IDs |
| `with` | Bound to child's `inputs` at execution time |
| Entry nodes | Inherit reference node's `depends` |
| Exit nodes | Replace reference node in parent's `depends` |
| `env` | Child inherits parent's `env`, child's `env` overrides |
| `defaults` | Child uses its own `defaults`, not parent's |
| Outputs | Via `needs.{prefix}/{child_id}.outputs.{key}` |
| Depth | Recursive (child can reference grandchild) |
| Cycle | Detected at load time (canonical path tracking) |
| Remote | HTTP(S) URLs fetched at load time |

## Architecture

```
acpx-g/
├── Cargo.toml
├── README.md
├── examples/                 # Example workflow YAML files
│   ├── simple-ci.yaml        # Parent workflow referencing build + notify
│   ├── build-lib.yaml        # Reusable build sub-workflow
│   ├── notify.yaml           # Reusable notification sub-workflow
│   └── setup.yml             # Setup sub-workflow
├── static/                   # Web UI (SPA)
│   ├── index.html            # Single-page app shell
│   ├── style.css             # Styles
│   ├── app.js                # Main application logic + DAG visualization
│   └── api-docs.js           # API documentation modal
└── src/
    ├── main.rs               # axum HTTP server, CLI arg parsing, watcher startup
    ├── lib.rs                # crate root
    ├── schema.rs             # YAML schema types + platform resolution
    ├── watcher.rs            # Directory watcher for auto-submitting workflows
    ├── db/
    │   ├── mod.rs            # SQLite init + migrations
    │   └── models.rs         # WorkflowRun, NodeRun, API request/response types
    ├── api/
    │   ├── mod.rs
    │   └── workflows.rs      # axum handlers (workflows + templates + docs)
    └── runner/
        ├── mod.rs            # DAG scheduler (topological sort, parallel exec)
        ├── executor.rs       # Shell + agent execution with retry/timeout
        ├── loader.rs         # YAML loading + recursive reference resolution
        └── template.rs       # Template variable interpolation ({{ }})
```

### DAG Execution Model

1. **Topological sort** (Kahn's algorithm) — detects cycles, produces parallel levels
2. **Parallel execution** — same-level nodes run concurrently (semaphore: 16 max)
3. **Retry** — exponential backoff (1s/2s/4s/...) on failure
4. **Timeout** — per-node timeout via `tokio::time::timeout`
5. **Failure propagation** — failed nodes stop downstream unless `continue_on_error: true`

### Database

SQLite via `sqlx`. Two tables:

- `workflow_runs` — id, name, version, yaml_content, status, node_count, timestamps, error_message
- `node_runs` — id, run_id, node_id, node_type, status, attempt, timestamps, exit_code, stdout, stderr, error_message, outputs (JSON), depends (JSON array)

Default DB path: `acpx-g.db` (configurable via `DATABASE_URL`).

## Directory Watcher

When started with `--workflow-dir <DIR>`, the server watches a directory for `.yaml`/`.yml` files:

1. **First scan** — tracks all workflow names and versions (no submission)
2. **Subsequent scans** (every 10s) — submits a new run when a workflow's `version` field changes
3. **Template list** — all discovered workflows are exposed via `GET /api/v1/templates`
4. **Recursive** — scans subdirectories recursively

This enables a GitOps-style workflow: update a YAML file's `version`, and the watcher automatically submits a new run.

## CLI Options

| Option | Description |
|--------|-------------|
| `--workflow-dir <DIR>` | Watch directory for workflow YAML files (auto-submit on version change) |
| `--help`, `-h` | Show usage information |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `sqlite:acpx-g.db?mode=rwc` | SQLite connection string |
| `PORT` | `3000` | HTTP server port |
| `RUST_LOG` | — | Tracing log level (e.g. `info`, `debug`) |

## Dependencies

Zero dependencies on the existing agent framework. Fully standalone.

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP server + routing |
| `sqlx` | SQLite persistence (compile-time query checks) |
| `serde` + `serde_yaml` | YAML schema deserialization |
| `tokio` | Async runtime |
| `reqwest` | Remote workflow fetching |
| `tower-http` | CORS, tracing, static file serving |
| `tracing` | Structured logging |
| `uuid` | v7 run/node ID generation (time-sortable) |
| `chrono` | Timestamp handling |
