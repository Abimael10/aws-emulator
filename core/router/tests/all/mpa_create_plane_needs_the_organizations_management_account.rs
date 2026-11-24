// SPDX-License-Identifier: CC0-1.0

//! The multi-party approval creates that SET AN ORGANIZATION'S approval up —
//! `CreateApprovalTeam` and `CreateIdentitySource` — are reserved to that organization's
//! MANAGEMENT ACCOUNT, and which account that is belongs to AWS Organizations. So the
//! standing the gate judges by is reachable over the wire: a client calls
//! `Organizations:CreateOrganization`, which makes the calling account the management
//! account, and the mpa create plane opens up behind it. Only a cross-service suite can
//! pin that; each crate's own tests can assert its own half alone.
//!
//! The rule these tables span: for EVERY operation mpa reserves to the management
//! account, a caller the organization does not name as its management account is refused
//! HTTP 403 `AccessDeniedException` with one fixed sentence, whatever the request body
//! carries, however often the same request is repeated and however many arrive at once,
//! and the call records nothing — no resource, no `Name` taken, no idempotent answer.
//! The decision takes no value from the request, so it is reached before the body is
//! read: a body that breaks a modelled constraint, one that breaks a rule the service
//! itself owns and one that breaks nothing all earn the SAME refusal, and the refusal
//! quotes nothing from the request. Standing in front of validation does not replace it:
//! once the organization records the caller as its management account, each of those same
//! requests earns its OWN answer — served and readable back, or refused by the rule it
//! breaks.
//!
//! The rows differ along the gated OPERATION, along WHAT the body breaks (nothing; a
//! modelled `@pattern` or `@length`; a rule the service owns: the M-of-N approver count,
//! a repeated approver, an approver identity source that is no ARN of one, an
//! unattachable policy reference, the reserved tag namespace, a Region that does not
//! exist), and along HOW the call arrives (once, repeated, carrying an idempotency token,
//! eight at once). Every modelled bound in a comment names the member it comes from
//! (`smithy_query(query="operation",operation="CreateApprovalTeam,CreateIdentitySource")`,
//! `smithy_query(query="shape",shape="ApprovalTeamName,QualifiedPolicyArn,IdcInstanceArn")`).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Region every request below is signed for, and so the Region of the ARNs mpa mints
/// here.
const REGION: &str = "us-east-1";

/// The wire error code of the refusal a caller outside the organization's management
/// account earns — `AccessDeniedException`, `@httpError(403)`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The `Message` that refusal carries: the whole sentence is fixed text, with no part
/// taken from the request or from state (note the typographic apostrophe in
/// "organization's").
const MANAGEMENT_ACCOUNT_ONLY: &str =
    "Action can only be performed by an organization\u{2019}s management account.";

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the front door reads. The signature is never checked: the router here runs
/// with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// mpa and Organizations mounted on ONE engine, as `core/router/src/lib.rs` mounts them,
/// over the state a freshly booted emulator holds: an account in no organization.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_mpa_awsfluffycoreservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(engine),
    );
    router
}

/// One mpa call, over its `restJson1` wire: the operation's own method and URI.
async fn mpa(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let bytes = match body {
        None => Bytes::new(),
        Some(value) => Bytes::from(serde_json::to_vec(value).expect("the request body serializes")),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("mpa.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("mpa"))
        .body(bytes)
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, decoded)
}

/// One Organizations call, over its awsJson1_1 wire.
async fn organizations(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("organizations.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSOrganizationsV20161128.{operation}"),
        )
        .header("authorization", authorization("organizations"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, decoded)
}

/// The account every emulated service presents, and so the management account of the
/// organization `CreateOrganization` creates.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The one request a client can send to establish the fact the gate reads: the calling
/// account becomes the management account of the organization it creates.
async fn given_an_organization(router: &MultiServiceRouter) {
    let (status, created) = organizations(router, "CreateOrganization", &json!({})).await;
    assert_eq!(
        status, 200,
        "the organization is created over the wire, got {status} {created:?}"
    );
    assert_eq!(
        created["Organization"]["MasterAccountId"].as_str(),
        Some(account()),
        "the calling account manages the new organization, got {created:?}"
    );
}

/// The `__type` a `restJson1` error body carries, without any namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A modelled error's `Message`.
fn message(body: &Value) -> &str {
    body.get("Message").and_then(Value::as_str).unwrap_or_default()
}

/// Percent-encode an ARN for a non-greedy `@httpLabel`, which binds ONE path segment.
fn encode_label(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            '$' => "%24".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// The qualified ARN of a policy version the catalog publishes, discovered the way a
/// client discovers it: `ListPolicies` names the policy and `$DEFAULT` selects the version
/// it defaults to — the two forms `QualifiedPolicyArn`'s pattern admits are a version
/// number and `$DEFAULT`.
async fn published_policy_version(router: &MultiServiceRouter) -> String {
    let (status, listed) = mpa(router, "POST", "/policies/?List", None).await;
    assert_eq!(
        status, 200,
        "the published catalog answers ListPolicies, got {status} {listed:?}"
    );
    let arn = listed["Policies"]
        .as_array()
        .and_then(|policies| policies.first())
        .and_then(|policy| policy["Arn"].as_str())
        .unwrap_or_else(|| {
            panic!("the published catalog names at least one policy, got {listed:?}")
        });
    format!("{arn}/$DEFAULT")
}

/// An identity-source ARN in the GRAMMAR `PrimaryIdentitySourceArn` references one by —
/// the form mpa mints for an identity source, which the served create below reads back.
fn identity_source_arn(id: &str) -> String {
    format!("arn:aws:mpa:{REGION}:{}:identity-source/{id}", account())
}

/// One approver. `PrimaryIdentityId` is `@length(1..100)`; `PrimaryIdentitySourceArn`
/// references the identity source the approver authenticates through.
fn approver(n: usize) -> Value {
    json!({
        "PrimaryIdentityId": format!("identity-{n:04}"),
        "PrimaryIdentitySourceArn": identity_source_arn(&format!("ids-{n:012}")),
    })
}

/// A `CreateApprovalTeam` body.
fn team_body(name: &str, min_approvals: i64, approvers: Vec<Value>, policy_arn: &str) -> Value {
    json!({
        "Name": name,
        "Description": "the organization's approval team",
        "ApprovalStrategy": { "MofN": { "MinApprovalsRequired": min_approvals } },
        "Approvers": approvers,
        "Policies": [{ "PolicyArn": policy_arn }],
    })
}

/// A `CreateIdentitySource` body. `IamIdentityCenter` is the only member
/// `IdentitySourceParameters` declares.
fn identity_source_body(instance_arn: &str, region: &str) -> Value {
    json!({
        "IdentitySourceParameters": {
            "IamIdentityCenter": { "InstanceArn": instance_arn, "Region": region }
        }
    })
}

/// A well-formed IAM Identity Center instance ARN, satisfying `IdcInstanceArn`'s
/// `@pattern ^arn:.+:sso:::instance/(?:sso)?ins-[a-zA-Z0-9-.]{16}$` — an instance this
/// emulator has never been told about, which the gate decides nothing from.
const INSTANCE_ARN: &str = "arn:aws:sso:::instance/ssoins-1234567890abcdef";

/// Which gated operation a row drives, and where its resource is reported.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gated {
    ApprovalTeam,
    IdentitySource,
}

impl Gated {
    /// The create's own `@http` URI (`POST`).
    fn create_uri(self) -> &'static str {
        match self {
            Self::ApprovalTeam => "/approval-teams",
            Self::IdentitySource => "/identity-sources",
        }
    }

    /// That resource's list operation and the member it answers in.
    fn listing(self) -> (&'static str, &'static str) {
        match self {
            Self::ApprovalTeam => ("/approval-teams/?List", "ApprovalTeams"),
            Self::IdentitySource => ("/identity-sources/?List", "IdentitySources"),
        }
    }
}

/// What the SAME request earns once the organization records the caller as its management
/// account.
enum BehindTheGate {
    /// Served: the create is performed and the resource reads back through its own Get.
    Served,
    /// Refused by a rule of its own, with this status and this modelled error code.
    Refused { status: u16, code: &'static str },
}

/// One row: a request to a gated operation, and the answer it earns behind the gate.
struct Row {
    case: &'static str,
    gated: Gated,
    body: Value,
    behind_the_gate: BehindTheGate,
}

/// The modelled refusal of an input the service will not accept: `ValidationException`,
/// `@httpError(400)`.
fn validation() -> BehindTheGate {
    BehindTheGate::Refused {
        status: 400,
        code: "ValidationException",
    }
}

/// The table. `policy_arn` is a policy version the catalog publishes, so a body that
/// breaks nothing really is attachable.
fn rows(policy_arn: &str) -> Vec<Row> {
    vec![
        Row {
            case: "CreateApprovalTeam, a body every rule accepts",
            gated: Gated::ApprovalTeam,
            body: team_body("gate-conforming-team", 1, vec![approver(1)], policy_arn),
            behind_the_gate: BehindTheGate::Served,
        },
        Row {
            // The control call of the cross-field rule below: M equal to N is accepted.
            case: "CreateApprovalTeam, MinApprovalsRequired equal to the approver count",
            gated: Gated::ApprovalTeam,
            body: team_body(
                "gate-m-equals-n",
                2,
                vec![approver(1), approver(2)],
                policy_arn,
            ),
            behind_the_gate: BehindTheGate::Served,
        },
        Row {
            // `ApprovalTeamName`'s `@pattern ^[a-zA-Z0-9._-]+$`: a space is not in the
            // class, and botocore does not enforce a pattern client-side, so the value
            // reaches the service.
            case: "CreateApprovalTeam, Name breaks the modelled pattern (a space)",
            gated: Gated::ApprovalTeam,
            body: team_body("gate order team", 1, vec![approver(1)], policy_arn),
            behind_the_gate: validation(),
        },
        Row {
            // `ApprovalTeamName` is `@length(max: 64)`: one character past the bound.
            case: "CreateApprovalTeam, Name is one past the modelled length maximum (64)",
            gated: Gated::ApprovalTeam,
            body: team_body(&"n".repeat(65), 1, vec![approver(1)], policy_arn),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: `MofN` cannot ask for more approvals than the team
            // has approvers. The model bounds M at `@range(min: 1)` but cannot relate it
            // to the approver count.
            case: "CreateApprovalTeam, MinApprovalsRequired exceeds the approver count",
            gated: Gated::ApprovalTeam,
            body: team_body(
                "gate-m-over-n",
                3,
                vec![approver(1), approver(2)],
                policy_arn,
            ),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: a repeat inflates N without adding anyone who can
            // approve.
            case: "CreateApprovalTeam, Approvers names one identity twice",
            gated: Gated::ApprovalTeam,
            body: team_body(
                "gate-repeated-approver",
                1,
                vec![approver(1), approver(1)],
                policy_arn,
            ),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: `PrimaryIdentitySourceArn` carries no modelled
            // pattern, so a value that is no ARN of an identity source is refused here.
            case: "CreateApprovalTeam, an approver's identity source is no ARN of one",
            gated: Gated::ApprovalTeam,
            body: team_body(
                "gate-approver-arn",
                1,
                vec![json!({
                    "PrimaryIdentityId": "identity-0001",
                    "PrimaryIdentitySourceArn": "ssoins-1234567890abcdef",
                })],
                policy_arn,
            ),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: the reference satisfies `QualifiedPolicyArn`'s
            // pattern but names a policy version the catalog does not hold.
            case: "CreateApprovalTeam, Policies names a policy version the catalog lacks",
            gated: Gated::ApprovalTeam,
            body: team_body(
                "gate-unattachable-policy",
                1,
                vec![approver(1)],
                "arn:aws:mpa:::aws:policy/IdentityCenter/NoSuchPolicy/$DEFAULT",
            ),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: the `aws:` tag namespace is Amazon Web Services'
            // own, so no path that writes tags writes a key in it.
            case: "CreateApprovalTeam, Tags names a key in the reserved namespace",
            gated: Gated::ApprovalTeam,
            body: {
                let mut body = team_body("gate-reserved-tag", 1, vec![approver(1)], policy_arn);
                body["Tags"] = json!({ "aws:owner": "platform" });
                body
            },
            behind_the_gate: validation(),
        },
        Row {
            case: "CreateIdentitySource, a body every rule accepts",
            gated: Gated::IdentitySource,
            body: identity_source_body(INSTANCE_ARN, REGION),
            behind_the_gate: BehindTheGate::Served,
        },
        Row {
            // `IdcInstanceArn`'s pattern: this is not an IAM Identity Center instance.
            case: "CreateIdentitySource, InstanceArn breaks the modelled pattern",
            gated: Gated::IdentitySource,
            body: identity_source_body("arn:aws:iam::123456789012:user/not-an-instance", REGION),
            behind_the_gate: validation(),
        },
        Row {
            // The same pattern fixes the instance id at 16 characters; 15 breaks it.
            case: "CreateIdentitySource, InstanceArn's instance id is one short of 16",
            gated: Gated::IdentitySource,
            body: identity_source_body("arn:aws:sso:::instance/ssoins-1234567890abcde", REGION),
            behind_the_gate: validation(),
        },
        Row {
            // A rule the service owns: `Region` names the Region the instance is located
            // in, so it has to name one that exists.
            case: "CreateIdentitySource, Region names no Amazon Web Services Region",
            gated: Gated::IdentitySource,
            body: identity_source_body(INSTANCE_ARN, "nowhere-1"),
            behind_the_gate: validation(),
        },
        Row {
            case: "CreateIdentitySource, Tags names a key in the reserved namespace",
            gated: Gated::IdentitySource,
            body: {
                let mut body = identity_source_body(INSTANCE_ARN, REGION);
                body["Tags"] = json!({ "aws:owner": "platform" });
                body
            },
            behind_the_gate: validation(),
        },
    ]
}

/// Assert one answer is the management-account refusal, whole: the status, the code and
/// the fixed sentence.
fn assert_refused_as_not_the_management_account(case: &str, status: u16, got: &Value) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} with {got:?}"
    );
    assert_eq!(
        error_code(got),
        ACCESS_DENIED,
        "case {case:?}: expected error code {ACCESS_DENIED}, got {got:?}"
    );
    assert_eq!(
        message(got),
        MANAGEMENT_ACCOUNT_ONLY,
        "case {case:?}: expected the management-account sentence, got {:?}",
        message(got)
    );
}

/// Assert the mpa control plane is reachable from this caller AND holds no gated
/// resource: both listings answer, and both are empty.
async fn assert_nothing_was_recorded(router: &MultiServiceRouter, case: &str) {
    for gated in [Gated::ApprovalTeam, Gated::IdentitySource] {
        let (uri, member) = gated.listing();
        let (status, listed) = mpa(router, "POST", uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {member} listing answers, got {status} with {listed:?}"
        );
        let count = listed.get(member).and_then(Value::as_array).map_or(0, Vec::len);
        assert_eq!(
            count, 0,
            "case {case:?}: {member} reports {count} resource(s) after a refusal: {listed:?}"
        );
    }
}

/// How many resources a gated operation's listing reports.
async fn listed(router: &MultiServiceRouter, gated: Gated) -> usize {
    let (uri, member) = gated.listing();
    let (status, listed) = mpa(router, "POST", uri, None).await;
    assert_eq!(
        status, 200,
        "{member} listing answers, got {status} with {listed:?}"
    );
    listed.get(member).and_then(Value::as_array).map_or(0, Vec::len)
}

/// RULE: a gated mpa create is refused 403 `AccessDeniedException` with the fixed
/// management-account sentence for a caller the organization does not name as its
/// management account — whatever its body carries — and records nothing; behind the
/// organization a client establishes over the wire, each of those same requests earns its
/// own answer instead.
#[tokio::test]
async fn a_gated_mpa_create_is_refused_until_organizations_names_the_caller_its_management_account()
{
    let catalog = build_router();
    let policy_arn = published_policy_version(&catalog).await;

    for row in rows(&policy_arn) {
        let case = row.case;
        let router = build_router();

        // The emulator starts with the account in no organization, so it is the
        // management account of none: the request is refused before its body is read.
        let (status, got) = mpa(&router, "POST", row.gated.create_uri(), Some(&row.body)).await;
        assert_refused_as_not_the_management_account(case, status, &got);
        assert_nothing_was_recorded(&router, case).await;

        // The one request that establishes the fact, sent by a client that speaks only
        // HTTP.
        given_an_organization(&router).await;

        let (status, got) = mpa(&router, "POST", row.gated.create_uri(), Some(&row.body)).await;
        match row.behind_the_gate {
            BehindTheGate::Served => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to be served, got {status} with {got:?}"
                );
                assert_eq!(
                    listed(&router, row.gated).await,
                    1,
                    "case {case:?}: a served create is listed exactly once"
                );
                read_back(&router, case, row.gated, &row.body, &got).await;
            }
            BehindTheGate::Refused {
                status: want,
                code,
            } => {
                assert_eq!(
                    status, want,
                    "case {case:?}: expected HTTP {want} behind the gate, got {status} with \
                     {got:?}"
                );
                assert_eq!(
                    error_code(&got),
                    code,
                    "case {case:?}: expected error code {code} behind the gate, got {got:?}"
                );
                // The body's own rule is what answers now — the gate does not stand in
                // for it, and it did not stand in for the gate before.
                assert_ne!(
                    message(&got),
                    MANAGEMENT_ACCOUNT_ONLY,
                    "case {case:?}: the permitted caller still reads the gate's refusal"
                );
                assert_nothing_was_recorded(&router, case).await;
            }
        }
    }
}

/// A served create reads back through its own Get, out of what it stored.
async fn read_back(
    router: &MultiServiceRouter,
    case: &str,
    gated: Gated,
    sent: &Value,
    created: &Value,
) {
    match gated {
        Gated::ApprovalTeam => {
            let arn = created["Arn"].as_str().unwrap_or_default().to_string();
            assert_eq!(
                arn,
                format!(
                    "arn:aws:mpa:{REGION}:{}:approval-team/{}",
                    account(),
                    sent["Name"].as_str().unwrap_or_default()
                ),
                "case {case:?}: the create minted the wrong Arn: {created:?}"
            );
            let (status, got) = mpa(
                router,
                "GET",
                &format!("/approval-teams/{}", encode_label(&arn)),
                None,
            )
            .await;
            assert_eq!(
                (status, got["Name"].as_str()),
                (200, sent["Name"].as_str()),
                "case {case:?}: the created team reads back as {status} {got:?}"
            );
        }
        Gated::IdentitySource => {
            let arn = created["IdentitySourceArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                arn.starts_with(&identity_source_arn("")),
                "case {case:?}: expected an identity-source ARN of the caller's account, got \
                 {arn:?}"
            );
            let (status, got) = mpa(
                router,
                "GET",
                &format!("/identity-sources/{}", encode_label(&arn)),
                None,
            )
            .await;
            assert_eq!(
                (status, got["IdentitySourceArn"].as_str()),
                (200, Some(arn.as_str())),
                "case {case:?}: the created identity source reads back as {status} {got:?}"
            );
            assert_eq!(
                got["IdentitySourceParameters"]["IamIdentityCenter"]["InstanceArn"].as_str(),
                sent["IdentitySourceParameters"]["IamIdentityCenter"]["InstanceArn"].as_str(),
                "case {case:?}: the stored instance ARN reads back as {got:?}"
            );
        }
    }
}

/// Send `count` copies of `body` to `uri` AT ONCE, each from its own task.
async fn at_once(
    router: &Arc<MultiServiceRouter>,
    uri: &'static str,
    body: &Value,
    count: usize,
) -> Vec<(u16, Value)> {
    let mut pending = Vec::with_capacity(count);
    for _ in 0..count {
        let router = Arc::clone(router);
        let body = body.clone();
        pending.push(tokio::spawn(async move {
            mpa(&router, "POST", uri, Some(&body)).await
        }));
    }
    let mut answers = Vec::with_capacity(count);
    for task in pending {
        answers.push(task.await.expect("the request answered"));
    }
    answers
}

/// How many requests arrive together, naming one team.
const AT_ONCE: usize = 8;

/// RULE (the same decision, over how the call arrives): a refused gated create takes
/// nothing — the identical request repeated, the same request carrying an idempotency
/// token, and eight arriving at once are each refused the SAME 403, never a conflict and
/// never a second answer, and no `Name` is held and no idempotent answer recorded
/// afterwards. Behind the organization, the same eight at once mint exactly ONE team and
/// the rest earn the modelled `ConflictException`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_gated_create_holds_no_name_and_records_no_idempotent_answer() {
    let router = Arc::new(build_router());
    let policy_arn = published_policy_version(&router).await;
    let body = team_body("gate-repeat-team", 1, vec![approver(1)], &policy_arn);
    let with_token = {
        let mut body = team_body("gate-token-team", 1, vec![approver(1)], &policy_arn);
        body["ClientToken"] = json!("gate-repeat-token");
        body
    };

    // Repeated: the identical request reads the same refusal every time. The first call
    // took no `Name`, so the second is not told its `Name` conflicts.
    for attempt in 1..=3 {
        let (status, got) = mpa(&router, "POST", "/approval-teams", Some(&body)).await;
        assert_refused_as_not_the_management_account(
            &format!("attempt {attempt} of the identical request"),
            status,
            &got,
        );
    }

    // Carrying an idempotency token, twice: a refusal is not an answer to replay.
    for attempt in 1..=2 {
        let (status, got) = mpa(&router, "POST", "/approval-teams", Some(&with_token)).await;
        assert_refused_as_not_the_management_account(
            &format!("attempt {attempt} of the request carrying an idempotency token"),
            status,
            &got,
        );
    }

    // Eight at once, all naming the same team: every one of them is refused, so none of
    // them is the one that "wins".
    for (index, (status, got)) in at_once(&router, "/approval-teams", &body, AT_ONCE)
        .await
        .iter()
        .enumerate()
    {
        assert_refused_as_not_the_management_account(
            &format!("request {index} of {AT_ONCE} arriving at once"),
            *status,
            got,
        );
    }
    assert_nothing_was_recorded(&router, "after every refused create").await;

    // The fact a client establishes over the wire.
    given_an_organization(&router).await;

    // The same eight at once now mint exactly ONE team: a `Name` is held by one team
    // only, and the others read the modelled conflict rather than a second create.
    let answers = at_once(&router, "/approval-teams", &body, AT_ONCE).await;
    let served: Vec<&Value> = answers
        .iter()
        .filter(|(status, _)| *status == 200)
        .map(|(_, got)| got)
        .collect();
    assert_eq!(
        served.len(),
        1,
        "expected exactly one of {AT_ONCE} identical creates to be served, got {answers:?}"
    );
    for (index, (status, got)) in answers.iter().enumerate() {
        if *status == 200 {
            continue;
        }
        assert_eq!(
            (*status, error_code(got).as_str()),
            (409, "ConflictException"),
            "request {index} of {AT_ONCE}: expected the modelled conflict, got {status} {got:?}"
        );
    }
    let minted = served[0];
    read_back(&router, "the one served create", Gated::ApprovalTeam, &body, minted).await;
    assert_eq!(
        listed(&router, Gated::ApprovalTeam).await,
        1,
        "expected exactly one team stored after {AT_ONCE} identical creates, got {answers:?}"
    );

    // The token the refused calls carried recorded no answer, so it mints now — and
    // only then does its replay repeat that answer.
    let (status, created) = mpa(&router, "POST", "/approval-teams", Some(&with_token)).await;
    assert_eq!(
        status, 200,
        "expected the token refused earlier to mint a team now, got {status} {created:?}"
    );
    read_back(
        &router,
        "the request carrying an idempotency token",
        Gated::ApprovalTeam,
        &with_token,
        &created,
    )
    .await;
    let (status, replayed) = mpa(&router, "POST", "/approval-teams", Some(&with_token)).await;
    assert_eq!(
        (status, replayed["Arn"].as_str()),
        (200, created["Arn"].as_str()),
        "expected the replay to answer with the first call's Arn, got {status} {replayed:?}"
    );
    assert_eq!(
        listed(&router, Gated::ApprovalTeam).await,
        2,
        "expected the replay to mint no second team"
    );
}
