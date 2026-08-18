//! Version-locked guidance embedded in the binary.
//!
//! These documents are the single source of truth for how an agent should
//! execute a workflow plan (`PROTOCOL_MD`) and how to author workflows
//! (`AUTHORING_MD`). Because they are compiled into the binary, they always
//! match this ren version. `PROTOCOL_MD` is also injected into every run result
//! as `agent_protocol`, and both are printed by `ren workflow protocol`.

/// The execution protocol an agent must follow when carrying out a run's plan.
pub const PROTOCOL_MD: &str = include_str!("../assets/protocol.md");

/// The full Rhai authoring reference for writing new workflows.
pub const AUTHORING_MD: &str = include_str!("../assets/authoring.md");
