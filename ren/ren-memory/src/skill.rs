use ren_workflow::{EmbeddedSkill, SkillFile};

/// The embedded Agent Skills entrypoint for `ren-memory`.
pub const MEMORY_SKILL_MD: &str = include_str!("../assets/skill/ren-memory/SKILL.md");

/// UI-facing metadata installed alongside [`MEMORY_SKILL_MD`].
pub const MEMORY_OPENAI_YAML: &str = include_str!("../assets/skill/ren-memory/agents/openai.yaml");

/// Every file that makes up the embedded `ren-memory` skill.
pub const MEMORY_SKILL_FILES: &[SkillFile] = &[
    SkillFile {
        relative: "SKILL.md",
        contents: MEMORY_SKILL_MD,
    },
    SkillFile {
        relative: "agents/openai.yaml",
        contents: MEMORY_OPENAI_YAML,
    },
];

/// The embedded `ren-memory` skill definition.
pub const MEMORY_SKILL: EmbeddedSkill = EmbeddedSkill {
    name: "ren-memory",
    files: MEMORY_SKILL_FILES,
};
