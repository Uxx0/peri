---
name: langfuse
description: Interact with Langfuse and access its documentation. Use when needing to (1) query or modify Langfuse data programmatically via the CLI — traces, prompts, datasets, scores, sessions, and any other API resource, (2) look up Langfuse documentation, concepts, integration guides, or SDK usage, or (3) understand how any Langfuse feature works. This skill covers CLI-based API access (via bunx) and multiple documentation retrieval methods.
allowed-tools:
  - WebFetch(domain:langfuse.com)
  - Bash(curl *langfuse.com/*)
  - Bash(bunx langfuse-cli api __schema *)
  - Bash(bunx langfuse-cli api * --help *)
  - Bash(bunx langfuse-cli api * list *)
  - Bash(bunx langfuse-cli api * get *)
  - Bash(bun .claude/skills/langfuse/scripts/analyze.ts *)
---

# Langfuse

## 1. Langfuse API via CLI

Use `langfuse-cli` to interact with the full Langfuse REST API. Run via bunx (auto-loads `.env`):

```bash
bunx langfuse-cli api __schema                              # Discover all resources
bunx langfuse-cli api <resource> --help                     # List actions for a resource
bunx langfuse-cli api <resource> <action> --help            # Show args for an action
bunx langfuse-cli api <resource> <action> [options]         # Execute
```

### Credentials

bunx automatically loads `.env`. Ensure it contains:

```bash
LANGFUSE_PUBLIC_KEY=pk-lf-...
LANGFUSE_SECRET_KEY=sk-lf-...
LANGFUSE_HOST=https://cloud.langfuse.com  # Required
```

If credentials are missing, ask the user to add them to `.env`. Do not ask to paste keys in chat.

### CLI Tips

- Use `--json` for machine-readable output
- Use `--curl` to preview HTTP request without executing
- Prefer `observations-v2s` over `observations`, `score-v2s` over `scores`

## 2. Cost Analysis

### Analyze Script

```bash
bun .claude/skills/langfuse/scripts/analyze.ts [N]              # Overview + trace table + flags
bun .claude/skills/langfuse/scripts/analyze.ts --tools [N]      # Tool call analysis
bun .claude/skills/langfuse/scripts/analyze.ts --growth [N]     # Context growth trend
bun .claude/skills/langfuse/scripts/analyze.ts --report [N]     # Full report (all 7 sections)
bun .claude/skills/langfuse/scripts/analyze.ts --trace-id <id>  # Single trace detail
```

### Report Sections

| # | Section | What it shows |
|---|---------|---------------|
| 1 | Overview | Aggregate stats, cache efficiency, output/input ratio |
| 2 | Per-Trace Table | Input/output/cache/latency per trace |
| 3 | Tool Analysis | Frequency, avg latency, redundancy detection, tool→context growth |
| 4 | Context Growth | Per-trace token trend (visual bar chart), session accumulation, cross-trace growth rate |
| 5 | System Prompt Occupancy | Section breakdown with estimated tokens, system vs conversation ratio |
| 6 | Most Expensive Trace | Per-LLM-call detail with delta |
| 7 | Summary & Flags | Auto-detected issues (low cache, redundant tools, slow calls, etc.) |

### Red Flags

| Pattern | Threshold | Root Cause |
|---------|-----------|------------|
| Cache hit rate < 90% | Single trace | System prompt instability, cold start, or structure changing across turns |
| Effective new tokens > 20K | Single trace | Tool results or context growing unbounded |
| Output/Input ratio > 5% | Single trace | Model over-explaining |
| Output/Input ratio < 0.1% | Single trace | Massive input for tiny output — unnecessary context |
| LLM calls > 10 for simple task | Single trace | Agent looping or retrying |
| Single LLM call > 60s | Per-call | Model generating too much for the task |

### Optimization Checklist

After analysis, evaluate:

1. **System Prompt Weight** — >40% of context → trim; largest section → shorten or lazy-load; stale CLAUDE.md TRAPs → archive
2. **Context Accumulation** — tool results retained across turns?; micro-compact threshold right?; redundant reads?
3. **Agent Loop Efficiency** — redundant tool calls?; sequential reads → batch?; broad exploration → targeted search?
4. **Task Decomposition** — complex task → focused sub-tasks?; sub-agents to reduce context pressure?

### Reflection Output Format

```
## Cost Reflection

### Metrics
- Traces analyzed: N
- Total input: X tokens (Y% cache hit)
- Total output: Z tokens
- Avg LLM calls per trace: M

### Findings
1. [Pattern with specific trace example]
2. [Another pattern]

### Recommendations
1. [Actionable optimization] — estimated savings: ~X tokens/trace
2. [Another recommendation]
```

## 3. Langfuse Documentation

### 3a. Documentation Index (llms.txt)

```bash
curl -s https://langfuse.com/llms.txt
```

Returns structured list of every doc page. Use to discover the right page, then fetch it.

### 3b. Fetch Pages as Markdown

Append `.md` to any doc path:

```bash
curl -s "https://langfuse.com/docs/observability/overview.md"
```

### 3c. Search Documentation

```bash
curl -s "https://langfuse.com/api/search-docs?query=How+do+I+trace+LangGraph+agents"
```

Returns matching documents with URLs, titles, and excerpts. Also indexes GitHub Issues/Discussions.

### Workflow

1. Start with **llms.txt** to orient
2. **Fetch specific pages** when identified
3. Fall back to **search** when topic is unclear

## Use Case References

- instrumenting an application: references/instrumentation.md
- migrating prompts: references/prompt-migration.md
- user feedback as scores: references/user-feedback.md
- CLI tips: references/cli.md
- SDK upgrade: references/sdk-upgrade.md
- judge calibration: references/judge-calibration.md
- error analysis: references/error-analysis.md
- skill feedback: references/skill-feedback.md
