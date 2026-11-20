// SPDX-License-Identifier: CC0-1.0

//! What SHAPE a document an inline policy write carries has to have, end to end over the
//! awsQuery wire.
//!
//! The rule under test: **every operation that embeds an inline policy reads the document's
//! shape before it stores anything** — the document is a JSON object, it carries the
//! `Statement` element the IAM JSON policy grammar requires, and that element carries a
//! statement or an array of statements, each of them a JSON object of policy elements. A
//! `Statement` carrying anything else (a string, a number, `null`, a list with a scalar or a
//! list in it) describes no statement at all, so the write is refused with the modelled
//! `MalformedPolicyDocumentException` (awsQuery code `MalformedPolicyDocument`, HTTP 400) and
//! the fixed sentence IAM words a document it cannot read as a policy with, and NOTHING is
//! stored. A document whose shape IS a policy is stored and reads back byte for byte, which
//! is what keeps the refusals from being a blanket one.
//!
//! The three operations that embed an inline policy — `PutRolePolicy`, `PutUserPolicy` and
//! `PutGroupPolicy` — answer the same way, and each is read back through its own reporting
//! half (`GetRolePolicy`, `GetUserPolicy`, `GetGroupPolicy`), so what is asserted is the
//! HTTP behaviour of a persisted (or refused) write rather than a handler called directly.
//!
//! The rows differ along what the rule generalises over: which JSON type the `Statement`
//! element carries, whether the offending value is the element itself or an entry of its
//! list, where in the list it sits, the entity type the policy is embedded in, and whether
//! the document declares a `Version`. The accepted rows span the forms the grammar admits:
//! the statement list, the single statement object, and the empty list.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and every suite in
//! this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, document_element, element, error_code, router};

/// The awsQuery error code the model fixes for `MalformedPolicyDocumentException`
/// (`awsQueryError` on the error shape), which is what a client branches on.
const MALFORMED: &str = "MalformedPolicyDocument";

/// The awsQuery error code a read of an inline policy the entity does not hold answers
/// with — what a refused write must leave behind.
const NO_SUCH_ENTITY: &str = "NoSuchEntity";

/// The fixed sentence IAM words a document it cannot read as a policy with.
const SYNTAX_ERRORS: &str = "Syntax errors in policy.";

/// A trust policy with a single statement: the roles this suite creates need one, and the
/// trust document is not what is under test here.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"dlm.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The name every policy this suite writes is embedded under.
const POLICY_NAME: &str = "embedded";

/// The three entity types an inline policy is embedded in, each with the operations that
/// write and read one and the fixture that creates the entity.
#[derive(Clone, Copy, Debug)]
enum Embeds {
    Role,
    User,
    Group,
}

impl Embeds {
    /// Every entity type, so each row runs against all three.
    const ALL: [Self; 3] = [Self::Role, Self::User, Self::Group];

    /// The awsQuery action that embeds an inline policy in this entity type.
    fn put(self) -> &'static str {
        match self {
            Self::Role => "PutRolePolicy",
            Self::User => "PutUserPolicy",
            Self::Group => "PutGroupPolicy",
        }
    }

    /// The awsQuery action that reads one back.
    fn get(self) -> &'static str {
        match self {
            Self::Role => "GetRolePolicy",
            Self::User => "GetUserPolicy",
            Self::Group => "GetGroupPolicy",
        }
    }

    /// The name of the input member that names the entity.
    fn member(self) -> &'static str {
        match self {
            Self::Role => "RoleName",
            Self::User => "UserName",
            Self::Group => "GroupName",
        }
    }

    /// Create the entity the policy is embedded in, with the account's own credentials.
    async fn create(self, router: &MultiServiceRouter, name: &str) {
        let mut params: Vec<(&str, &str)> = vec![(self.member(), name)];
        let action = match self {
            Self::Role => {
                params.push(("AssumeRolePolicyDocument", TRUST_POLICY));
                "CreateRole"
            }
            Self::User => "CreateUser",
            Self::Group => "CreateGroup",
        };
        let (status, body) = call(router, action, &params).await;
        assert_eq!(
            status, 200,
            "the fixture {action} must succeed for {name:?}, got {status}: {body}"
        );
    }

    /// Embed `document` under [`POLICY_NAME`].
    async fn embed(
        self,
        router: &MultiServiceRouter,
        name: &str,
        document: &str,
    ) -> (u16, String) {
        call(
            router,
            self.put(),
            &[
                (self.member(), name),
                ("PolicyName", POLICY_NAME),
                ("PolicyDocument", document),
            ],
        )
        .await
    }

    /// Read the embedded policy back.
    async fn read(self, router: &MultiServiceRouter, name: &str) -> (u16, String) {
        call(
            router,
            self.get(),
            &[(self.member(), name), ("PolicyName", POLICY_NAME)],
        )
        .await
    }
}

/// Every `Statement` element that carries no statement, each a shape a client reaches by
/// serializing something that is not a policy.
const NO_STATEMENT_SHAPES: &[(&str, &str)] = &[
    ("a Statement carrying a string", r#"{"Version":"2012-10-17","Statement":"Allow"}"#),
    ("a Statement carrying a number", r#"{"Statement":17}"#),
    ("a Statement carrying null", r#"{"Statement":null}"#),
    ("a Statement carrying a boolean", r#"{"Version":"2012-10-17","Statement":true}"#),
    (
        "a statement list carrying a string",
        r#"{"Statement":["Effect=Allow"]}"#,
    ),
    (
        "a statement list carrying a number",
        r#"{"Version":"2012-10-17","Statement":[42]}"#,
    ),
    (
        "a statement list carrying a list",
        r#"{"Statement":[[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]]}"#,
    ),
    (
        "a complete statement beside an entry that is no statement",
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"},"and more"]}"#,
    ),
];

/// Documents whose shape IS a policy, in each form the grammar admits: the list, the single
/// statement object, the empty list, and a statement carrying only what an inline policy is
/// held to.
const ACCEPTED_SHAPES: &[(&str, &str)] = &[
    (
        "a statement list",
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::reports/*"}]}"#,
    ),
    (
        "the single statement object the grammar admits",
        r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Action":"s3:DeleteObject","Resource":"*"}}"#,
    ),
    ("the empty statement list", r#"{"Statement":[]}"#),
    (
        "two statements and no Version",
        r#"{"Statement":[{"Effect":"Allow","Action":"logs:PutLogEvents","Resource":"*"},{"Effect":"Deny","Action":"logs:DeleteLogGroup","Resource":"arn:aws:logs:us-west-2:000000000000:log-group:audit"}]}"#,
    ),
];

/// RULE: a `Statement` element that carries no statement is refused with
/// `MalformedPolicyDocument` and IAM's syntax sentence, by every operation that embeds an
/// inline policy, and nothing is stored.
#[tokio::test]
async fn a_statement_that_is_no_statement_is_refused_and_stores_nothing() {
    let router = router();

    for embeds in Embeds::ALL {
        for (row, (case, document)) in NO_STATEMENT_SHAPES.iter().enumerate() {
            let case = format!("{case} on {}", embeds.put());
            let name = format!("shaped{row}");
            embeds.create(&router, &name).await;

            let (status, body) = embeds.embed(&router, &name, document).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                MALFORMED,
                "case {case:?}: expected the error code {MALFORMED}, got {:?}",
                error_code(&body)
            );
            assert_eq!(
                element(&body, "Message"),
                SYNTAX_ERRORS,
                "case {case:?}: expected the refusal worded {SYNTAX_ERRORS:?}, got {:?}",
                element(&body, "Message")
            );

            // Nothing was stored: the read of that policy name answers as it does for a
            // name the entity never held.
            let (read_status, read) = embeds.read(&router, &name).await;
            assert_eq!(
                read_status, 404,
                "case {case:?}: a refused write must store nothing, got {read_status}: {read}"
            );
            assert_eq!(
                error_code(&read),
                NO_SUCH_ENTITY,
                "case {case:?}: expected the read to answer {NO_SUCH_ENTITY}, got {read}"
            );
        }
    }
}

/// RULE: a document whose shape IS a policy is stored by every one of those operations and
/// reads back byte for byte — the refusals above are about the shape, not about the write.
#[tokio::test]
async fn a_document_whose_shape_is_a_policy_is_stored_and_reads_back_verbatim() {
    let router = router();

    for embeds in Embeds::ALL {
        for (row, (case, document)) in ACCEPTED_SHAPES.iter().enumerate() {
            let case = format!("{case} on {}", embeds.put());
            let name = format!("stored{row}");
            embeds.create(&router, &name).await;

            let (status, body) = embeds.embed(&router, &name, document).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the write to succeed, got {status}: {body}"
            );

            let (read_status, read) = embeds.read(&router, &name).await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: the stored policy must be readable, got {read_status}: {read}"
            );
            assert_eq!(
                document_element(&read, "PolicyDocument"),
                *document,
                "case {case:?}: expected the document to read back as it was embedded, got \
                 {:?}",
                document_element(&read, "PolicyDocument")
            );
        }
    }
}
