// SPDX-License-Identifier: CC0-1.0

//! WHERE in a submitted policy document a refusal of that document is.
//!
//! S3 states the condition of such a refusal in its `<Message>` — a fixed sentence per error
//! class, naming nothing of the request — and locates the statement it is about in the
//! `<Detail>` of the same error document: an `Action` that statement carries and the `Sid`
//! it is named by, as the document itself spells them
//! (`Action "s3:GetObject" in Statement "AllowTheRead"`).
//!
//! One locator, so every reading that faults a policy document
//! ([`super::policy_action_fault`]) says where it is the same way and a client parses one
//! shape.

#[allow(unused_imports)]
use super::*;

/// The statement a policy-document refusal is about, as its `<Detail>` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyStatementLocation {
    /// The `Action` the statement is named by — the faulted one for a refusal about an
    /// action, and otherwise the first the statement carries. `None` for a statement naming
    /// none, which the grammar refuses before any of this is read.
    pub action: Option<String>,
    /// The `Sid` the statement is named by, when it states one.
    pub sid: Option<String>,
}

impl PolicyStatementLocation {
    /// Where `statement` is: the first `Action` it names, and its `Sid`.
    #[must_use]
    pub fn of(statement: &::serde_json::Map<String, ::serde_json::Value>) -> Self {
        Self {
            action: statement
                .get(ACTION_ELEMENT)
                .map(super::element_values)
                .unwrap_or_default()
                .first()
                .cloned(),
            sid: statement
                .get("Sid")
                .and_then(::serde_json::Value::as_str)
                .map(ToString::to_string),
        }
    }

    /// The same statement, named by `action` rather than by the first action it carries —
    /// what a refusal ABOUT one action of a statement names it by.
    #[must_use]
    pub fn naming(self, action: &str) -> Self {
        Self {
            action: Some(action.to_string()),
            ..self
        }
    }

    /// The `<Detail>` of the refusal: the action and the statement, both as the submitted
    /// document spells them. A statement that states no `Sid` has no name to quote, so the
    /// quoted name is empty — which is what says the statement the caller has to change
    /// states none of its own.
    #[must_use]
    pub fn detail(&self) -> String {
        let action = self.action.as_deref().unwrap_or_default();
        let sid = self.sid.as_deref().unwrap_or_default();
        format!("{ACTION_ELEMENT} \"{action}\" in Statement \"{sid}\"")
    }
}

/// The `Action` element, as a `<Detail>` names it.
pub const ACTION_ELEMENT: &str = "Action";
