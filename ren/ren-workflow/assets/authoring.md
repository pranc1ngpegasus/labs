# Authoring ren workflows

A ren workflow is one `.rhai` file. Registry locations are:

- Project: `<repo-root>/.ren/workflows/<name>.rhai`
- User: `~/.ren/workflows/<name>.rhai`
- Bundled: compiled into the `ren` binary

Project entries shadow user entries, which shadow bundled entries with the same
`meta.name`.

## Complete starter template

```rhai
let meta = #{
    name: "review-change",
    description: "Reviews a code change from independent perspectives",
    when_to_use: "Use when a code change needs correctness and maintainability review",
    args_schema: #{
        type: "object",
        required: ["change"],
        properties: #{
            change: #{ type: "string" },
            strict: #{ type: "boolean" }
        }
    },
    phases: [
        #{ title: "Review", detail: "Run independent review passes" },
        #{ title: "Synthesize", detail: "Combine findings" }
    ]
};

phase("Review");
let findings = parallel([
    #{
        prompt: "Review correctness and edge cases:\n" + args.change,
        label: "correctness",
        phase: "Review",
        capability_mode: "read-only"
    },
    #{
        prompt: "Review maintainability and clarity:\n" + args.change,
        label: "maintainability",
        phase: "Review",
        capability_mode: "read-only"
    }
]);
log("Independent reviews complete");

phase("Synthesize");
let synthesis = agent(
    "Synthesize and deduplicate these findings:\n" +
        findings[0].output + "\n---\n" + findings[1].output,
    #{ label: "synthesizer", phase: "Synthesize", capability_mode: "read-only" }
);

complete(#{
    review: synthesis.output,
    budget: budget()
});
```

## Mandatory metadata

The **first statement** must be exactly a pure-literal declaration shaped like:

```rhai
let meta = #{ ... };
```

It may contain only literal maps, arrays, strings, numbers, and booleans — no
function calls, variables, or computed expressions.

Fields:

| Field | Required | Meaning |
|---|---:|---|
| `name` | yes | Stable lowercase ASCII letters/digits/hyphens. Must equal file stem. |
| `description` | yes | Short human-readable purpose. Used by discovery/tool descriptors. |
| `when_to_use` | no | Selection guidance for coding agents. |
| `args_schema` | no | JSON-Schema-like top-level map for validating `args`. |
| `phases` | no | Array of `#{ title, detail }` maps for user-facing execution stages. |

Supported argument validation is deliberately small:

- Top-level `type`
- `required` field presence
- `properties.<field>.type`
- `properties.<field>.minLength` for strings
- `properties.<field>.minimum` / `maximum` for numbers and integers
- Types: `object`, `array`, `string`, `boolean`, `null`, `number`, `integer`

Nested schemas, enums, formats, and schema composition are not enforced.

## Globals

### `args`

The JSON supplied by `--args`, converted to a Rhai value. For object schemas,
an omitted value defaults to an empty map. Access fields as `args.field`.

## Host API

### `agent(prompt [, options]) -> map`

Declares one sequential coding-agent call. It consumes one budget slot.

```rhai
let result = agent("Inspect the parser", #{
    label: "parser-review",
    phase: "Review",
    capability_mode: "read-only",
    agent_type: "explore",
    model: "optional-host-model",
    output_schema: #{
        type: "object",
        required: ["findings"],
        properties: #{ findings: #{ type: "array" } }
    }
});
```

All options are optional strings except `output_schema`, which may be any
JSON-compatible literal:

- `label`: human-readable invocation label
- `phase`: phase association
- `capability_mode`: `read-only`, `read-write`, `execute`, or `all`
- `agent_type`: requested host-specific agent type
- `model`: requested host-specific model
- `output_schema`: structured-output schema that real executors must validate
  before committing the slot or threading its output downstream

Result fields:

```text
agent_id: string
success: boolean
output: string
cancelled: boolean
tokens_used: integer
duration_ms: integer
```

The CLI's built-in echo host returns the prompt itself as `output`; it does not
launch a real agent or validate echoed placeholders against `output_schema`.
The version-matched execution protocol requires the real executor to validate
actual agent outputs and fail closed when correction by the same subagent does
not produce a conforming result.

### `parallel(items) -> array`

Declares independent calls as one parallel group. Each item consumes one budget
slot. Each item is a map containing a required string `prompt` plus the same
options accepted by `agent`.

```rhai
let results = parallel([
    #{ prompt: "Check correctness", label: "correctness", capability_mode: "read-only" },
    #{ prompt: "Check security", label: "security", capability_mode: "read-only" }
]);
```

A host failure for an individual slot may produce a unit/null-like result, so a
production workflow should account for unavailable slots if the host can fail.

### `phase(title)`

Appends a phase title to the execution result. Prefer titles declared in
`meta.phases` and call in execution order.

### `log(message)`

Appends a user-facing log message to the result. Logging has no host-call cost.

### `complete(value)`

Stores the JSON-compatible final result and stops the workflow successfully.
Prefer a map with named fields. Exactly one terminal path should call it.

### `pause(kind, message)`

Stops immediately with a `paused` result. This is an unjournaled explicit pause.

### `await_user(kind, message)`

Creates a journaled user gate. On the first run it records the gate and pauses.
On resume with that journal, the same call is replayed, returns unit, and
execution continues. The script and all earlier deterministic calls must remain
unchanged or journal-divergence validation will fail.

### `budget() -> map`

Returns:

```text
total, spent, reserved, remaining
```

Use it for reporting or deciding whether to launch optional work. `reserved` is
currently always zero.

### `json_encode(value) -> string`

Deterministically serializes a JSON-compatible Rhai value for inclusion in an
agent prompt.

### `fingerprint(text) -> string`

Returns the SHA-256 digest as lowercase hexadecimal text.

### Scratch files

```rhai
write_scratch_file("plan.txt", "content");
let content = read_scratch_file("plan.txt");
```

These are deterministic, in-memory workflow scratch entries journaled for
replay. Names must be a single safe filename: not empty, `.`, `..`, and no `/`,
`\\`, or NUL. They do **not** read or write arbitrary project files.

## Capability rules

Capabilities are ordered:

```text
read-only < read-write < execute < all
```

Every host decides the maximum it grants. A request above that maximum fails.
The check applies during journal replay too, so replay cannot bypass current
permissions. The CLI echo host accepts every capability because it only records
the plan and performs no requested operation. Real executors must still enforce
the capability recorded on each call.

For actual coding agents, use `read-only` for inspection, `read-write` for local
file changes, `execute` where command execution is needed, and `all` only when a
host-specific operation truly requires it.

## Budget rules

- Run budget range: 1–1024; default 128.
- `agent(...)`: one slot.
- `parallel([...])`: one slot per array item.
- A parallel group is rejected as a whole if it exceeds remaining capacity.
- Replayed calls also consume budget slots.

Keep fan-out bounded and proportionate. Avoid generating an array of requests
from unconstrained user input.

## Determinism and replay

Gyre records host calls in an ordered journal. On resume, requests must exactly
match the recorded calls. Changing a prompt, options, call kind, call order,
scratch content, or gate produces journal divergence.

Use `--journal run.json` to atomically checkpoint the journal after every
committed agent result, parallel slot, scratch effect, or user gate. If the
process or host aborts, resume with:

```bash
ren workflow run <name-or-path> --args '<same-json>' --resume run.json
```

Successful sequential calls and completed parallel slots are replayed without
calling the host again. Pending parallel slots continue from the checkpoint.
When a committed result is failed, cancelled, or infrastructure-failed, add
`--retry-failed`: Gyre preserves successful work before the first failure,
resets only failed slots in that parallel group, and discards later dependent
entries so they are recomputed. Unless `--journal` names a different output,
the resume file continues to receive checkpoints.

These nondeterministic functions are explicitly unavailable:

```text
timestamp(), sleep(...), rand(), random(), rand_int(...)
```

Do not rely on current time, random values, ambient file contents, or unordered
iteration to shape agent calls. Pass changing data explicitly through `args`.

## Validation checklist

1. The first statement is a pure-literal `let meta = #{ ... };`.
2. `meta.name` is lowercase/digits/hyphens and equals the `.rhai` file stem.
3. `args_schema` accurately describes every accessed argument.
4. Each declared phase is emitted in logical order.
5. Independent calls use `parallel`; dependent calls use sequential `agent`.
6. Every call requests the minimum capability needed.
7. Worst-case call count fits the budget.
8. All terminal success paths call `complete` with JSON-compatible values.
9. No nondeterministic function or ambient input shapes replayed calls.
10. Run `ren workflow show`, `schema`, and `run` to inspect and test the plan.
