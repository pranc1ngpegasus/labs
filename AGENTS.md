# AGENTS.md

For every implementation task, you MUST follow the rules below.

## Implementation Rules

### 1. First Principle Thinking

- You MUST NOT accept existing assumptions, conventions, or requirements at face value.
- You MUST break everything down to the most fundamental truths and rebuild from there.
- You MUST question every requirement — no matter who gave it to you.

---

### 2. Musk's 5 Step Design Process

You MUST apply these 5 steps **in strict order**. You MUST NOT skip a step. You MUST NOT move to the next step before the current one is satisfied.

#### Step 1: Make the requirements less dumb

- You MUST NOT blindly implement requirements as given.
- Before anything else, you MUST ask: "Is this requirement actually necessary? Can it be simpler?"
- "Your requirements are definitely dumb, no matter who gave them to you."
- Requirements from a "smart person" are the most dangerous — you won't question them enough.
- If a requirement is unclear or irrational, you MUST **call it out and propose a better alternative**.

#### Step 2: Delete the part or process

- Before writing anything, you MUST ask: "What can I delete?"
- You MUST remove any code, file, config, process, or abstraction that isn't strictly necessary.
- "Just in case" is forbidden. You MUST NOT keep things "just in case." If you can add it back later, delete it now.
- "If you're not adding things back in at least 10% of the time, you're clearly not deleting enough."
- Every requirement must be owned by a person, not a department — so it can be questioned.

#### Step 3: Simplify or optimise

- You MUST NOT optimise before completing Steps 1 and 2.
- "The most common error of a smart engineer is to optimise a thing that should not exist."
- You MUST take a holistic view. Local optimisation that doesn't serve the whole is waste.
- Does this part even need to exist? If not, you MUST NOT optimise it — delete it.

#### Step 4: Accelerate cycle time

- You MUST only accelerate once you're sure you're moving in the right direction.
- "If you're digging your grave, don't dig faster."
- You SHOULD ship in small increments. Tighten the feedback loop.
- You MUST iterate fast only after the first three steps are satisfied.

#### Step 5: Automate

- You MUST automate **last**. You MUST NOT automate first.
- Automating something that shouldn't exist is the biggest waste.
- You MUST only automate when the what and the how are proven stable through the steps above.

---

### 3. Pre-implementation Checklist

Before writing any code, you MUST ask yourself:

1. [ ] Did I question the requirements? (Step 1)
2. [ ] Did I delete everything that can be deleted? (Step 2)
3. [ ] Am I about to optimise something that shouldn't exist? (Step 3)
4. [ ] Am I sure I'm moving in the right direction before accelerating? (Step 4)
5. [ ] Am I automating as the last step, not the first? (Step 5)

---

### 4. Commit Convention

- You MUST use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) for all commit messages.
- Format: `<type>(<scope>): <description>`
- Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `ci`, `build`, `revert`
- You MUST keep commits small and focused. One logical change per commit.

---

### 5. Post-implementation Improvement Iterations

After implementation is complete, you MUST run the improvement loop:

- You MUST execute **up to 3 iterations** of self-review and refinement.
- In each iteration, you MUST critically review the implementation and improve: readability, performance, error handling, edge cases, and test coverage.
- You MUST stop early (fewer than 3 iterations) only when no meaningful improvements remain.
- You MUST NOT skip this step. "Done" means iterated, not just compiled.

**Skill-based review (MUST do at least once):**

At least one improvement iteration MUST include a review using the project skills:

1. Load and apply `rust-code-reviewer` (`.agents/skills/rust-code-reviewer/SKILL.md`) to review the changes for correctness, safety, idioms, performance, and maintainability.
2. If the changes include public API surface (new or modified `pub` items), also load and apply `rust-api-ergonomics-reviewer` (`.agents/skills/rust-api-ergonomics-reviewer/SKILL.md`) to review from the downstream consumer's perspective.
3. Fix all findings classified as High severity before considering the iteration complete. Address Medium findings or document the rationale for deferring them.

---

### 6. Pull Request

- You MUST NOT open a PR before completing the improvement iterations (Section 5).
- You MUST ensure all commits follow Conventional Commits (Section 4) before opening the PR.
- PR title MUST follow Conventional Commits format.
- PR description MUST include: what was changed, why, and how it was validated.
