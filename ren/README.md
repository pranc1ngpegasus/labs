# ren

![A five-petaled lotus rooted in memory, with a continuous flow connecting bloom and roots](./assets/ren-hero.png)

> 記憶に根を張り、開発を連ね、智を錬る。

`ren` is a foundation for continuous development with coding agents. It
currently provides deterministic Rhai workflows and durable local memory, and
is intended to grow across other parts of the development flow.

## The five petals

At the center of `ren` is the **5-Step Engineering Process**. Its five steps
form the petals of a lotus and must be followed in order:

| Petal | Step | Practice |
| --- | --- | --- |
| 1 | **Make your requirements less dumb** | Question every requirement and make it earn its place. |
| 2 | **Delete the part or process** | Remove anything that does not need to exist. |
| 3 | **Simplify / Optimise** | Improve only what survives deletion. |
| 4 | **Accelerate cycle times** | Shorten the path from action to feedback. |
| 5 | **Automate** | Automate the cycle only after the earlier steps have shaped it. |

The process does not end at the fifth petal. What the workflow learns returns
to memory, and the next cycle begins with better-grounded requirements.

## The name

The name `ren` carries three connected ideas:

- **蓮 (lotus)** — in the Five Phases, water represents wisdom; the lotus
  roots in memory and lets the five-petaled process bloom from muddy
  information.
- **連 (connection)** — linking people, agents, context, and steps into a
  continuous development flow.
- **錬 (refinement)** — returning workflow experience to memory so that
  knowledge can be refined over time.

## ren-workflow

Discover the available workflows and their arguments before running one:

```console
ren workflow list
ren workflow show <name>
ren workflow schema <name>
ren workflow run <name-or-path> --args '<json>'
```

Run `ren workflow --help` for the complete command reference.

The `ren init` command installs the embedded skills into every supported coding
agent — Claude, Cursor, Codex, Grok, OpenCode, and pi — or a single one with
`ren init --agent <agent>` (user scope by default, `--project` for the current
repository). pi reads user-global skills from `~/.pi/agent/skills` and project
skills from `.pi/skills`.

The bundled `implement` workflow connects implementation work back to project
memory. It searches and inspects relevant prior notes, captures bounded fleeting
notes after implementation and fix passes, checks the change for contradictions
with remembered decisions during independent review, and promotes verified
knowledge through an inspect-then-explicitly-apply proposal. Its final report is
ready to adapt into a PR description and includes the memory operations used.
Projects without a registered memory vault still run; the report records that
memory work was skipped. Task text is limited to 500 characters so the
maximum-effort, eight-round execution plan remains bounded.

## ren-memory

Capture and retrieve durable project knowledge through the local ren-memory
vault:

```console
ren memory --help
ren memory search <query>
ren memory list
ren memory show <note-id>
```

The hyphenated `ren-memory` name refers to the skill and component; `ren memory`
is its CLI command group.

### Set up the Codex hook

From the project directory whose memory vault you want to register, run these
commands in order:

```console
ren memory init --user
ren memory index --rebuild
ren memory hook install --agent codex --user
```

The hook is installed in the configuration for the current `CODEX_HOME`. When
`CODEX_HOME` is set, the target is `$CODEX_HOME/config.toml`; otherwise it is
`$HOME/.codex/config.toml`. Run the hook installation command once for each
`CODEX_HOME` you use, because each one has a separate Codex configuration.
