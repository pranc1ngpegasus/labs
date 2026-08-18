---
name: ren-workflow
description: >
  Discover, inspect, run, and author ren workflows — deterministic Rhai scripts
  that orchestrate coding-agent calls. This is a thin bootstrap: the ren binary
  is the source of truth, so consult its `--help` and obey the `agent_protocol`
  it prints. Use when the user wants to use ren, run a ren workflow,
  list/show/inspect/create ren workflows, install this skill (ren init), or
  runs /ren-workflow. Trigger phrases: "ren", "ren workflow",
  "ren workflow run/list/create/init", ".ren/workflows", "rhai workflow".
---

# ren-workflow

`ren` runs **deterministic** workflows written in Rhai that orchestrate
coding-agent calls. This skill is intentionally thin: **the `ren` binary is the
single source of truth.** Rely on its `--help` for the current command surface
and obey the version-matched instructions it prints at runtime.

Assume `ren` is installed and on `PATH`; call it directly. If a command reports
that `ren` is not found, ask the user to install it (e.g. `cargo install --path
ren` from the ren repo) rather than guessing a path.

## Step 1: Learn the current command surface

Do not hard-code flags from memory — ask the binary:

```bash
ren --help
ren workflow --help          # run | list | show | schema | create | init | protocol | bridge
ren workflow <sub> --help    # exact args for any subcommand
```

Discover and inspect workflows with `ren workflow list`, then
`ren workflow show <name>` / `ren workflow schema <name>` to learn the required
`args` before running.

## Step 2: Run a workflow, then obey `agent_protocol`

```bash
ren workflow run <name-or-path> --args '<json>'
```

The JSON result includes an **`agent_protocol`** field. **Read it and follow it
exactly** — it is embedded in the binary, so it always matches this ren version
and governs how to execute the returned plan (run every recorded call for real,
preserve order and parallel grouping, thread real upstream outputs downstream,
one subagent per slot). You can also print it standalone:

```bash
ren workflow protocol
```

## Step 3: Authoring and installation

- **Author a new workflow:** scaffold with `ren workflow create <name> --user`
  (or `--project`, `--from <bundled>`), then read the DSL reference:
  `ren workflow protocol --authoring`. Validate by running it (Step 2).
- **Install this skill into other agents:** `ren init` (all supported agents,
  user scope) or `ren init --agent <a> [--project] [--force]`.

## Guidelines

- The binary is the source of truth: prefer `--help` and the printed
  `agent_protocol` over anything memorized here.
- Never invent workflow names — discover them with `list`.
- Prefer `schema`/`show` before `run` so `--args` matches the declared schema.
