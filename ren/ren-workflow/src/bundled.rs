/// An official workflow embedded in the `ren` binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundledWorkflow {
    pub name: &'static str,
    pub file_name: &'static str,
    pub source: &'static str,
}

pub const WORKFLOWS: &[BundledWorkflow] = &[
    BundledWorkflow {
        name: "deep-research",
        file_name: "deep-research.rhai",
        source: include_str!("../bundled/deep-research.rhai"),
    },
    BundledWorkflow {
        name: "implement",
        file_name: "implement.rhai",
        source: include_str!("../bundled/implement.rhai"),
    },
];

#[must_use]
pub fn find(name: &str) -> Option<&'static BundledWorkflow> {
    WORKFLOWS.iter().find(|workflow| workflow.name == name)
}
