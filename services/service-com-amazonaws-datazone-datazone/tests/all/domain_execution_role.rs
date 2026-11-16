// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `CreateDomain` settles about the ROLES a domain is run as,
//! before it stores anything.
//!
//! Two rules under test, in the order the service applies them:
//!
//! 1. `domainExecutionRole` is a member DataZone itself requires though the model leaves it
//!    optional, so a create that names no value for it is refused `ValidationException`
//!    (HTTP 400) in the request boundary's own wording for an absent required member — the
//!    AWS validation envelope over one `Member must not be null` clause naming the member —
//!    and nothing is stored.
//! 2. Every role the domain would be taken on AS — its `domainExecutionRole` and its
//!    `serviceRole` — is resolved against this account's own IAM, and one IAM ANSWERS the
//!    service cannot assume (this account holds no such role, or the trust document it
//!    carries does not admit the DataZone service principal) is refused
//!    `AccessDeniedException` (HTTP 403) naming that role, with nothing stored. A role
//!    nothing can be shown about is not refused: an emulator with no IAM to ask refuses
//!    nothing, which is why every other suite in this crate, mounting DataZone alone,
//!    creates domains with roles no IAM record backs.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which other
//! members the create carries, what the role's trust document says (which principal, which
//! action, which effect, which casing, how many statements, whether it is a policy at all),
//! whether the account holds the role, which member names it, and the form the ARN takes.
//! Every call goes over the wire through the shared [`crate::harness`] emulator, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, role_arn, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState,
    DEFAULT_PATH,
};
use std::sync::Arc;

use crate::harness::{domain_execution_role, Emulator};

/// The service principal a domain's roles have to admit, written out here rather than read
/// from the crate's own constant so this suite pins it.
const DATAZONE_PRINCIPAL: &str = "datazone.amazonaws.com";

/// The whole `message` a create naming no value for `member` is refused with: the AWS
/// validation envelope over one clause, whose only variable part is the member name.
fn member_must_not_be_null(member: &str) -> String {
    format!(
        "1 validation error detected: Value null at '{member}' failed to satisfy constraint: \
         Member must not be null"
    )
}

/// The whole `message` a create naming a role the service cannot assume is refused with: the
/// fixed sentence and the role ARN as the request sent it, quoted.
fn unable_to_assume(role_arn: &str) -> String {
    format!("Unable to assume role '{role_arn}'")
}

/// Seed `role_name` into the shared engine's IAM store carrying `trust` as its
/// `AssumeRolePolicyDocument` — the record `CreateRole` writes — and answer the ARN IAM
/// holds it under.
fn seed_role(
    engine: &Arc<engine::InMemoryEngine>,
    role_name: &str,
    trust: Option<String>,
) -> String {
    let state = engine.state::<IamState>();
    let mut store = state.lock();
    let arn = role_arn(DEFAULT_PATH, role_name);
    store.roles.insert(
        name_key(role_name),
        Role {
            path: DEFAULT_PATH.to_string(),
            role_name: role_name.to_string(),
            role_id: format!("AROA{}", role_name.to_uppercase()),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            assume_role_policy_document: trust,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    arn
}

/// A trust document with one statement: `effect` for `principal` performing `action`.
fn trust(effect: &str, principal: &str, action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": action,
        }],
    })
    .to_string()
}

/// The trust document a role DataZone can assume carries.
fn datazone_trust() -> String {
    trust("Allow", DATAZONE_PRINCIPAL, "sts:AssumeRole")
}

/// How many domains `ListDomains` reports carrying `name` — what says whether a refused
/// create stored anything.
async fn domains_named(dz: &Emulator, name: &str) -> usize {
    let listed = dz.call("GET", "/v2/domains", None).await;
    assert_eq!(
        listed.status, 200,
        "ListDomains: expected 200, got {} {:?}",
        listed.status, listed.body
    );
    listed
        .body
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("name").and_then(Value::as_str) == Some(name))
                .count()
        })
        .unwrap_or(0)
}

/// RULE 1: a create naming no `domainExecutionRole` is refused 400 `ValidationException` with
/// the absent-required-member wording, whatever else it carries, and stores no domain.
///
/// The rows differ along which other members the create names: none but the name, a
/// description, tags, an idempotency token, a domain version, the OTHER role member, a KMS
/// key and single sign-on.
#[tokio::test]
async fn a_create_naming_no_execution_role_is_refused_and_stores_nothing() {
    let dz = Emulator::new();
    let expected = member_must_not_be_null("domainExecutionRole");

    // (case, the body the create sends — none of them names `domainExecutionRole`)
    let rows: Vec<(&str, Value)> = vec![
        ("the name alone", json!({ "name": "no-role-1" })),
        (
            "a described domain",
            json!({ "name": "no-role-2", "description": "a catalog" }),
        ),
        (
            "a domain carrying tags",
            json!({ "name": "no-role-3", "tags": { "team": "data", "env": "test" } }),
        ),
        (
            "a domain carrying an idempotency token",
            json!({ "name": "no-role-4", "clientToken": "no-role-token" }),
        ),
        (
            "a V2 domain",
            json!({ "name": "no-role-5", "domainVersion": "V2" }),
        ),
        (
            "a domain naming only the service role",
            json!({
                "name": "no-role-6",
                "serviceRole": "arn:aws:iam::123456789012:role/DZService",
            }),
        ),
        (
            "a domain naming a KMS key and single sign-on",
            json!({
                "name": "no-role-7",
                "kmsKeyIdentifier":
                    "arn:aws:kms:us-west-2:123456789012:key/1234abcd-12ab-34cd-56ef-1234567890ab",
                "singleSignOn": { "type": "IAM_IDC", "userAssignment": "MANUAL" },
            }),
        ),
    ];

    for (case, body) in &rows {
        let name = body["name"].as_str().expect("the row names the domain");
        let answer = dz.call("POST", "/v2/domains", Some(body.clone())).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the create to be refused, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code, "ValidationException",
            "case {case:?}: expected ValidationException, got {:?} {:?}",
            answer.error_code, answer.body
        );
        assert_eq!(
            answer.s("message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            answer.s("message")
        );
        assert!(
            answer.body.get("id").is_none(),
            "case {case:?}: a refused create must mint no domain, got {:?}",
            answer.body
        );
        assert_eq!(
            domains_named(&dz, name).await,
            0,
            "case {case:?}: a refused create must store no domain named {name:?}"
        );
    }

    // The control for every row: the SAME bodies with the member named are accepted, so the
    // rows above are about that one member and not about anything else they carry. The
    // created domain is read back through `GetDomain` carrying the role it was created with.
    for (case, body) in &rows {
        let created = dz.create_domain(body.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {case:?} (with the role): expected 201, got {} {:?}",
            created.status, created.body
        );
        let id = created.s("id");
        let read = dz.call("GET", &format!("/v2/domains/{id}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?} (with the role): expected 200 from GetDomain, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.s("domainExecutionRole"),
            domain_execution_role(),
            "case {case:?} (with the role): GetDomain must report the persisted role, got {:?}",
            read.body
        );
    }
}

/// RULE 1, continued: the refused create consumes nothing — the idempotency token it carried
/// is still free, so the same token with the member named creates a domain rather than
/// replaying a refusal.
#[tokio::test]
async fn a_refused_create_leaves_its_idempotency_token_unused() {
    let dz = Emulator::new();
    let token = "token-of-a-refused-create";
    let refused = dz
        .call(
            "POST",
            "/v2/domains",
            Some(json!({ "name": "token-no-role", "clientToken": token })),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "expected the create to be refused, got {} {:?}",
        refused.status, refused.body
    );

    let created = dz
        .create_domain(json!({ "name": "token-no-role", "clientToken": token }))
        .await;
    assert_eq!(
        created.status, 201,
        "a token no create consumed must still mint a domain, got {} {:?}",
        created.status, created.body
    );
}

/// What one row of the resolution table asks: the trust document the account holds for the
/// row's role, and whether the create must be admitted.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The role the account holds under the row's role name, as its trust document — or
    /// `None` for a row whose role the account does not hold at all.
    held: Option<Option<String>>,
    /// Whether the create must be admitted.
    admitted: bool,
}

/// The trust documents, and the absences of one, every role member is put through.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a trust document admitting the DataZone principal",
            held: Some(Some(datazone_trust())),
            admitted: true,
        },
        Row {
            case: "a service wildcard in the action",
            held: Some(Some(trust("Allow", DATAZONE_PRINCIPAL, "sts:*"))),
            admitted: true,
        },
        Row {
            case: "the principal and the action named in another casing",
            held: Some(Some(trust(
                "Allow",
                "DATAZONE.amazonaws.com",
                "STS:AssumeRole",
            ))),
            admitted: true,
        },
        Row {
            case: "a document whose second statement admits the DataZone principal",
            held: Some(Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": { "Service": "ec2.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        },
                        {
                            "Effect": "Allow",
                            "Principal": { "Service": DATAZONE_PRINCIPAL },
                            "Action": "sts:AssumeRole",
                        },
                    ],
                })
                .to_string(),
            )),
            admitted: true,
        },
        Row {
            case: "a trust document admitting another service only",
            held: Some(Some(trust(
                "Allow",
                "lambda.amazonaws.com",
                "sts:AssumeRole",
            ))),
            admitted: false,
        },
        Row {
            case: "a trust document naming another action",
            held: Some(Some(trust("Allow", DATAZONE_PRINCIPAL, "s3:GetObject"))),
            admitted: false,
        },
        Row {
            case: "a trust document explicitly denying the DataZone principal",
            held: Some(Some(trust("Deny", DATAZONE_PRINCIPAL, "sts:AssumeRole"))),
            admitted: false,
        },
        Row {
            case: "a trust document carrying no statement",
            held: Some(Some(json!({ "Version": "2012-10-17" }).to_string())),
            admitted: false,
        },
        Row {
            case: "a trust document that is not a policy at all",
            held: Some(Some("this is not a policy".to_string())),
            admitted: false,
        },
        Row {
            case: "a role carrying no trust document",
            held: Some(None),
            admitted: false,
        },
        Row {
            case: "a role this account does not hold",
            held: None,
            admitted: false,
        },
    ]
}

/// RULE 2: a domain is created only for roles IAM shows DataZone can assume — asked of the
/// `domainExecutionRole` and of the `serviceRole` alike — and a refusal stores nothing.
///
/// The rows differ along what the rule generalises over: which principal the trust document
/// admits, which action it names, which effect it states, the casing it names them in, how
/// many statements it carries, whether it is readable as a policy at all, whether it is
/// present on the role, whether the account holds the role, and WHICH member of the request
/// names it.
#[tokio::test]
async fn a_domain_is_created_only_for_roles_iam_shows_datazone_can_assume() {
    // Which member of the request carries the row's role; the other names an assumable one.
    for member in ["domainExecutionRole", "serviceRole"] {
        let (dz, engine) = Emulator::with_iam();
        let assumable = seed_role(&engine, &format!("ok-role-{member}"), Some(datazone_trust()));

        for (index, row) in rows().iter().enumerate() {
            let case = format!("{member} / {}", row.case);
            let role_name = format!("admission-{member}-{index}");
            let name = format!("admission-{member}-{index}");
            let role = match &row.held {
                Some(trust) => seed_role(&engine, &role_name, trust.clone()),
                // Not seeded, so IAM answers that this account holds no such role. The ARN
                // is the one IAM would hold it under.
                None => role_arn(DEFAULT_PATH, &role_name),
            };
            let mut body = json!({
                "name": name,
                "domainExecutionRole": assumable,
                "serviceRole": assumable,
            });
            body[member] = json!(role);

            let answer = dz.call("POST", "/v2/domains", Some(body)).await;
            if row.admitted {
                assert_eq!(
                    answer.status, 201,
                    "case {case:?}: expected the create to be admitted, got {} {:?}",
                    answer.status, answer.body
                );
                // Round trip: the domain the create admitted is readable, carrying the role.
                let id = answer.s("id");
                let read = dz.call("GET", &format!("/v2/domains/{id}"), None).await;
                assert_eq!(
                    read.s(member),
                    role,
                    "case {case:?}: GetDomain must report the persisted {member}, got {:?}",
                    read.body
                );
                continue;
            }
            assert_eq!(
                answer.status, 403,
                "case {case:?}: expected the create to be denied, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code, "AccessDeniedException",
                "case {case:?}: expected AccessDeniedException, got {:?} {:?}",
                answer.error_code, answer.body
            );
            let expected = unable_to_assume(&role);
            assert_eq!(
                answer.s("message"),
                expected,
                "case {case:?}: expected the message {expected:?}, got {:?}",
                answer.s("message")
            );
            assert_eq!(
                domains_named(&dz, &name).await,
                0,
                "case {case:?}: a denied create must store no domain named {name:?}"
            );
        }
    }
}

/// RULE 2, over the FORMS a role member can name a role by: a role of this account is
/// resolved whatever path its ARN carries, while an ARN of ANOTHER account names a role this
/// emulator's IAM cannot be asked about — so nothing is shown and nothing is refused.
#[tokio::test]
async fn a_role_member_is_resolved_only_where_this_accounts_iam_can_answer() {
    let (dz, engine) = Emulator::with_iam();
    seed_role(&engine, "form-datazone-role", Some(datazone_trust()));
    seed_role(
        &engine,
        "form-ec2-role",
        Some(trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole")),
    );
    let account = ::engine::emulator_account_id();

    // (case, the `domainExecutionRole` value, whether the create must be admitted)
    let forms: Vec<(&str, String, bool)> = vec![
        (
            "this account's ARN of a role trusting DataZone",
            role_arn(DEFAULT_PATH, "form-datazone-role"),
            true,
        ),
        (
            "the same role under a service-role path, which IAM holds by name",
            format!("arn:aws:iam::{account}:role/service-role/form-datazone-role"),
            true,
        ),
        (
            "this account's ARN of a role not trusting DataZone",
            role_arn(DEFAULT_PATH, "form-ec2-role"),
            false,
        ),
        (
            "this account's ARN of a role it does not hold",
            role_arn(DEFAULT_PATH, "form-absent-role"),
            false,
        ),
        (
            // Another account's IAM is not this emulator's to ask, so nothing is shown
            // about the role and the create goes ahead.
            "another account's role ARN, which this IAM cannot be asked about",
            "arn:aws:iam::123456789012:role/their-datazone-role".to_string(),
            true,
        ),
        (
            "another partition's ARN of a role this account does not hold",
            "arn:aws-us-gov:iam::123456789012:role/their-datazone-role".to_string(),
            true,
        ),
    ];

    for (index, (case, role, admitted)) in forms.iter().enumerate() {
        let name = format!("form-domain-{index}");
        let answer = dz
            .call(
                "POST",
                "/v2/domains",
                Some(json!({ "name": name, "domainExecutionRole": role })),
            )
            .await;
        if *admitted {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected the create to be admitted, got {} {:?}",
                answer.status, answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 403,
            "case {case:?}: expected the create to be denied, got {} {:?}",
            answer.status, answer.body
        );
        let expected = unable_to_assume(role);
        assert_eq!(
            answer.s("message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            answer.s("message")
        );
        assert_eq!(
            domains_named(&dz, &name).await,
            0,
            "case {case:?}: a denied create must store no domain named {name:?}"
        );
    }
}

/// The roles are resolved BEFORE the name uniqueness rule and before the idempotency token,
/// so a request that is wrong in two ways at once reports the ROLE — and the same request
/// with assumable roles is still refused for the name, which is what makes this about
/// precedence rather than about the other rule having gone away.
///
/// The rows differ along which member carries the un-assumable role and whether the request
/// also carries a token.
#[tokio::test]
async fn the_roles_are_judged_before_the_name_and_the_token() {
    let (dz, engine) = Emulator::with_iam();
    let assumable = seed_role(&engine, "order-datazone-role", Some(datazone_trust()));
    let unassumable = seed_role(
        &engine,
        "order-ec2-role",
        Some(trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole")),
    );
    let taken = "already-taken-domain";
    let first = dz
        .call(
            "POST",
            "/v2/domains",
            Some(json!({ "name": taken, "domainExecutionRole": assumable })),
        )
        .await;
    assert_eq!(
        first.status, 201,
        "the first create must be accepted, got {} {:?}",
        first.status, first.body
    );

    // (case, the members the row sends beside the name)
    let rows: Vec<(&str, Value)> = vec![
        (
            "an un-assumable execution role on a name already in use",
            json!({ "domainExecutionRole": unassumable }),
        ),
        (
            "an un-assumable service role on a name already in use",
            json!({ "domainExecutionRole": assumable, "serviceRole": unassumable }),
        ),
        (
            "an un-assumable execution role and a fresh token",
            json!({ "domainExecutionRole": unassumable, "clientToken": "order-token" }),
        ),
    ];

    for (case, members) in &rows {
        let mut body = members.clone();
        body["name"] = json!(taken);
        let answer = dz.call("POST", "/v2/domains", Some(body)).await;
        assert_eq!(
            answer.status, 403,
            "case {case:?}: the role is judged first, so expected 403, got {} {:?}",
            answer.status, answer.body
        );
        let expected = unable_to_assume(&unassumable);
        assert_eq!(
            answer.s("message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            answer.s("message")
        );
    }

    // With assumable roles the SAME name is refused for the name instead.
    let conflict = dz
        .call(
            "POST",
            "/v2/domains",
            Some(json!({ "name": taken, "domainExecutionRole": assumable })),
        )
        .await;
    assert_eq!(
        (conflict.status, conflict.error_code.as_str()),
        (409, "ConflictException"),
        "a name already in use must still be the conflict, got {} {:?}",
        conflict.status,
        conflict.body
    );
    assert_eq!(
        domains_named(&dz, taken).await,
        1,
        "exactly one domain named {taken:?} must remain"
    );
}
