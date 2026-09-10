//! Workflows are data: an ordered list of agent steps. The kernel verifies
//! after every step and pushes after the last; those are not steps because
//! they are not optional. See docs/WORKFLOWS.md.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// An agent writes the change in the task's clone.
    Code,
    /// An agent writes tests, only inside the verification namespace, that
    /// fail on the base commit; its summary becomes the coder's interface.
    Tests,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Code => "code",
            Step::Tests => "tests",
        }
    }
}

pub struct Workflow {
    pub name: &'static str,
    pub steps: &'static [Step],
    pub blurb: &'static str,
}

pub const WORKFLOWS: &[Workflow] = &[
    Workflow {
        name: "direct",
        steps: &[Step::Code],
        blurb: "one agent writes the change; the kernel verifies",
    },
    Workflow {
        name: "tdd",
        steps: &[Step::Tests, Step::Code],
        blurb: "one agent writes hidden tests that fail on base; another makes them pass seeing only the interface",
    },
];

pub fn get(name: &str) -> Option<&'static Workflow> {
    WORKFLOWS.iter().find(|w| w.name == name)
}
