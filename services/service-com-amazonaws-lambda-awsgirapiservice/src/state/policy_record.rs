// SPDX-License-Identifier: CC0-1.0

//! The RESOURCE-BASED POLICY one function, version, alias or layer version holds: the
//! document, its statements, and the record this state keeps it as.
//!
//! `AddPermission` "grants a principal permission to use a function", `GetPolicy` "returns the
//! resource-based IAM policy for a function, version, or alias" and `RemovePermission`
//! "revokes function-use permission from an Amazon Web Services service or another Amazon Web
//! Services account" — so one record holds the whole document a resource's grants add up to,
//! and every grant is one statement of it. This module is that document: how a principal, a
//! statement and a revision are written and read, and how a statement is added to and taken
//! out of the document a read reports.
//!
//! Split out of [`super`] (the module root) so the policy document has a file of its own
//! beside the two readings it uses — [`super::policy_principal`], which decides which form a
//! principal is in, and [`super::policy_identity`] — rather than sitting among the function
//! records. Nothing here changed in the move: the items are the ones the root held, and
//! [`super`] re-exports them, so every `crate::state::…` path still resolves.

use super::*;

/// The `Principal` of a policy statement for the principal `AddPermission` was given.
///
/// The model documents the forms the member takes — "The Amazon Web Services service,
/// Amazon Web Services account, IAM user, or IAM role that invokes the function", where
/// "For Amazon Web Services services, the principal is a domain-style identifier that
/// the service defines, such as s3.amazonaws.com" — and the policy language spells each
/// of them differently:
///
/// * an account id is that account's root principal, written as its ARN;
/// * an IAM user or role is already an ARN, and is used as given;
/// * anything else is the domain-style identifier of a service principal;
/// * `*` is every principal, which the language writes as the bare string.
///
/// Which of those forms a value IS is read by [`policy_principal`], the same reading that
/// decides whether a grant may name it at all, so what is admitted and what is written
/// here cannot disagree.
#[must_use]
pub fn principal_json(principal: &str) -> String {
    if principal == EVERY_PRINCIPAL {
        return json_string(principal);
    }
    if is_account_id(principal) {
        return json_object(&[(
            "AWS",
            json_string(&format!("arn:aws:iam::{principal}:root")),
        )]);
    }
    if principal.starts_with("arn:") {
        return json_object(&[("AWS", json_string(principal))]);
    }
    json_object(&[("Service", json_string(principal))])
}

/// One statement of a function's resource-based policy: exactly the grant one
/// `AddPermission` call described, plus the ARN it was granted on.
#[derive(Debug, Clone)]
pub struct PolicyStatement {
    /// The `StatementId`, which "differentiates the statement from others in the same
    /// policy" and is the statement's `Sid`.
    pub sid: String,
    /// The action the principal may use on the function.
    pub action: String,
    /// The principal, as supplied; [`principal_json`] renders it.
    pub principal: String,
    /// The ARN the permission is granted on: the function's unqualified ARN for a
    /// function-level statement, and the qualified one for a version or alias.
    pub resource: String,
    /// `SourceArn`, when the caller limited the grant to one invoking resource.
    pub source_arn: Option<String>,
    /// `SourceAccount`, when the caller limited the grant to one owning account.
    pub source_account: Option<String>,
    /// `PrincipalOrgID`, when the caller limited the grant to one organization.
    pub principal_org_id: Option<String>,
    /// `EventSourceToken`, when the caller required one.
    pub event_source_token: Option<String>,
    /// `FunctionUrlAuthType`, when the grant is about a function URL.
    pub function_url_auth_type: Option<String>,
    /// `InvokedViaFunctionUrl`, when the caller restricted the grant to invocations
    /// that come through a function URL.
    pub invoked_via_function_url: Option<bool>,
}

impl PolicyStatement {
    /// The `Condition` block of this statement, or `None` when the grant carries no
    /// restriction at all.
    ///
    /// Each optional member of the request restricts the grant, and each is written as
    /// the IAM condition key that expresses it. `SourceArn` is compared as an ARN — the
    /// model's note "Lambda configures the comparison using the `StringLike` operator"
    /// is why wildcards in the ARN match, which is what the ARN-typed `ArnLike` operator
    /// does — the remaining string-valued keys are compared for equality, and the
    /// boolean one with `Bool`.
    pub(super) fn condition_json(&self) -> Option<String> {
        let mut operators: Vec<(&str, String)> = Vec::new();
        if let Some(source_arn) = &self.source_arn {
            operators.push((
                "ArnLike",
                json_object(&[("AWS:SourceArn", json_string(source_arn))]),
            ));
        }

        let mut equals: Vec<(&str, String)> = Vec::new();
        if let Some(source_account) = &self.source_account {
            equals.push(("AWS:SourceAccount", json_string(source_account)));
        }
        if let Some(org_id) = &self.principal_org_id {
            equals.push(("aws:PrincipalOrgID", json_string(org_id)));
        }
        if let Some(token) = &self.event_source_token {
            equals.push(("lambda:EventSourceToken", json_string(token)));
        }
        if let Some(auth_type) = &self.function_url_auth_type {
            equals.push(("lambda:FunctionUrlAuthType", json_string(auth_type)));
        }
        if !equals.is_empty() {
            operators.push(("StringEquals", json_object(&equals)));
        }

        if let Some(via_function_url) = self.invoked_via_function_url {
            operators.push((
                "Bool",
                json_object(&[(
                    "lambda:InvokedViaFunctionUrl",
                    json_string(&via_function_url.to_string()),
                )]),
            ));
        }

        (!operators.is_empty()).then(|| json_object(&operators))
    }

    /// This statement as the JSON `AddPermission` answers with and `GetPolicy` embeds:
    /// "The permission statement that's added to the function policy".
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut members = vec![
            ("Sid", json_string(&self.sid)),
            ("Effect", json_string(STATEMENT_EFFECT_ALLOW)),
            ("Principal", principal_json(&self.principal)),
            ("Action", json_string(&self.action)),
            ("Resource", json_string(&self.resource)),
        ];
        if let Some(condition) = self.condition_json() {
            members.push(("Condition", condition));
        }
        json_object(&members)
    }
}

/// The member of a policy document that carries its statements, as the IAM policy
/// language names it.
pub(super) const POLICY_STATEMENT_MEMBER: &str = "Statement";

/// The member of a policy statement that carries its id, as the IAM policy language
/// names it; it is the `StatementId` an `AddPermission` grant was made under.
pub(super) const STATEMENT_ID_MEMBER: &str = "Sid";

/// The refusal a value that is not a policy document gets, on the operations that take one
/// as a string member.
///
/// The `Policy` member is "formatted as a JSON string", and what is wrong with a value that
/// is not one is that it does not PARSE as the policy language's document: the sentence
/// states that and nothing of the request, so it reads the same whatever was submitted.
pub const POLICY_SYNTAX_ERROR_MESSAGE: &str = "Policy has syntax errors.";

/// Whether `document` is a policy document.
///
/// The `ResourcePolicy` member is "formatted as a JSON string", and the policy language
/// writes a policy as a JSON OBJECT (a `Version` and the statements under
/// [`POLICY_STATEMENT_MEMBER`]), so a value that is not one is no policy at all.
#[must_use]
pub fn is_policy_document(document: &str) -> bool {
    ::serde_json::from_str::<::serde_json::Value>(document).is_ok_and(|parsed| parsed.is_object())
}

/// The `Sid` of the rendered policy statement `statement`, when it carries one.
pub(super) fn statement_id_of(statement: &::serde_json::Value) -> Option<&str> {
    statement
        .get(STATEMENT_ID_MEMBER)
        .and_then(::serde_json::Value::as_str)
}

/// `document` with `statement` — one rendered policy statement — appended to the
/// statements it carries, or `None` when the two cannot be merged at all.
///
/// A document this store holds was accepted as a JSON object and a statement is rendered
/// here, so both parse and the merge is always made; `None` is the answer for a value
/// that is somehow neither, which lets the caller keep the statement rather than report a
/// grant it did not store (see [`PolicyRecord::add_statement`]).
///
/// The merged document is RE-SERIALIZED, so a document that carried its members in
/// another order, or any whitespace, is reported in this one afterwards: the policy is
/// the same policy, but only a document nobody has added a statement to since is reported
/// byte for byte as it was supplied.
pub(super) fn document_with_statement(document: &str, statement: &str) -> Option<String> {
    let (Ok(mut parsed), Ok(added)) = (
        ::serde_json::from_str::<::serde_json::Value>(document),
        ::serde_json::from_str::<::serde_json::Value>(statement),
    ) else {
        return None;
    };
    let members = parsed.as_object_mut()?;
    let held = members
        .entry(POLICY_STATEMENT_MEMBER)
        .or_insert_with(|| ::serde_json::Value::Array(Vec::new()));
    match held {
        // The language admits a single statement as well as a list of them, so a
        // document carrying one becomes a document carrying both.
        ::serde_json::Value::Array(statements) => statements.push(added),
        single => {
            let first = single.take();
            *single = ::serde_json::Value::Array(vec![first, added]);
        }
    }
    Some(parsed.to_string())
}

/// The resource-based policy of one function, version or alias: the statements
/// `AddPermission` has added to it, or the document a `PutResourcePolicy` stored, and
/// the revision they are at.
#[derive(Debug, Clone)]
pub struct PolicyRecord {
    /// The statements, in the order they were added, so the document reports them
    /// deterministically. Empty for a policy held as a whole [`PolicyRecord::document`].
    pub statements: Vec<PolicyStatement>,
    /// The whole policy document a caller supplied, in the form this service reports it.
    ///
    /// `PutResourcePolicy` "replaces any existing policy on the Lambda resource. If you
    /// previously added permissions using the AddPermission operation, the new policy
    /// overwrites those permissions", so a policy stored that way IS that document: the
    /// statements above are then empty and a later grant is merged into it (see
    /// [`PolicyRecord::add_statement`]). `None` for a policy made up of statements.
    pub document: Option<String>,
    /// The revision this policy is at. `GetPolicy` reports it, and `AddPermission`'s
    /// `RevisionId` precondition is checked against it ("For AddPermission and
    /// RemovePermission API operations: Call GetPolicy to retrieve the latest RevisionId
    /// for your resource"); a fresh one is minted on every change.
    pub revision_id: String,
}

impl Default for PolicyRecord {
    fn default() -> Self {
        Self {
            statements: Vec::new(),
            document: None,
            revision_id: mint_revision_id(),
        }
    }
}

impl PolicyRecord {
    /// The policy a caller supplied as a whole document, at a fresh revision.
    ///
    /// The document is STORED as this service reports it, which is not the caller's bytes:
    /// a resource policy is reported in the one rendering every service that attaches an
    /// IAM policy document reports it in (`service_common::iam_policy_rendering`) — the
    /// insignificant whitespace between its JSON tokens dropped, and an element the policy
    /// language types as a string or an array of them written as the string when the caller
    /// wrote a one-element array. Everything else is the caller's: member order, the
    /// spelling of every value, and every byte inside a string literal. So the policy a
    /// caller reads back is the same text whatever layout it was submitted in, and putting
    /// back what was read stores the same bytes again.
    #[must_use]
    pub fn of_document(document: String) -> Self {
        Self {
            statements: Vec::new(),
            document: Some(::service_common::iam_policy_rendering::rendered(&document)),
            revision_id: mint_revision_id(),
        }
    }

    /// Hand `read` the statements the STORED DOCUMENT carries, in the order it carries them,
    /// and answer what it made of them — `None` for a policy that holds no document to read
    /// them out of (one made of the statements this record holds structured, or one whose
    /// document is somehow not the JSON object it was accepted as).
    ///
    /// The policy language admits a single statement as well as a list of them, so a
    /// document carrying one object reads as one statement here: that is the one place
    /// this rule is stated, and every reader of a stored document's statements goes
    /// through it. They are LENT rather than handed over, so a reader that only counts or
    /// searches them copies none of them.
    pub(super) fn with_document_statements<T>(
        &self,
        read: impl FnOnce(&[::serde_json::Value]) -> T,
    ) -> Option<T> {
        let document = self.document.as_ref()?;
        let parsed = ::serde_json::from_str::<::serde_json::Value>(document).ok()?;
        Some(match parsed.get(POLICY_STATEMENT_MEMBER) {
            Some(::serde_json::Value::Array(statements)) => read(statements),
            Some(single) => read(::std::slice::from_ref(single)),
            None => read(&[]),
        })
    }

    /// Whether this policy already holds a statement with `sid` — among the statements
    /// `AddPermission` added, or among those the stored document carries.
    #[must_use]
    pub fn holds(&self, sid: &str) -> bool {
        if self.statements.iter().any(|statement| statement.sid == sid) {
            return true;
        }
        self.with_document_statements(|statements| {
            statements
                .iter()
                .any(|statement| statement_id_of(statement) == Some(sid))
        })
        .unwrap_or(false)
    }

    /// Add `statement` to this policy, which is what `AddPermission` appends to it.
    ///
    /// A policy held as a caller-supplied document keeps being that document, so the
    /// statement joins the statements that document carries; otherwise it joins the
    /// statements this record holds.
    ///
    /// A statement merged into a document is part of the document from then on, so
    /// `RemovePermission` — which retains over the statements this record holds
    /// structured — cannot take it back out again, and refuses with the statement id it
    /// cannot find rather than reporting a removal it did not make. Removing from a
    /// stored document is the remaining half of that operation's own slice.
    pub fn add_statement(&mut self, statement: PolicyStatement) {
        // The document is taken out first, so a value that cannot carry the statement at
        // all (one that is somehow not the JSON object a stored document was accepted as)
        // leaves this record holding the statement structured rather than reporting a
        // grant it stored nowhere.
        match self
            .document
            .take()
            .and_then(|document| document_with_statement(&document, &statement.to_json()))
        {
            Some(merged) => self.document = Some(merged),
            None => self.statements.push(statement),
        }
    }

    /// The whole policy document, which is what `GetPolicy`'s and
    /// `GetResourcePolicy`'s `Policy` member carries.
    #[must_use]
    pub fn to_json(&self) -> String {
        if let Some(document) = &self.document {
            return document.clone();
        }
        let statements: Vec<String> = self
            .statements
            .iter()
            .map(PolicyStatement::to_json)
            .collect();
        json_object(&[
            ("Version", json_string(POLICY_LANGUAGE_VERSION)),
            ("Id", json_string(POLICY_ID)),
            (
                POLICY_STATEMENT_MEMBER,
                format!("[{}]", statements.join(",")),
            ),
        ])
    }

    /// How many statements this policy holds — those a grant added STRUCTURED and those the
    /// stored document carries, summed.
    ///
    /// The two are mutually exclusive as this record is written today
    /// ([`PolicyRecord::add_statement`] merges a grant into a stored document, and keeps
    /// statements structured only for a policy that holds no document), so the sum is one of
    /// the two in practice — and it stays right for a record that somehow holds both rather
    /// than silently dropping one side. A stored document that does not parse at all carries
    /// no statement this can count, which is the same position
    /// [`PolicyRecord::holds`] is in about it.
    #[must_use]
    pub fn statement_count(&self) -> usize {
        self.statements.len()
            + self
                .with_document_statements(<[::serde_json::Value]>::len)
                .unwrap_or(0)
    }

    /// The size of this policy as the quota accounting measures it, which is what a
    /// refusal over [`MAX_POLICY_SIZE_BYTES`] states and what that quota is applied to.
    ///
    /// It is the document's own bytes plus
    /// [`POLICY_SIZE_STATEMENT_OVERHEAD_BYTES`] for every statement in it, so a policy
    /// made of many small statements measures further past the document a read returns
    /// than one made of a few large ones — and the grant that crosses the quota is refused
    /// on that measure rather than on the document's length.
    #[must_use]
    pub fn measured_size(&self) -> usize {
        self.to_json().len() + POLICY_SIZE_STATEMENT_OVERHEAD_BYTES * self.statement_count()
    }
}
