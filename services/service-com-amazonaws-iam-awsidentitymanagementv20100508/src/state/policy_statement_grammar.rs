// SPDX-License-Identifier: CC0-1.0

//! What a policy document's STATEMENTS must and must not carry, for each of the two kinds
//! of document this API takes: the IDENTITY-based policy a managed or inline policy holds,
//! and the RESOURCE-based policy a role's trust document is.
//!
//! The top-level grammar — a JSON object carrying `Statement` — is
//! [`super::policy_document_defect`]; this is the reading BELOW it, in one place, because
//! the two kinds differ in exactly one element and otherwise state the same thing:
//!
//! * an identity-based policy's statements name the actions they are about and the
//!   resources those actions apply to, and name NO principal — "you do not specify a
//!   principal in an identity-based policy, because the principal is the user or role that
//!   the policy is attached to" ("IAM JSON policy elements: Principal", IAM User Guide), so
//!   a document that states one describes a grant IAM cannot read as an identity policy;
//! * a resource-based policy's statements DO name a principal — "you must specify a
//!   principal in a resource-based policy", and a role's trust policy "is the resource-based
//!   policy that is attached to the role" — so a trust statement naming none trusts nobody.
//!
//! A document whose `Statement` element describes NO statement at all is likewise no
//! MANAGED policy: the element is what carries the permissions, so a document whose list is
//! empty grants nothing and is refused where one with a complete statement is stored. An
//! INLINE policy and a trust document are not held to that: `Statement` present and empty is
//! a document the top-level grammar admits. What the two IDENTITY readings ARE held to is
//! that a statement is a statement: the element carries "a statement … or an array of
//! individual statements", each an object of policy elements, so a `Statement` carrying a
//! string, a number or a list with a scalar in it describes none and is refused as an error
//! in the policy's syntax. A TRUST document is not held to that either — recorded behaviour
//! of a trust-policy write is that such a document is stored — so its reading holds the
//! statements it can read to the principal clause and refuses nothing for the rest.
//!
//! Which of the three readings below an operation asks is therefore the KIND of document it
//! stores, and the no-principal clause is common to all the identity ones:
//!
//! * [`identity_policy_document_defect`] — a managed policy's document
//!   (`CreatePolicy`, `CreatePolicyVersion`);
//! * [`inline_policy_document_defect`] — an inline policy's document (`PutUserPolicy`,
//!   `PutRolePolicy`, `PutGroupPolicy`);
//! * [`trust_policy_document_defect`] — a role's trust policy (`CreateRole`,
//!   `UpdateAssumeRolePolicy`).
//!
//! Each clause is answered with the modelled `MalformedPolicyDocumentException` (awsQuery
//! code `MalformedPolicyDocument`, HTTP 400) and the sentence
//! [`super::PolicyDocumentDefect`] words it with, so a document AWS refuses is refused here
//! with the same code and the same text, and nothing is stored.

#[allow(unused_imports)]
use super::*;

/// The fixed sentence IAM refuses an IDENTITY-based policy document whose statement names a
/// principal with. Fixed text throughout: it names neither the statement nor the element's
/// value.
pub const IDENTITY_POLICY_STATES_PRINCIPAL: &str = "Policy document should not specify a principal.";

/// The sentence IAM refuses a policy document that omits an element the policy language
/// requires of it with — the `Principal` a resource-based policy, and so a role's trust
/// policy, states the trusted party in.
///
/// It is the wording every service in this tree answers a missing policy element with
/// (`::service_common::iam_policy::AttachedPolicyDefect::MissingElement`, "Missing required
/// field <element>"), with the element this clause is about named.
pub const POLICY_WITHOUT_PRINCIPAL: &str = "Missing required field Principal";

/// The elements the IAM JSON policy grammar PROHIBITS in a statement of an IDENTITY-based
/// policy: the principal element and its complement, in both spellings the grammar admits.
const PROHIBITED_IDENTITY_STATEMENT_ELEMENTS: &[&str] = &["Principal", "NotPrincipal"];

/// The elements a statement of a RESOURCE-based policy names the trusted party under —
/// the same two spellings, which that kind of document REQUIRES one of.
const PRINCIPAL_STATEMENT_ELEMENTS: &[&str] = &["Principal", "NotPrincipal"];

/// The statements `document` declares, or the defect that says it declares none.
///
/// `Statement` is read both as the LIST and as the single statement object the grammar
/// admits, so a document written either way is held to the same per-statement rules.
fn declared_statements(
    document: &str,
) -> Result<Vec<::serde_json::Value>, PolicyDocumentDefect> {
    let statement = policy_statements(document)?;
    Ok(match statement {
        ::serde_json::Value::Array(entries) => entries,
        single => vec![single],
    })
}

/// Why `document` is no IDENTITY-based policy document a MANAGED policy can hold, or
/// `None` when it is one.
///
/// The ONE definition of the rule, applied by both operations of this service that store a
/// managed policy's document — `CreatePolicy` and `CreatePolicyVersion` — so one document
/// is judged one way whichever of them was handed it. An INLINE policy's document is held
/// to the clause they share ([`inline_policy_document_defect`]).
pub fn identity_policy_document_defect(document: &str) -> Option<PolicyDocumentDefect> {
    let statements = match declared_statements(document) {
        Ok(statements) => statements,
        Err(defect) => return Some(defect),
    };
    // A managed policy's permissions ARE its statements, so a document describing none
    // describes no permission at all.
    if statements.is_empty() {
        return Some(PolicyDocumentDefect::NoStatements);
    }
    for statement in &statements {
        let Some(elements) = statement.as_object() else {
            // A statement that is not an object at all is no statement — the SYNTAX
            // defect, not a missing or a prohibited element.
            return Some(PolicyDocumentDefect::Syntax);
        };
        if states_principal(elements) {
            return Some(PolicyDocumentDefect::StatesPrincipal);
        }
        for (spellings, defect) in IDENTITY_STATEMENT_ELEMENTS {
            if !spellings
                .iter()
                .any(|spelling| names_something(elements.get(*spelling)))
            {
                return Some(*defect);
            }
        }
    }
    None
}

/// Why `document` is no document an INLINE policy can hold, or `None` when it is one.
///
/// An inline policy is an identity-based policy too — "an inline policy … is embedded in
/// the identity" and grants that identity its permissions — so it is the top-level grammar
/// every policy document is held to ([`super::policy_document_defect`]), plus the two things
/// that hold of a STATEMENT of such a document whatever else it says: a statement is a JSON
/// object (the grammar's `Statement` element is "a statement … or an array of individual
/// statements", each of which is an object of policy elements), and no statement of an
/// identity-based policy states a principal. A `Statement` element carrying anything else —
/// a string, a number, a list with a scalar in it — describes no statement at all, which is
/// an error in the policy's syntax rather than a missing or a prohibited element.
///
/// The ONE definition of that, shared by the three operations that embed one.
pub fn inline_policy_document_defect(document: &str) -> Option<PolicyDocumentDefect> {
    let statements = match declared_statements(document) {
        Ok(statements) => statements,
        Err(defect) => return Some(defect),
    };
    statements.iter().find_map(|statement| {
        let Some(elements) = statement.as_object() else {
            return Some(PolicyDocumentDefect::Syntax);
        };
        states_principal(elements).then_some(PolicyDocumentDefect::StatesPrincipal)
    })
}

/// Whether one statement NAMES A PRINCIPAL, in either spelling the grammar admits
/// ([`PROHIBITED_IDENTITY_STATEMENT_ELEMENTS`]).
fn states_principal(elements: &::serde_json::Map<String, ::serde_json::Value>) -> bool {
    PROHIBITED_IDENTITY_STATEMENT_ELEMENTS
        .iter()
        .any(|spelling| names_something(elements.get(*spelling)))
}

/// Why `document` is no document a role's TRUST policy can hold, or `None` when it is one.
///
/// The top-level grammar plus the one element a resource-based policy's statements must
/// name: the principal the role trusts. A `Statement` element that carries something other
/// than a statement or a list of statements is NOT refused here, unlike for an
/// identity-based document ([`inline_policy_document_defect`]): recorded behaviour of a
/// trust-policy write is that such a document is stored
/// (`tests/all/role_lifecycle.rs`), so this validator reads the statements it can and holds
/// each of them to the principal clause. The ONE definition of the rule, shared by both
/// operations that write the member — `CreateRole` and `UpdateAssumeRolePolicy` — so a
/// document one of them refuses is refused by the other.
pub fn trust_policy_document_defect(document: &str) -> Option<PolicyDocumentDefect> {
    let statements = match declared_statements(document) {
        Ok(statements) => statements,
        Err(defect) => return Some(defect),
    };
    statements.iter().find_map(|statement| {
        let elements = statement.as_object()?;
        (!PRINCIPAL_STATEMENT_ELEMENTS
            .iter()
            .any(|spelling| names_something(elements.get(*spelling))))
        .then_some(PolicyDocumentDefect::NoPrincipals)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an identity-based policy's every statement names actions and resources and no
    /// principal, and a MANAGED policy's document declares at least one statement.
    #[test]
    fn an_identity_policy_is_held_to_its_statement_elements() {
        // Rows differ along what the rule generalises over: which element is at fault,
        // which spelling carries it, the form `Statement` takes (a list or the single
        // object), the position of the offending statement, and the form the principal
        // element takes (an object, a string, a list).
        let rows: [(&str, &str, Option<PolicyDocumentDefect>); 12] = [
            (
                "a complete statement",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#,
                None,
            ),
            (
                "the single statement object the grammar admits",
                r#"{"Statement":{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}}"#,
                None,
            ),
            (
                "two complete statements",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:Get*","Resource":"*"},{"Effect":"Deny","NotAction":"s3:Put*","NotResource":"arn:aws:s3:::a/*"}]}"#,
                None,
            ),
            (
                "no statement at all",
                r#"{"Version":"2012-10-17","Statement":[]}"#,
                Some(PolicyDocumentDefect::NoStatements),
            ),
            (
                "no Statement element",
                r#"{"Version":"2012-10-17"}"#,
                Some(PolicyDocumentDefect::NoStatements),
            ),
            (
                "a text that is no JSON object",
                "not a policy",
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a statement that is not an object",
                r#"{"Statement":[17]}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a principal written as an object",
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"*"},"Action":"s3:GetObject","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::StatesPrincipal),
            ),
            (
                "a principal written as a string",
                r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::StatesPrincipal),
            ),
            (
                "the complement spelling, in the second statement",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:Get*","Resource":"*"},{"Effect":"Deny","NotPrincipal":{"AWS":["arn:aws:iam::000000000000:root"]},"Action":"s3:Put*","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::StatesPrincipal),
            ),
            (
                "a statement naming no action",
                r#"{"Statement":[{"Effect":"Allow","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::NoActions),
            ),
            (
                "a statement naming no resource",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:GetObject"}]}"#,
                Some(PolicyDocumentDefect::NoResources),
            ),
        ];

        for (case, document, expected) in rows {
            let defect = identity_policy_document_defect(document);
            assert_eq!(
                defect, expected,
                "case {case:?}: expected {expected:?}, got {defect:?} for {document}"
            );
        }
    }

    /// RULE: an INLINE policy's document is held to the top-level grammar, to a statement
    /// being a statement at all, and to the one clause no identity-based document may break
    /// — a statement of it states no principal — and to nothing below that.
    #[test]
    fn an_inline_policy_is_held_to_naming_no_principal() {
        let rows: [(&str, &str, Option<PolicyDocumentDefect>); 12] = [
            (
                "a complete statement",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#,
                None,
            ),
            // The smallest document the top-level grammar admits: an inline write stores
            // it, which is what keeps the principal clause from being a blanket refusal.
            ("no statement at all", r#"{"Statement":[]}"#, None),
            (
                "a statement naming no action or resource",
                r#"{"Statement":[{"Effect":"Allow"}]}"#,
                None,
            ),
            (
                "no Statement element",
                r#"{"Version":"2012-10-17"}"#,
                Some(PolicyDocumentDefect::NoStatements),
            ),
            // A `Statement` element that is not a statement and not a list of statements:
            // the shapes a caller reaches by serializing something other than a policy.
            (
                "a Statement element carrying a string",
                r#"{"Version":"2012-10-17","Statement":"Allow"}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a Statement element carrying a number",
                r#"{"Statement":17}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a Statement element carrying null",
                r#"{"Statement":null}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a statement list carrying a string",
                r#"{"Statement":["Allow"]}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a statement list carrying a list",
                r#"{"Statement":[[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]]}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a complete statement beside one that is no object",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"},true]}"#,
                Some(PolicyDocumentDefect::Syntax),
            ),
            (
                "a principal written as an object",
                r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"*"},"Action":"s3:GetObject","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::StatesPrincipal),
            ),
            (
                "the complement spelling in the second statement",
                r#"{"Statement":[{"Effect":"Allow","Action":"s3:Get*","Resource":"*"},{"Effect":"Deny","NotPrincipal":"*","Action":"s3:Put*","Resource":"*"}]}"#,
                Some(PolicyDocumentDefect::StatesPrincipal),
            ),
        ];

        for (case, document, expected) in rows {
            let defect = inline_policy_document_defect(document);
            assert_eq!(
                defect, expected,
                "case {case:?}: expected {expected:?}, got {defect:?} for {document}"
            );
        }
    }

    /// RULE: a trust policy's every statement names the principal the role trusts; the
    /// top-level grammar is the one every policy document is held to.
    #[test]
    fn a_trust_policy_is_held_to_naming_a_principal() {
        let rows: [(&str, &str, Option<PolicyDocumentDefect>); 9] = [
            (
                "a service principal",
                r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
                None,
            ),
            (
                "the complement spelling",
                r#"{"Statement":[{"Effect":"Deny","NotPrincipal":{"AWS":"*"},"Action":"sts:AssumeRole"}]}"#,
                None,
            ),
            (
                "the single statement object the grammar admits",
                r#"{"Statement":{"Effect":"Allow","Principal":"*","Action":"sts:AssumeRole"}}"#,
                None,
            ),
            // `Statement` present and empty is a document the top-level grammar admits,
            // and it holds no statement to require a principal of.
            ("no statement at all", r#"{"Statement":[]}"#, None),
            (
                "a statement naming no principal",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sts:AssumeRole"}]}"#,
                Some(PolicyDocumentDefect::NoPrincipals),
            ),
            (
                "the second statement naming none",
                r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"},{"Effect":"Allow","Action":"sts:AssumeRole"}]}"#,
                Some(PolicyDocumentDefect::NoPrincipals),
            ),
            (
                "a principal element naming nothing",
                r#"{"Statement":[{"Effect":"Allow","Principal":[],"Action":"sts:AssumeRole"}]}"#,
                Some(PolicyDocumentDefect::NoPrincipals),
            ),
            // A `Statement` carrying something that is no statement: an identity-based
            // document is refused for it, a trust document is not (see the reading above).
            (
                "a Statement element carrying a string",
                r#"{"Version":"2012-10-17","Statement":"Allow"}"#,
                None,
            ),
            (
                "a statement list carrying a number",
                r#"{"Statement":[42]}"#,
                None,
            ),
        ];

        for (case, document, expected) in rows {
            let defect = trust_policy_document_defect(document);
            assert_eq!(
                defect, expected,
                "case {case:?}: expected {expected:?}, got {defect:?} for {document}"
            );
        }
    }
}
