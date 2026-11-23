// SPDX-License-Identifier: CC0-1.0

//! What a resource of EVERY AWS Secrets Manager type reports when that service refused the
//! call its provider made, end to end over the awsQuery wire.
//!
//! RULE under test: **the SDK diagnostic quoted in a failed resource's
//! `ResourceStatusReason` belongs to the SERVICE the provider called, not to one resource
//! type of it: whichever type's provider ran into a refusal, what CloudFormation quotes is
//! the exception that provider's SDK raised — the owning service's own sentence followed by
//! that SDK's diagnostic of the call, naming the service as its SDK names it, the HTTP status
//! the service answered and the id of the call — closed by the `RequestToken` of the resource
//! request and the `HandlerErrorCode` class the refusal falls in.**
//!
//! Why it matters: the reason is the only place a client sees WHICH service refused a
//! resource and with what status. Automation routes a stack failure to the service that
//! answered it and quotes the call's id when it asks that service's operators about it, and it
//! has to do so for every resource type of that service — a reason that carried the clause for
//! one type and dropped it for the next would make the clause unparseable.
//!
//! The rows differ along what the rule generalises over: WHICH resource type of the service
//! the stack declared — the secret itself and the rotation configured on it, deployed by two
//! different providers calling two different operations — and, with that, which refusal of the
//! owning service they ran into and which `HandlerErrorCode` class it falls in (a name the
//! service already holds, a secret it does not hold at all, a well-formed call it will not
//! carry out). The third type, the resource POLICY of a secret, is held to the same diagnostic
//! over its own refusals by [`crate::secretsmanager_refusal_sdk_diagnostic`], whose assertion
//! and secret seeding this suite reads rather than restating.
//!
//! The sentences are the owning service's own, read from the crate that owns them.

use serde_json::{json, Value};

use service_com_amazonaws_cloudformation_cloudformation::ops::types::HANDLER_ERROR_CODE_VALUES;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    secret_already_exists_message, NO_ROTATION_FUNCTION_MESSAGE, SECRET_NOT_FOUND_MESSAGE,
};

use crate::refused_resource_sdk_exception::parts;
use crate::secretsmanager_refusal_sdk_diagnostic::{assert_diagnostic, seed_a_secret};
use crate::stack_lifecycle::{
    create_stack, resource_event_reason, router, settled_stack, xml_text,
};
use crate::stack_owned_secret_teardown::secretsmanager;

/// The logical id every row declares its one resource under, so the resource the stack fails
/// is unambiguous.
const DECLARED: &str = "DeclaredResource";

/// The value the secret a row seeds holds, so the secret is a real one the declared resource
/// could have been deployed against.
const SEEDED_VALUE: &str = "a-stack-attached-credential";

/// Which resource type of the service a row declares — the dimension the rule generalises
/// over, since each type is deployed by a provider of its own calling an operation of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declared {
    /// `AWS::SecretsManager::Secret`, deployed through `CreateSecret`.
    Secret,
    /// `AWS::SecretsManager::RotationSchedule`, deployed through `RotateSecret`.
    RotationSchedule,
}

impl Declared {
    /// The type name a template declares this resource under.
    fn resource_type(self) -> &'static str {
        match self {
            Self::Secret => "AWS::SecretsManager::Secret",
            Self::RotationSchedule => "AWS::SecretsManager::RotationSchedule",
        }
    }
}

/// One row: the declaration the owning service refuses, and the exception it answers with.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The stack the row deploys, which also names the secret it seeds or addresses so no two
    /// rows collide.
    stack: &'static str,
    /// Whether the row SEEDS that secret through the owning service's own wire: the row about
    /// a secret the service does not hold seeds none.
    seeds_the_secret: bool,
    /// The resource type the row declares.
    declared: Declared,
    /// The error code the owning service refuses the provider's call with, which the row
    /// observes through the SENTENCE that service words for it ([`sentence`]): the reason
    /// carries the exception's message and its `HandlerErrorCode` class, never the code
    /// itself, so the sentence is what says which refusal of that operation was reached.
    expected_code: &'static str,
    /// The HTTP status it answers that refusal with.
    expected_status: u16,
    /// The `HandlerErrorCode` class the refusal falls in, where which class that is follows
    /// from the error code the owning service answered; `None` for a row whose class this
    /// suite does not pin, which is then held only to the model's own value set.
    expected_class: Option<&'static str>,
}

/// The class a refusal that names a resource the owning service does not hold falls in.
const NOT_FOUND: &str = "NotFound";

/// The class a refusal of the REQUEST the declaration asked for falls in.
const INVALID_REQUEST: &str = "InvalidRequest";

const ROWS: &[Row] = &[
    Row {
        // The secret ITSELF, declared under a name the service already holds: a refusal of
        // `CreateSecret`, in the one class CloudFormation's own record of the Region could
        // have said more about — it says nothing here, because the holder is a secret no
        // stack of this Region manages, so the owning service's own sentence stands.
        case: "a secret declared under a name the service already holds",
        stack: "sm-type-diagnostic-name-held",
        seeds_the_secret: true,
        declared: Declared::Secret,
        expected_code: "ResourceExistsException",
        expected_status: 400,
        // Which class this provider reports a name collision under is the provider's, and no
        // recording this suite can read states it, so the row pins the DIAGNOSTIC and holds
        // the class only to the model's own value set.
        expected_class: None,
    },
    Row {
        // The ROTATION of a secret that service does not hold: a refusal of `RotateSecret`,
        // made by another provider over another operation, in another class again.
        case: "a rotation declared on a secret the service does not hold",
        stack: "sm-type-diagnostic-absent-secret",
        seeds_the_secret: false,
        declared: Declared::RotationSchedule,
        expected_code: "ResourceNotFoundException",
        expected_status: 400,
        expected_class: Some(NOT_FOUND),
    },
    Row {
        // The same type over a secret that service DOES hold, refused for the request itself:
        // rotation cannot be enabled on a secret that has no rotation function and the
        // declaration names none.
        case: "a rotation declared on a secret that has no rotation function",
        stack: "sm-type-diagnostic-no-function",
        seeds_the_secret: true,
        declared: Declared::RotationSchedule,
        expected_code: "InvalidRequestException",
        expected_status: 400,
        expected_class: Some(INVALID_REQUEST),
    },
];

/// The sentence the owning service words for a row's refusal — that service's own wording,
/// read from the crate that owns it rather than copied here, which is also what says WHICH
/// refusal of the called operation the row reached.
fn sentence(row: &Row) -> String {
    match row.declared {
        Declared::Secret => secret_already_exists_message(row.stack),
        Declared::RotationSchedule if row.seeds_the_secret => {
            NO_ROTATION_FUNCTION_MESSAGE.to_string()
        }
        Declared::RotationSchedule => SECRET_NOT_FOUND_MESSAGE.to_string(),
    }
}

/// The properties a row declares for its one resource.
fn properties(row: &Row) -> Value {
    match row.declared {
        // The secret's name and the value it holds, which is the whole of a secret.
        Declared::Secret => json!({"Name": row.stack, "SecretString": SEEDED_VALUE}),
        // The one property the type requires: the secret the rotation is configured for.
        Declared::RotationSchedule => json!({"SecretId": row.stack}),
    }
}

/// A template declaring ONE resource of the row's type.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            DECLARED: {
                "Type": row.declared.resource_type(),
                "Properties": properties(row),
            }
        }
    })
    .to_string()
}

#[tokio::test]
async fn a_refused_resource_of_any_secretsmanager_type_reports_its_sdk_exception() {
    for row in ROWS {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        if row.seeds_the_secret {
            seed_a_secret(&router, case, row.stack).await;
        }

        let (status_code, body) =
            create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused call to roll the stack back, got {described}"
        );

        let reason = resource_event_reason(
            &router,
            row.stack,
            DECLARED,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = parts(case, &reason);
        assert_diagnostic(case, &quoted, &sentence(row), row.expected_status);
        assert!(
            HANDLER_ERROR_CODE_VALUES.contains(&class.as_str()),
            "case {case:?}: expected one of the model's own HandlerErrorCode values, got \
             {class:?} in {reason:?}"
        );
        if let Some(expected) = row.expected_class {
            assert_eq!(
                class, expected,
                "case {case:?}: expected the {expected} class for {}, got {class:?} in \
                 {reason:?}",
                row.expected_code
            );
        }

        // The refusal was the owning service's, so what that service held before the stack
        // ran is exactly what it holds afterwards: a seeded secret is still there, and a
        // secret the row never seeded was never created.
        let (described_code, described_secret) =
            secretsmanager(&router, "DescribeSecret", &json!({"SecretId": row.stack})).await;
        let expected_code = if row.seeds_the_secret { 200 } else { 400 };
        assert_eq!(
            described_code, expected_code,
            "case {case:?}: expected DescribeSecret({}) to answer {expected_code}, got \
             {described_code} {described_secret}",
            row.stack
        );
    }
}
