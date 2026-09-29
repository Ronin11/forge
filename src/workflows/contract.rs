//! The workflow-shape vocabulary: what a directive's contract may be
//! (docs/ACTIONS.md), what steps produce and consume, and the two things a
//! catalog entry can be — an action's `kind` and a workflow's `kind`.

use super::*;

/// Contracts the kernel enforces for directives. A directive file names
/// one (default: its own name); any other value is rejected. Many
/// directives over few contracts (docs/ACTIONS.md). Serialized by its
/// lowercase name, which is what the files and the stored JSON carry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contract {
    Code,
    Tests,
    Review,
    Plan,
}

impl Contract {
    pub const ALL: [Contract; 4] = [
        Contract::Code,
        Contract::Tests,
        Contract::Review,
        Contract::Plan,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Contract::Code => "code",
            Contract::Tests => "tests",
            Contract::Review => "review",
            Contract::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<Contract> {
        Contract::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Whether the directive is expected to change files; a read-only
    /// contract is never faulted for not editing.
    pub fn writes(self) -> bool {
        matches!(self, Contract::Code | Contract::Tests)
    }

    /// Whether the contract's verdict runs checks of its own (L1/L2 for
    /// code, red-on-base for tests). A contract that runs none is judged
    /// by its L0 rows alone: nothing to object to is a pass, not
    /// "unverified".
    pub fn verifies_work(self) -> bool {
        matches!(self, Contract::Code | Contract::Tests)
    }
}

impl std::fmt::Display for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What steps produce and consume: the data-flow vocabulary. `branch` is
/// the clone and every step that changes it; `verdict` is the kernel's
/// verify after a directive; the rest are one step's output shown to a
/// later one. Serialized by its file name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Product {
    Branch,
    VerifyRef,
    Interface,
    Verdict,
    Review,
    Plan,
    Context,
}

impl Product {
    pub const ALL: [Product; 7] = [
        Product::Branch,
        Product::VerifyRef,
        Product::Interface,
        Product::Verdict,
        Product::Review,
        Product::Plan,
        Product::Context,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Product::Branch => "branch",
            Product::VerifyRef => "verify_ref",
            Product::Interface => "interface",
            Product::Verdict => "verdict",
            Product::Review => "review",
            Product::Plan => "plan",
            Product::Context => "context",
        }
    }

    pub fn parse(s: &str) -> Option<Product> {
        Product::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl std::fmt::Display for Product {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an operation may produce. `branch`: it changes the tree, the
/// kernel commits the result and verifies it. `interface`: its stdout is
/// the interface the next code directive is shown. Everything else is a
/// directive's or the kernel's to produce.
pub const OPERATION_PRODUCES: &[Product] = &[Product::Branch, Product::Interface, Product::Context];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directive,
    Operation,
}

/// A workflow's `kind`: `build` (the default) runs a task to a landing;
/// `run` runs a job to a verified effect instead. See docs/JOBS.md.
/// Serialized by its lowercase name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowKind {
    #[default]
    Build,
    Run,
}

impl WorkflowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowKind::Build => "build",
            WorkflowKind::Run => "run",
        }
    }
}

impl std::fmt::Display for WorkflowKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_and_products_serialize_by_their_file_names() {
        // The stored resolved JSON and the action files carry these names;
        // the enums must round-trip them byte for byte.
        for c in Contract::ALL {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
            assert_eq!(serde_json::from_str::<Contract>(&json).unwrap(), c);
            assert_eq!(Contract::parse(c.as_str()), Some(c));
        }
        for p in Product::ALL {
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Product>(&json).unwrap(), p);
            assert_eq!(Product::parse(p.as_str()), Some(p));
        }
        assert_eq!(Product::VerifyRef.as_str(), "verify_ref");
        assert!(Contract::parse("verify").is_none());
        assert!(Product::parse("tests").is_none());
        assert!(Contract::Code.writes() && Contract::Tests.writes());
        assert!(!Contract::Review.writes() && !Contract::Plan.writes());
    }
}
