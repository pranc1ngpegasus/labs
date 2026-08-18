# ren execution protocol

`ren workflow run` executes the workflow against ren's built-in **echo host**.
It does **not** spawn real agents. It deterministically returns the ordered plan:
every agent prompt, the phases, the budget spent, and the final `complete` value.
Treat that output as an authoritative execution plan, then carry it out for real.

## Rules

- **Execute every recorded call as a real subagent** — including the opening
  `planner`-style single `agent(...)` call. Never skip a slot, and never reuse the
  echoed prompt as if it were a real result.
- **Preserve order and grouping.** Sequential `agent(...)` calls run one after
  another; every item inside one `parallel([...])` runs concurrently as a group.
- **Thread real data.** Where a downstream prompt embeds an upstream result, the
  echo host records placeholder text (the earlier prompt strings). When you run
  for real, substitute the *actual* upstream subagent outputs instead.
- **Enforce structured outputs.** When a recorded request has `output_schema`,
  validate the real subagent output against it before committing the slot or
  threading the output downstream. If it is invalid, ask that same subagent to
  correct its output to the recorded schema. If it still cannot produce a valid
  result, fail the slot. Never silently repair, reinterpret, or forward a
  schema-invalid output.
- **One real subagent per recorded slot** (each `agent` = 1, each `parallel` item
  = 1). Report spent/total from the `complete` budget.
- **Capabilities.** The echo host only records requests, so it accepts every
  capability without performing the operation. When you execute the plan for
  real, honor each call's requested `capability_mode`; echo-host acceptance is
  not permission to broaden it.

## Example

For a plan `planner → parallel[facts, critique] → synthesizer`: run `planner` as a
subagent, feed its output into the two parallel research subagents, then feed all
three real outputs into the `synthesizer` subagent — one subagent per recorded
slot.
