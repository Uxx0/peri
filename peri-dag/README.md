# peri-dag

DAG workflow engine — YAML-defined workflows, web API, SQLite persistence.

## Quick Start

```bash
# Start the server
DATABASE_URL="sqlite:peri-dag.db?mode=rwc" cargo run -p peri-dag

# Or with default settings
cargo run -p peri-dag
```

Server starts on `http://0.0.0.0:3000` (configurable via `PORT` env).

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/workflows` | Submit and execute a workflow |
| `GET` | `/api/v1/workflows` | List recent workflow runs |
| `GET` | `/api/v1/workflows/{run_id}` | Get run details + all node statuses |
| `GET` | `/api/v1/workflows/{run_id}/nodes/{node_id}/logs` | Get node stdout/stderr |

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
    run: "git clone https://github.com/org/repo.git && cd repo && git checkout {{ inputs.tag }}"
    outputs:
      repo_dir: "./repo"

  # Shell: external file
  - id: build
    type: shell
    run: { file: "./scripts/build.sh" }
    depends: [checkout]
    timeout: 600
    retry: 1

  # Shell: platform-specific scripts
  - id: deploy
    type: shell
    run:
      linux: "./scripts/deploy-linux.sh"
      macos: "./scripts/deploy-macos.sh"
      windows: "./scripts/deploy.ps1"
      default: "./scripts/deploy.sh"
    depends: [build]

  # Agent: wraps acpx CLI
  - id: review
    type: agent
    prompt: "Review changes for tag {{ inputs.tag }}"
    model: sonnet
    cwd: "{{ needs.checkout.outputs.repo_dir }}"
    depends: [build]

  # Agent: external prompt file
  - id: summarize
    type: agent
    prompt: { file: "./prompts/summarize.md" }
    depends: [review]

  # Reference: call another workflow
  - id: call-notify
    type: reference
    ref: notify
    with:
      channel: "#deploy"
    depends: [deploy]
```

## Node Types

| Type | Description |
|------|-------------|
| `shell` | Execute inline script, external file, or platform-specific scripts |
| `agent` | Wrap `acpx` CLI as a node (inline prompt, external file, or platform-specific) |
| `reference` | Call another workflow by alias (local path or HTTPS URL) |

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
| `ci-pipeline.yaml` | Parent workflow referencing build + notify |
| `build-lib.yaml` | Reusable build sub-workflow (checkout → build → test) |
| `notify.yaml` | Reusable notification sub-workflow |
| `simple-ci.yaml` | Standalone workflow (no references) |

**Before expansion** (`ci-pipeline.yaml`):

```
do-build ─────────────────────┐    (reference node)
notify-build-ok ──────┐       │    (reference node)
deploy ───────────────┼───────┤    (depends: do-build/build)
review ───────────────┼───────┤    (depends: do-build/checkout)
notify-done ──────────┴───────┘    (depends: deploy, review, notify-build-ok)
```

**After expansion** (flat DAG):

```
do-build/checkout ──→ do-build/build ──→ do-build/test
       │                     │
       ├─→ review            ├─→ deploy
       │                     │
notify-ok/send ←─────────────┼──────── (depends: do-build exit nodes)
       │
notify-done/send ←───────────┴──────── (depends: deploy, review, notify-ok/send)
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
peri-dag/
├── Cargo.toml
├── README.md
└── src/
    ├── main.rs            # axum HTTP server
    ├── lib.rs             # crate root
    ├── schema.rs          # YAML schema types + platform resolution
    ├── db/
    │   ├── mod.rs         # SQLite init + migrations
    │   └── models.rs      # WorkflowRun, NodeRun, API types
    ├── api/
    │   ├── mod.rs
    │   └── workflows.rs   # axum handlers
    └── runner/
        ├── mod.rs         # DAG scheduler (topological sort, parallel exec)
        ├── executor.rs    # Shell + agent execution
        └── loader.rs      # YAML loading + reference resolution
```

### DAG Execution Model

1. **Topological sort** (Kahn's algorithm) — detects cycles, produces parallel levels
2. **Parallel execution** — same-level nodes run concurrently (semaphore: 16 max)
3. **Retry** — exponential backoff (1s/2s/4s/...) on failure
4. **Timeout** — per-node timeout via `tokio::time::timeout`
5. **Failure propagation** — failed nodes stop downstream unless `continue_on_error: true`

### Database

SQLite via `sqlx`. Two tables:

- `workflow_runs` — id, name, version, yaml_content, status, timestamps
- `node_runs` — id, run_id, node_id, node_type, status, attempt, stdout, stderr, exit_code

Default DB path: `peri-dag.db` (configurable via `DATABASE_URL`).

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DATABASE_URL` | `sqlite:peri-dag.db?mode=rwc` | SQLite connection string |
| `PORT` | `3000` | HTTP server port |
| `RUST_LOG` | — | Tracing log level (e.g. `info`, `debug`) |

## Dependencies

Zero dependencies on the existing agent framework. Fully standalone.

| Crate | Purpose |
|-------|---------|
| `axum` | HTTP server + routing |
| `sqlx` | SQLite persistence |
| `serde` + `serde_yaml` | YAML schema deserialization |
| `tokio` | Async runtime |
| `reqwest` | Remote workflow fetching |
| `tracing` | Structured logging |
| `uuid` | Run/node ID generation |
