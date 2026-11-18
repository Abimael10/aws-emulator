// SPDX-License-Identifier: CC0-1.0

//! WHO may drive this front door's operations, end to end over the wire this service speaks
//! (`awsQuery`), with the role, its policies, the user and the session held by the IAM and
//! STS services mounted on the SAME engine — so the documents under test are the ones
//! `iam:PutRolePolicy` stores and the principal a refusal blames is the one `sts:AssumeRole`
//! minted.
//!
//! THE RULE UNDER TEST: AWS decides authorization at the IAM layer, in front of the service,
//! so a request whose calling principal's identity-based policies do not allow
//! `rds:<OperationName>` — or that an explicit `Deny` covers — is refused with HTTP 403 and
//! the Query API error code `AccessDenied`, naming the principal, the action and which of
//! IAM's rules withheld the grant, and reads nothing and changes nothing; a request those
//! policies allow is served the operation's own answer. The decision is a fact about the
//! credential the request presented, so it is the same decision whichever operation of this
//! front door the request names.
//!
//! The rows span what the rule generalises over: the OPERATION (two reads of different
//! resource kinds and a create, so a grant of one says nothing about the others), the
//! DOCUMENTS in force (none at all, a grant of the whole service, a grant of exactly one
//! action, a grant written in another case, a grant scoped to one resource's ARN, an explicit
//! deny beside a wildcard allow), and the KIND of credential (an assumed-role session, an IAM
//! user's long-term access key, the account's own published credential).

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{text, REGION};

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIDEMULATOR";

/// The API versions a client sends in each service's query body.
const RDS_API_VERSION: &str = "2014-10-31";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// This service's IAM action prefix and the code it refuses an unauthorized caller with,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const ACTION_PREFIX: &str = "rds";
const ACCESS_DENIED: &str = "AccessDenied";

/// The status an authorization refusal carries.
const DENIED_STATUS: u16 = 403;

/// One operation the rows drive, with the parameters a client sends with it: two reads over
/// different resource kinds and one create, each its own `rds:` action.
const DRIVEN: [(&str, &[(&str, &str)]); 3] = [
    ("DescribeDBInstances", &[]),
    ("DescribeDBClusters", &[]),
    (
        "CreateDBParameterGroup",
        &[
            ("DBParameterGroupFamily", "mysql8.0"),
            ("Description", "authorization probe"),
        ],
    ),
];

/// What a row expects a call to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the policies in force allow the action.
    Served,
    /// HTTP 403 `AccessDenied` because a statement about the request says `Deny`.
    ExplicitDeny,
    /// HTTP 403 `AccessDenied` because no readable statement allows the action at all.
    NoAllow,
}

/// This emulator's account id, which every ARN below carries.
fn account() -> &'static str {
    ::engine::emulator_account_id()
}

/// The emulator the rows drive: this service beside the IAM and STS services on ONE shared
/// engine, exactly as the assembled router mounts them, so the policies a row stores and the
/// session it mints are the ones the gate reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Verifying signatures is the emulator's concern, not this service's; these tests sign
    // nothing, so the scope header is read for the key id and the Region alone.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// One form value as a client puts it in an `application/x-www-form-urlencoded` body: every
/// byte outside the unreserved set is percent-escaped, so a policy document travels as an SDK
/// would send it.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// One query-protocol call to `service`, presented as signed by the access key id `akid` —
/// the only place the credential appears on the wire, and so what the service reads the
/// caller's identity from.
async fn call_as(
    router: &MultiServiceRouter,
    service: &str,
    api_version: &str,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={api_version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = if service == "iam" {
        "iam.amazonaws.com".to_string()
    } else {
        format!("{service}.{REGION}.amazonaws.com")
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{REGION}/{service}/aws4_request, \
                 SignedHeaders=host, Signature=unverified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One call to THIS service, signed by `akid`.
async fn rds_as(
    router: &MultiServiceRouter,
    akid: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    call_as(router, "rds", RDS_API_VERSION, akid, action, params).await
}

/// One IAM call, made by the account's own published credential — the identity that
/// provisions the roles and users the rows are about.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    call_as(
        router,
        "iam",
        IAM_API_VERSION,
        ACCOUNT_KEY_ID,
        action,
        params,
    )
    .await
}

/// A trust policy admitting the account's own principals to assume the role, which is what
/// lets the published credential mint the session a row signs with.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole"}}]}}"#,
        account()
    )
}

/// A one-statement identity-based document: `effect` on `action` over `resource`.
fn statement(effect: &str, action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// The one `<tag>` of a query answer, with the XML entities resolved.
fn member(body: &str, tag: &str) -> String {
    text(body, tag)
}

/// Create the role `role_name` with `inline` policies put on it, then assume it: answers the
/// session's access key id and the ARN `sts:GetCallerIdentity` reports for it, which a refusal
/// must name.
async fn session_of(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &[(&str, String)],
) -> (String, String) {
    let trust = trust_policy();
    let (status, created) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be created, got {status} {created}"
    );
    for (policy_name, document) in inline {
        let (status, put) = iam_call(
            router,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", policy_name),
                ("PolicyDocument", document),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the inline policy {policy_name:?} must be stored, got {status} {put}"
        );
    }
    let role_arn = format!("arn:aws:iam::{}:role/{role_name}", account());
    let session_name = format!("{role_name}-session");
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", &session_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must be assumable, got {status} {assumed}"
    );
    let akid = member(&assumed, "AccessKeyId");
    let arn = member(&assumed, "Arn");
    assert!(
        !akid.is_empty() && !arn.is_empty(),
        "case {case:?}: the session must report its key and its ARN, got {assumed}"
    );
    (akid, arn)
}

/// Create the IAM user `user_name` and mint its long-term access key: answers that key's id
/// and the user's ARN.
async fn user_key_of(router: &MultiServiceRouter, case: &str, user_name: &str) -> (String, String) {
    let (status, created) = iam_call(router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user must be created, got {status} {created}"
    );
    let user_arn = member(&created, "Arn");
    let (status, minted) = iam_call(router, "CreateAccessKey", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the access key must be minted, got {status} {minted}"
    );
    (member(&minted, "AccessKeyId"), user_arn)
}

/// The clause a refusal closes with, naming which of IAM's rules withheld the grant.
fn refusal_reason(action: &str, expected: Expect) -> String {
    match expected {
        Expect::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Expect::NoAllow => format!("because no identity-based policy allows the {action} action"),
        Expect::Served => unreachable!("a served call carries no refusal"),
    }
}

/// The sentence this service refuses `principal` asking for `action` with.
fn refusal_message(principal: &str, action: &str, expected: Expect) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} {}",
        refusal_reason(action, expected)
    )
}

/// Drive `operation` as `akid` and assert it answered what `expected` requires of it — and,
/// when it was refused, that the refusal carries this service's code, status and sentence.
async fn assert_call(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    operation: &str,
    params: &[(&str, &str)],
    expected: Expect,
) {
    // The create needs a name of its own per caller, so a row that is SERVED twice is not
    // refused the second time for a name the account already holds.
    let name = format!("emu-authz-{}", principal_slug(principal));
    let mut sent: Vec<(&str, &str)> = params.to_vec();
    if operation == "CreateDBParameterGroup" {
        sent.insert(0, ("DBParameterGroupName", name.as_str()));
    }
    let (status, body) = rds_as(router, akid, operation, &sent).await;
    match expected {
        Expect::Served => assert_eq!(
            status, 200,
            "case {case:?} / {operation}: expected the call to be served, got {status} {body}"
        ),
        refused => {
            assert_eq!(
                status, DENIED_STATUS,
                "case {case:?} / {operation}: expected HTTP {DENIED_STATUS}, got {status} {body}"
            );
            assert_eq!(
                member(&body, "Code"),
                ACCESS_DENIED,
                "case {case:?} / {operation}: expected {ACCESS_DENIED}, got {body}"
            );
            let action = format!("{ACTION_PREFIX}:{operation}");
            assert_eq!(
                member(&body, "Message"),
                refusal_message(principal, &action, refused),
                "case {case:?} / {operation}: expected IAM's refusal wording, got {body}"
            );
            assert!(
                !body.contains("on resource"),
                "case {case:?} / {operation}: a decision taken over every resource names none, \
                 got {body}"
            );
        }
    }
}

/// A principal's ARN folded to the characters a resource name admits, so each caller's create
/// addresses a name of its own.
fn principal_slug(principal: &str) -> String {
    principal
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// One row: the documents in force on the role the caller is a session of, and what each of
/// the three driven operations must answer.
struct Row {
    case: &'static str,
    role: &'static str,
    inline: Vec<(&'static str, String)>,
    /// What the three [`DRIVEN`] operations answer, in that order.
    expect: [Expect; 3],
}

/// RULE: an assumed-role session's call is decided by the identity-based documents in force
/// on its ROLE — an explicit `Deny` about the action refuses whatever any `Allow` beside it
/// says, an `Allow` covering the action serves it, and with neither the request falls to IAM's
/// default deny — whichever operation of this front door it names.
#[tokio::test]
async fn the_documents_in_force_decide_every_operation() {
    let instance_arn = format!("arn:aws:rds:{REGION}:{}:db:orders", account());
    let rows = vec![
        Row {
            case: "a role carrying no policy at all",
            role: "rds-bare-role",
            inline: Vec::new(),
            expect: [Expect::NoAllow; 3],
        },
        Row {
            case: "a role allowed every action of this service",
            role: "rds-full-role",
            inline: vec![("allow-rds", statement("Allow", "rds:*", "*"))],
            expect: [Expect::Served; 3],
        },
        Row {
            case: "a role allowed exactly one of the reads",
            role: "rds-instances-only-role",
            inline: vec![(
                "allow-describe-instances",
                statement("Allow", "rds:DescribeDBInstances", "*"),
            )],
            expect: [Expect::Served, Expect::NoAllow, Expect::NoAllow],
        },
        Row {
            case: "a role allowed the reads with one of them explicitly denied",
            role: "rds-denied-instances-role",
            inline: vec![
                ("allow-describes", statement("Allow", "rds:Describe*", "*")),
                (
                    "deny-describe-instances",
                    statement("Deny", "rds:DescribeDBInstances", "*"),
                ),
            ],
            expect: [Expect::ExplicitDeny, Expect::Served, Expect::NoAllow],
        },
        Row {
            case: "a role allowed everything under a wildcard deny over the reads",
            role: "rds-wildcard-denied-role",
            inline: vec![
                ("allow-rds", statement("Allow", "rds:*", "*")),
                ("deny-describes", statement("Deny", "rds:Describe*", "*")),
            ],
            expect: [
                Expect::ExplicitDeny,
                Expect::ExplicitDeny,
                Expect::Served,
            ],
        },
        Row {
            case: "a role allowed one read in another case",
            // "IAM compares action names case-insensitively", so the spelling of the
            // statement's action is not what decides the call.
            role: "rds-mixed-case-role",
            inline: vec![(
                "allow-mixed-case",
                statement("Allow", "RDS:describedbinstances", "*"),
            )],
            expect: [Expect::Served, Expect::NoAllow, Expect::NoAllow],
        },
        Row {
            case: "a role whose grant of one read is scoped to one instance's ARN",
            // The gate resolves no resource ARN, so a grant of the action scoped to
            // particular resources reaches the operation rather than being refused on a
            // resource element nothing read — while the actions the document says nothing
            // about are still refused.
            role: "rds-resource-scoped-role",
            inline: vec![(
                "allow-one-instance",
                statement("Allow", "rds:DescribeDBInstances", &instance_arn),
            )],
            expect: [Expect::Served, Expect::NoAllow, Expect::NoAllow],
        },
    ];

    let router = emulator();
    for row in &rows {
        let (akid, principal) = session_of(&router, row.case, row.role, &row.inline).await;
        for ((operation, params), expected) in DRIVEN.iter().zip(row.expect.iter()) {
            assert_call(
                &router,
                row.case,
                &akid,
                &principal,
                operation,
                params,
                *expected,
            )
            .await;
        }
    }
}

/// RULE: the principal a request is judged on is the one its access key id speaks for — the
/// session STS minted the key for, the IAM user that holds a long-term key, or the account's
/// own published credential, which holds no IAM policies and is therefore not bounded by them.
#[tokio::test]
async fn the_credential_decides_which_principal_the_call_is_judged_on() {
    let router = emulator();

    // The account's own published credential: a key neither store holds a record for.
    for (operation, params) in DRIVEN {
        // The create is driven through the same helper as every other caller's, so the name
        // it addresses is this caller's own.
        let principal = format!("arn:aws:iam::{}:root", account());
        assert_call(
            &router,
            "the account's own credential",
            ACCOUNT_KEY_ID,
            &principal,
            operation,
            params,
            Expect::Served,
        )
        .await;
    }

    // An IAM user holding no policy at all is refused; the same user, once a policy allowing
    // the family is put on it, is served.
    let case = "an IAM user with no policy";
    let (user_akid, user_arn) = user_key_of(&router, case, "rds-bare-user").await;
    for (operation, params) in DRIVEN {
        assert_call(
            &router,
            case,
            &user_akid,
            &user_arn,
            operation,
            params,
            Expect::NoAllow,
        )
        .await;
    }
    let (status, put) = iam_call(
        &router,
        "PutUserPolicy",
        &[
            ("UserName", "rds-bare-user"),
            ("PolicyName", "allow-rds"),
            ("PolicyDocument", &statement("Allow", "rds:*", "*")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the user's policy must be stored, got {status} {put}"
    );
    for (operation, params) in DRIVEN {
        assert_call(
            &router,
            "an IAM user allowed the service",
            &user_akid,
            &user_arn,
            operation,
            params,
            Expect::Served,
        )
        .await;
    }
}

/// RULE: the decision is taken before the operation reads or writes anything, so a refused
/// read reports nothing of the account's estate and a refused create leaves nothing behind —
/// and what the account holds is exactly what a caller the policies allow reads back.
#[tokio::test]
async fn a_refused_call_reports_nothing_and_changes_nothing() {
    let router = emulator();
    let instance = "emu-authz-estate";
    let (status, created) = rds_as(
        &router,
        ACCOUNT_KEY_ID,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", instance),
            ("Engine", "postgres"),
            ("DBInstanceClass", "db.t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the instance must be created, got {status} {created}"
    );

    let case = "a role denied the read of the instances";
    let (akid, principal) = session_of(
        &router,
        case,
        "rds-estate-denied-role",
        &[
            ("allow-rds", statement("Allow", "rds:*", "*")),
            (
                "deny-describe-instances",
                statement("Deny", "rds:DescribeDBInstances", "*"),
            ),
        ],
    )
    .await;
    let (status, refused) = rds_as(&router, &akid, "DescribeDBInstances", &[]).await;
    assert_eq!(
        status, DENIED_STATUS,
        "case {case:?}: the denied read must be refused, got {status} {refused}"
    );
    assert!(
        !refused.contains(instance),
        "case {case:?}: a refused read discloses no instance of the account, got {refused}"
    );
    assert_eq!(
        member(&refused, "Message"),
        refusal_message(
            &principal,
            &format!("{ACTION_PREFIX}:DescribeDBInstances"),
            Expect::ExplicitDeny
        ),
        "case {case:?}: expected IAM's refusal wording, got {refused}"
    );

    // A create no document allows writes nothing: the group it named is not in the account
    // afterwards, read back as a caller the policies allow.
    let group = "emu-authz-unwritten";
    let case = "a role carrying no policy at all";
    let (bare_akid, _) = session_of(&router, case, "rds-estate-bare-role", &[]).await;
    let (status, denied_create) = rds_as(
        &router,
        &bare_akid,
        "CreateDBParameterGroup",
        &[
            ("DBParameterGroupName", group),
            ("DBParameterGroupFamily", "mysql8.0"),
            ("Description", "authorization probe"),
        ],
    )
    .await;
    assert_eq!(
        status, DENIED_STATUS,
        "case {case:?}: the unauthorized create must be refused, got {status} {denied_create}"
    );
    let (status, groups) = rds_as(
        &router,
        ACCOUNT_KEY_ID,
        "DescribeDBParameterGroups",
        &[("DBParameterGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: the refused create must have written nothing, got {status} {groups}"
    );

    // Read back through the same operation as a caller the policies allow: the estate the
    // refusal did not disclose is still exactly what the account holds.
    let (status, reported) = rds_as(&router, ACCOUNT_KEY_ID, "DescribeDBInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the permitted read must be served, got {status} {reported}"
    );
    assert!(
        reported.contains(instance),
        "case {case:?}: expected the instance to still be reported, got {reported}"
    );
}
