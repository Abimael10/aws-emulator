// SPDX-License-Identifier: CC0-1.0

//! WHY IAM refused a signed call to its own API, end to end over the awsQuery wire: the
//! reason clause that closes the `AccessDenied` message it answers.
//!
//! RULE: the clause following `User: <principal> is not authorized to perform: <action> on
//! resource: <resource> ` names the DOCUMENT that decided the refusal, and the four
//! documents that can decide one are worded differently from each other, because they need
//! different fixes: `because no identity-based policy allows the <action> action` where
//! nothing in force granted the call, `with an explicit deny in an identity-based policy`
//! where a statement of the principal's own documents denies it, `with an explicit deny in
//! a permissions boundary: <boundary-policy-arn>` where a statement of the ceiling capping
//! them does — named by the ARN of the policy that IS the boundary, the value `GetUser`
//! reports under `PermissionsBoundary.PermissionsBoundaryArn`, since an account can carry
//! several — and `with an explicit deny in a session policy` where the deciding `Deny` came
//! from the session policies the temporary credential was minted with. All four are HTTP
//! 403 with the awsQuery error code `AccessDenied`, and a ceiling or a scope NARROWS rather
//! than revokes: the same principal is still served the calls the document it runs under
//! allows. The message then continues with the pointer at the request's authorization
//! details whichever document decided it — so the clause is read with that continuation
//! dropped by shape, never by pinning the per-request authorization id it names.
//!
//! Two services have to agree for the fourth of those clauses to be reachable at all — STS
//! mints the session and records the scope it was minted with, IAM holds the user, the role
//! and their documents and is the gate that decides the call — so this is the suite that
//! can pin the four side by side, on one principal estate, which no single crate's own test
//! target can.
//!
//! The rows span what the rule generalises over: WHICH document decided the refusal (all
//! four), WHICH operation of the deciding service asked (a read of one user, a read of that
//! user's keys, a read of its group memberships), and WHICH credential class signed (a
//! long-term user key and a role session's temporary credential).

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The account id every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own administrative
/// credential: what the estate below is created with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The IAM user whose calls every refusal here is about.
const CALLER: &str = "reason-caller";

/// The role the caller assumes for the session-scoped leg.
const ROLE: &str = "ReasonRole";

/// The `RoleSessionName` the mint asks for — the last segment of the assumed-role ARN a
/// refusal names as the principal.
const SESSION: &str = "attributed";

/// The managed policy that is the caller's permissions boundary.
const BOUNDARY_POLICY: &str = "reason-ceiling";

/// A trust policy admitting the account root, which is the delegation that lets a principal
/// of this account the identity policies allow assume the role.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// IAM and STS over ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key the service reads the caller's identity from.
/// The signature is never checked: this router verifies none.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Post one awsQuery form body to `signing_name`, presenting `access_key_id`.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One IAM call: the action and its form parameters, presented as signed by
/// `access_key_id`.
async fn iam(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    query(router, access_key_id, "iam", body).await
}

/// One IAM call that is part of the fixture, which must be served.
async fn ok(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = iam(router, access_key_id, action, parameters).await;
    assert_eq!(
        status, 200,
        "the fixture {action} must succeed, got {status}: {body}"
    );
    body
}

/// The `Message` of a refusal, with the serializer's escaping undone.
fn message_of(body: &str) -> String {
    elements(body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Message> in {body}"))
}

/// The awsQuery error code of a refusal.
fn error_code(body: &str) -> String {
    elements(body, "Code")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Code> in {body}"))
}

/// The ARN of an IAM user of this account at the default path, which is the resource a call
/// addressing that user by name is decided against.
fn user_arn(user_name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/{user_name}")
}

/// A document whose statements are written in the order given, each stating one effect over
/// one action list and one resource list. The JSON is built here rather than by a
/// serializer because every value in it is an ASCII ARN or action name.
fn document(statements: &[(&str, &[&str], &[String])]) -> String {
    let written: Vec<String> = statements
        .iter()
        .map(|(effect, actions, resources)| {
            let actions: Vec<String> = actions.iter().map(|a| format!("\"{a}\"")).collect();
            let resources: Vec<String> = resources.iter().map(|r| format!("\"{r}\"")).collect();
            format!(
                r#"{{"Effect":"{effect}","Action":[{}],"Resource":[{}]}}"#,
                actions.join(","),
                resources.join(",")
            )
        })
        .collect();
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        written.join(",")
    )
}

/// Every resource, as a one-entry resource list.
fn everything() -> Vec<String> {
    vec!["*".to_string()]
}

/// One operation a row asks for: the awsQuery action, and the `iam:` action a policy
/// statement names it by.
struct Operation {
    action: &'static str,
    iam_action: &'static str,
}

/// The three operations the rows ask for: each addresses ONE user by name, so every row is
/// decided against that user's ARN and every refusal names it the same way.
const OPERATIONS: &[Operation] = &[
    Operation {
        action: "GetUser",
        iam_action: "iam:GetUser",
    },
    Operation {
        action: "ListAccessKeys",
        iam_action: "iam:ListAccessKeys",
    },
    Operation {
        action: "ListGroupsForUser",
        iam_action: "iam:ListGroupsForUser",
    },
];

/// The `iam:` actions every document of the estate is written over.
fn subject_actions() -> Vec<&'static str> {
    OPERATIONS.iter().map(|op| op.iam_action).collect()
}

/// Which document decides a row's refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// Nothing in force granted the call.
    NoIdentityAllow,
    /// A statement of the caller's own documents denies it.
    IdentityDeny,
    /// A statement of the ceiling capping them denies it.
    BoundaryDeny,
    /// A statement of the session policies the credential was minted with denies it.
    SessionDeny,
}

/// The four causes, in the order the rows walk them.
const CAUSES: &[Cause] = &[
    Cause::NoIdentityAllow,
    Cause::IdentityDeny,
    Cause::BoundaryDeny,
    Cause::SessionDeny,
];

impl Cause {
    /// The user a call reaching this cause addresses. Each cause has its OWN target, so one
    /// estate reaches all four without any document being rewritten between the calls.
    fn target(self) -> &'static str {
        match self {
            Self::NoIdentityAllow => "reason-ungranted",
            Self::IdentityDeny => "reason-identity-denied",
            Self::BoundaryDeny => "reason-boundary-denied",
            Self::SessionDeny => "reason-session-denied",
        }
    }

    /// The clause closing the refusal of `iam_action` for this cause, under the boundary
    /// `boundary_arn`.
    fn reason(self, iam_action: &str, boundary_arn: &str) -> String {
        match self {
            Self::NoIdentityAllow => {
                format!("because no identity-based policy allows the {iam_action} action")
            }
            Self::IdentityDeny => "with an explicit deny in an identity-based policy".to_string(),
            Self::BoundaryDeny => {
                format!("with an explicit deny in a permissions boundary: {boundary_arn}")
            }
            Self::SessionDeny => "with an explicit deny in a session policy".to_string(),
        }
    }
}

/// The principal estate every row is decided over: the caller, its ceiling, the role it
/// assumes and the two credentials its calls are signed with.
struct Estate {
    router: MultiServiceRouter,
    /// The caller's long-term access key id, which signs the first three causes' calls.
    user_key: String,
    /// The role session's access key id, which signs the fourth's.
    session_key: String,
    /// The ARN of the policy that is the caller's permissions boundary, read back the way a
    /// client reads it.
    boundary_arn: String,
    /// The principal a refusal of a long-term-key call names.
    user_principal: String,
    /// The principal a refusal of a session call names.
    session_principal: String,
    /// The user every principal is allowed to read, which is the control each is served.
    control: &'static str,
}

/// Build that estate: five target users, a boundary denying the subject actions on one of
/// them, a caller whose own documents allow them on three and deny them on one, a role
/// allowing them on two, and a session of that role scoped by a policy denying them on one.
async fn estate() -> Estate {
    let router = build_router();
    let actions = subject_actions();
    let control = "reason-control";

    // The targets: the control every principal may read, plus one per cause.
    for target in ::std::iter::once(control).chain(CAUSES.iter().map(|cause| cause.target())) {
        ok(&router, AMBIENT_KEY, "CreateUser", &[("UserName", target)]).await;
    }

    // The ceiling: it admits everything, so it caps nothing except the one action/target
    // pair a statement of it denies — which is what makes the boundary, and not the
    // caller's own documents, the document that decides that call.
    let boundary_document = document(&[
        ("Allow", &["*"], &everything()),
        (
            "Deny",
            &actions,
            &[user_arn(Cause::BoundaryDeny.target())],
        ),
    ]);
    let created = ok(
        &router,
        AMBIENT_KEY,
        "CreatePolicy",
        &[
            ("PolicyName", BOUNDARY_POLICY),
            ("PolicyDocument", &boundary_document),
        ],
    )
    .await;
    let boundary_arn = element(&created, "Arn");

    // The caller, carrying that ceiling from the moment it is created.
    ok(
        &router,
        AMBIENT_KEY,
        "CreateUser",
        &[("UserName", CALLER), ("PermissionsBoundary", &boundary_arn)],
    )
    .await;
    // Read the ceiling back the way a client reads it, so the rows below are decided
    // against a boundary the account really holds for this caller.
    let read = ok(&router, AMBIENT_KEY, "GetUser", &[("UserName", CALLER)]).await;
    let reported = element(&read, "PermissionsBoundaryArn");
    assert_eq!(
        reported, boundary_arn,
        "the fixture caller must report the boundary it was created with, got {reported:?}"
    );

    // The role the session is minted from, allowing the subject actions on the control and
    // on the session cause's target alone.
    ok(
        &router,
        AMBIENT_KEY,
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    ok(
        &router,
        AMBIENT_KEY,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "allow-the-subject-actions"),
            (
                "PolicyDocument",
                &document(&[(
                    "Allow",
                    &actions,
                    &[
                        user_arn(control),
                        user_arn(Cause::SessionDeny.target()),
                    ],
                )]),
            ),
        ],
    )
    .await;

    // The caller's own documents: the subject actions allowed on the control, on the
    // boundary cause's target and on the identity cause's target, denied on that last one,
    // and the role assumable.
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}");
    ok(
        &router,
        AMBIENT_KEY,
        "PutUserPolicy",
        &[
            ("UserName", CALLER),
            ("PolicyName", "the-callers-grants"),
            (
                "PolicyDocument",
                &document(&[
                    (
                        "Allow",
                        &actions,
                        &[
                            user_arn(control),
                            user_arn(Cause::BoundaryDeny.target()),
                            user_arn(Cause::IdentityDeny.target()),
                        ],
                    ),
                    (
                        "Deny",
                        &actions,
                        &[user_arn(Cause::IdentityDeny.target())],
                    ),
                    (
                        "Allow",
                        &["sts:AssumeRole"],
                        ::std::slice::from_ref(&role_arn),
                    ),
                ]),
            ),
        ],
    )
    .await;

    let minted = ok(
        &router,
        AMBIENT_KEY,
        "CreateAccessKey",
        &[("UserName", CALLER)],
    )
    .await;
    let user_key = element(&minted, "AccessKeyId");

    // The session: minted BY the caller's own long-term key, scoped by a policy that admits
    // everything and denies the subject actions on the session cause's target alone.
    let session_policy = document(&[
        ("Allow", &["*"], &everything()),
        (
            "Deny",
            &actions,
            &[user_arn(Cause::SessionDeny.target())],
        ),
    ]);
    let body = format!(
        "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={SESSION}&Policy={}",
        form_encode(&role_arn),
        form_encode(&session_policy)
    );
    let (status, assumed) = query(&router, &user_key, "sts", body).await;
    assert_eq!(
        status, 200,
        "the fixture AssumeRole must succeed, got {status}: {assumed}"
    );
    let session_key = element(&assumed, "AccessKeyId");

    Estate {
        router,
        user_key,
        session_key,
        boundary_arn,
        user_principal: user_arn(CALLER),
        session_principal: format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{SESSION}"),
        control,
    }
}

impl Estate {
    /// The credential a call reaching `cause` is signed with.
    fn credential(&self, cause: Cause) -> &str {
        match cause {
            Cause::SessionDeny => &self.session_key,
            _ => &self.user_key,
        }
    }

    /// The principal a refusal of a call signed with that credential names.
    fn principal(&self, cause: Cause) -> &str {
        match cause {
            Cause::SessionDeny => &self.session_principal,
            _ => &self.user_principal,
        }
    }

    /// Ask `operation` about `target`, signed for `cause`.
    async fn ask(&self, cause: Cause, operation: &Operation, target: &str) -> (u16, String) {
        iam(
            &self.router,
            self.credential(cause),
            operation.action,
            &[("UserName", target)],
        )
        .await
    }
}

/// The reason clause of `message`: what follows the sentence's head, with the pointer at
/// the request's authorization details taken off.
///
/// The head is asserted here, so a row that diverges on the principal, the action or the
/// resource says so rather than reporting a mangled clause; the continuation is read by the
/// suite that owns that reader (`iam_authorization_details_pointer.rs`), which asserts its
/// fixed text and that one id is named in both of its slots — the id itself is a fact of the
/// request this run made, so no row pins its value.
fn reason_of(
    case: &str,
    message: &str,
    principal: &str,
    iam_action: &str,
    named_as: &str,
) -> String {
    use crate::iam_authorization_details_pointer as pointer;

    let head =
        format!("User: {principal} is not authorized to perform: {iam_action} on resource: {named_as} ");
    pointer::pointed_at(case, message);
    let sentence = pointer::sentence_of(case, message);
    sentence
        .strip_prefix(&head)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the refusal to open {head:?}, got {message:?}")
        })
        .to_string()
}

/// Assert that `body` is the refusal of `operation` on `target` that `cause` decides, and
/// answer the reason clause it closed with.
fn assert_refusal(
    case: &str,
    status: u16,
    body: &str,
    estate: &Estate,
    cause: Cause,
    operation: &Operation,
    target: &str,
) -> String {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status}: {body}"
    );
    let code = error_code(body);
    assert_eq!(
        code, "AccessDenied",
        "case {case:?}: expected the error code AccessDenied, got {code:?}: {body}"
    );
    let message = message_of(body);
    let reason = reason_of(
        case,
        &message,
        estate.principal(cause),
        operation.iam_action,
        &format!("user {target}"),
    );
    let expected = cause.reason(operation.iam_action, &estate.boundary_arn);
    assert_eq!(
        reason, expected,
        "case {case:?}: expected the clause naming the document that decided the refusal, \
         {expected:?}, got {reason:?} in {message:?}"
    );
    reason
}

/// RULE: each of the four documents that can decide a refusal is named by its OWN clause —
/// the identity-based policies that granted nothing, the explicit `Deny` in them, the
/// explicit `Deny` in the permissions boundary capping them (by the boundary's ARN) and the
/// explicit `Deny` in the session policies the credential was minted with — so the four
/// clauses of one action are pairwise distinct, whichever operation asked and whichever
/// credential class signed.
#[tokio::test]
async fn each_denial_cause_is_named_by_its_own_reason_clause() {
    let estate = estate().await;

    for operation in OPERATIONS {
        let mut seen: Vec<(Cause, String)> = Vec::new();
        for cause in CAUSES.iter().copied() {
            let target = cause.target();
            let case = format!("{cause:?} refusing {} on {target}", operation.iam_action);
            let (status, body) = estate.ask(cause, operation, target).await;
            let reason = assert_refusal(
                &case,
                status,
                &body,
                &estate,
                cause,
                operation,
                target,
            );
            seen.push((cause, reason));
        }

        // Pairwise distinct: a client that cannot tell the four apart cannot tell what to
        // fix, so two causes answering the same clause is the defect even where each clause
        // read on its own is the documented one.
        for (first, (one_cause, one)) in seen.iter().enumerate() {
            for (other_cause, other) in seen.iter().skip(first + 1) {
                assert_ne!(
                    one, other,
                    "case {:?} vs {:?} on {}: the two causes must be worded differently, both \
                     answered {one:?}",
                    one_cause, other_cause, operation.iam_action
                );
            }
        }
    }
}

/// RULE: a permissions boundary and a session policy NARROW what a principal may do rather
/// than revoking it — "the permissions boundary sets the maximum permissions that the
/// entity can have", and a session's permissions are the intersection of the role's
/// policies and the session policies — so the same principal whose call on one target is
/// refused is still SERVED the same operation on a target the documents it runs under
/// allow.
#[tokio::test]
async fn a_narrowed_principal_is_still_served_what_its_documents_allow() {
    let estate = estate().await;

    // Both credential classes: the bounded user's own long-term key, and the scoped
    // session's temporary one.
    for cause in [Cause::BoundaryDeny, Cause::SessionDeny] {
        for operation in OPERATIONS {
            let case = format!(
                "{cause:?}'s credential asking {} about the control target",
                operation.iam_action
            );
            let (status, body) = estate.ask(cause, operation, estate.control).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the narrowing document allows this call, so it must be served, \
                 got {status}: {body}"
            );
        }
    }
}
