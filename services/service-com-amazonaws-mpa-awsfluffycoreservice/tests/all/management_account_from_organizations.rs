// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH organization this service's management-account gate is
//! decided against: the one AWS ORGANIZATIONS records.
//!
//! The rule: an operation a service restricts to an organization's management account is
//! authorized against the organization AWS Organizations holds — the service that owns
//! that fact, whose own rule is that "the account whose user is calling the
//! CreateOrganization operation automatically becomes the management account of the new
//! organization" — so whether the caller is that account is read out of Organizations at
//! the moment of the call rather than out of a copy this service keeps. A client therefore
//! reaches the permitted state over the wire alone, and an account that belongs to no
//! organization is a standalone account and stays refused.
//!
//! Both services are mounted on ONE engine, so the gate reads Organizations over the same
//! in-process callback the running emulator gives it, and every request here goes over the
//! wire a client uses: `restJson1` for this service, `awsJson1_1` for Organizations.
//!
//! The rows span the class the rule generalises over: WHO Organizations reports as the
//! management account (nobody, because the account is in no organization; another account;
//! an account one digit from the caller's; the caller itself, both as the emulator mounts
//! it and as a client creates it over the wire), and WHICH operation this service gates on
//! that account (`CreateApprovalTeam` and `CreateIdentitySource`, the creates that set an
//! organization's multi-party approval up).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_mpa_awsfluffycoreservice::EmulatedService;
use service_com_amazonaws_organizations_awsorganizationsv20161128::state::{
    ComAmazonawsOrganizationsAwsorganizationsv20161128State as OrganizationsState,
    DEFAULT_FEATURE_SET,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::approval_team_lifecycle::{encode_label, error_code, expected_arn, text, REGION};

/// The routing header AWS Organizations is reached by, from that crate's `OPERATIONS.md`:
/// `POST /` with `X-Amz-Target: AWSOrganizationsV20161128.<Op>`.
const ORGANIZATIONS_TARGET_PREFIX: &str = "AWSOrganizationsV20161128.";

/// The refusal a caller outside the organization's management account earns. The
/// requirement is the 403 and the `AccessDeniedException` code; this service states the
/// sentence in one place (`state::not_management_account`).
const MANAGEMENT_ACCOUNT_ONLY: &str =
    "Action can only be performed by an organization\u{2019}s management account.";

/// Multi-party approval and AWS Organizations, mounted on one engine — the pair the
/// running emulator mounts.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    /// Both services as the emulator mounts them: mounting the Organizations control
    /// plane establishes the organization the emulated account is the management account
    /// of, before any request is served.
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's, not either service's; these requests
        // carry a credential scope so they ROUTE, not a real signature.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Both services, with the Organizations store put back into the state of an account
    /// that belongs to NO organization.
    ///
    /// Mounting that control plane establishes the organization the emulated account
    /// belongs to, and this model declares no operation that removes one, so the
    /// in-no-organization state is reached by clearing that store after both services are
    /// mounted — the only reach into a store in this suite, and into the OTHER service's.
    fn without_organization() -> Self {
        let emulator = Self::new();
        *emulator.organizations_store().lock() = OrganizationsState::default();
        emulator
    }

    /// The AWS Organizations store, which holds the organization under test.
    fn organizations_store(&self) -> engine::ServiceState<OrganizationsState> {
        self.engine.state()
    }

    /// Make `account_id` the management account of an organization Organizations holds.
    ///
    /// A management account OTHER than the emulator's single account is not reachable over
    /// the wire — Organizations makes the CALLER the management account of what it creates
    /// — so those rows are written into that service's own record, through its own
    /// organization writer.
    fn with_management_account(account_id: &str) -> Self {
        let emulator = Self::without_organization();
        {
            let store = emulator.organizations_store();
            let mut guard = store.lock();
            guard.create_organization(DEFAULT_FEATURE_SET);
            guard
                .organization
                .as_mut()
                .expect("the organization just established")
                .master_account_id = account_id.to_string();
        }
        emulator
    }

    /// Dispatch one `restJson1` request to this service and return `(status, body)`.
    async fn mpa(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        self.dispatch(
            method,
            uri,
            "mpa",
            None,
            body.map(|value| serde_json::to_vec(&value).expect("serialize the body")),
            "application/json",
        )
        .await
    }

    /// Dispatch one `awsJson1_1` request to AWS Organizations and return `(status, body)`.
    async fn organizations(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.dispatch(
            "POST",
            "/",
            "organizations",
            Some(format!("{ORGANIZATIONS_TARGET_PREFIX}{operation}")),
            Some(serde_json::to_vec(body).expect("serialize the body")),
            "application/x-amz-json-1.1",
        )
        .await
    }

    async fn dispatch(
        &self,
        method: &str,
        uri: &str,
        signing_name: &str,
        target: Option<String>,
        body: Option<Vec<u8>>,
        content_type: &str,
    ) -> (u16, Value) {
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", content_type)
            // The credential scope is what routes the request to a service and names the
            // Region it is addressed to.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=abc"
                ),
            );
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let request = request
            .body(body.map(Bytes::from).unwrap_or_default())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// A policy reference a create can attach, taken from this service's OWN catalog over
    /// the wire: `ListPolicies` reports each policy's unqualified `Arn`, and a
    /// `PolicyReference` qualifies it with a version (`$DEFAULT`, one of the two forms
    /// `QualifiedPolicyArn`'s pattern allows).
    async fn policy_reference(&self) -> String {
        let (status, listed) = self.mpa("POST", "/policies/?List", None).await;
        assert_eq!(status, 200, "ListPolicies returned {listed:?}");
        let arn = listed
            .get("Policies")
            .and_then(Value::as_array)
            .and_then(|policies| policies.first())
            .map(|policy| text(policy, "Arn"))
            .unwrap_or_default();
        assert!(!arn.is_empty(), "ListPolicies reported no policy: {listed:?}");
        format!("{arn}/$DEFAULT")
    }
}

/// A `CreateApprovalTeam` body every modelled constraint accepts, attaching `policy_arn`.
fn team_body(name: &str, policy_arn: &str) -> Value {
    json!({
        "Name": name,
        "Description": "created by the organization's management account",
        "ApprovalStrategy": { "MofN": { "MinApprovalsRequired": 1 } },
        "Approvers": [{
            "PrimaryIdentityId": "identity-0001",
            "PrimaryIdentitySourceArn": format!(
                "arn:aws:mpa:{REGION}:{}:identity-source/ssoins-0001",
                engine::emulator_account_id()
            ),
        }],
        "Policies": [{ "PolicyArn": policy_arn }],
    })
}

/// A `CreateIdentitySource` body every modelled constraint accepts: `InstanceArn`
/// satisfies `IdcInstanceArn`'s `@pattern
/// ^arn:.+:sso:::instance/(?:sso)?ins-[a-zA-Z0-9-.]{16}$`.
fn identity_source_body() -> Value {
    json!({
        "IdentitySourceParameters": {
            "IamIdentityCenter": {
                "InstanceArn": "arn:aws:sso:::instance/ssoins-1234567890abcdef",
                "Region": REGION,
            }
        }
    })
}

/// The whole wire path the issue reports as unreachable: a client that starts in no
/// organization is refused, creates the organization through AWS Organizations' own API,
/// and is then served both gated creates — each read back through this service's own
/// reader.
#[tokio::test]
async fn a_client_that_creates_the_organization_over_the_wire_may_then_create_teams() {
    let emulator = Emulator::without_organization();
    let policy_arn = emulator.policy_reference().await;

    // Before the organization exists the caller is a standalone account, which is the
    // refusal this gate is for.
    let (status, refused) = emulator
        .mpa("POST", "/approval-teams", Some(team_body("wire-team", &policy_arn)))
        .await;
    assert_eq!(
        status, 403,
        "before an organization exists, create returned {status} with {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "AccessDeniedException",
        "wrong error code before an organization exists: {refused:?}"
    );

    // The account calling CreateOrganization becomes the organization's management
    // account — Organizations' own rule, reported in its own answer.
    let (status, created) = emulator
        .organizations("CreateOrganization", &json!({ "FeatureSet": DEFAULT_FEATURE_SET }))
        .await;
    assert_eq!(status, 200, "CreateOrganization returned {status} with {created:?}");
    let organization = created
        .get("Organization")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        text(&organization, "MasterAccountId"),
        engine::emulator_account_id(),
        "CreateOrganization made another account the management account: {created:?}"
    );

    // ... and that is the account this service's gate now permits.
    let (status, team) = emulator
        .mpa("POST", "/approval-teams", Some(team_body("wire-team", &policy_arn)))
        .await;
    assert_eq!(
        status, 200,
        "the management account's create returned {status} with {team:?}"
    );
    let team_arn = text(&team, "Arn");
    assert_eq!(
        team_arn,
        expected_arn("wire-team"),
        "create minted the wrong Arn: {team:?}"
    );
    assert_eq!(text(&team, "Name"), "wire-team", "create answered the wrong Name: {team:?}");
    assert!(
        !text(&team, "VersionId").is_empty(),
        "create answered no VersionId: {team:?}"
    );

    // The created team is the store's, not the answer's: it reads back through
    // GetApprovalTeam and is reported by ListApprovalTeams.
    let (status, read) = emulator
        .mpa(
            "GET",
            &format!("/approval-teams/{}", encode_label(&team_arn)),
            None,
        )
        .await;
    assert_eq!(status, 200, "GetApprovalTeam returned {status} with {read:?}");
    assert_eq!(
        text(&read, "Name"),
        "wire-team",
        "GetApprovalTeam reports another team: {read:?}"
    );
    let (status, listed) = emulator.mpa("POST", "/approval-teams/?List", None).await;
    assert_eq!(status, 200, "ListApprovalTeams returned {status} with {listed:?}");
    let listed_arns: Vec<String> = listed
        .get("ApprovalTeams")
        .and_then(Value::as_array)
        .map(|teams| teams.iter().map(|team| text(team, "Arn")).collect())
        .unwrap_or_default();
    assert_eq!(
        listed_arns,
        vec![team_arn.clone()],
        "ListApprovalTeams reports {listed_arns:?} after one create"
    );

    // The other gated create is served to the same caller, and reads back the same way.
    let (status, source) = emulator
        .mpa("POST", "/identity-sources", Some(identity_source_body()))
        .await;
    assert_eq!(
        status, 200,
        "the management account's CreateIdentitySource returned {status} with {source:?}"
    );
    let source_arn = text(&source, "IdentitySourceArn");
    assert!(
        !source_arn.is_empty(),
        "CreateIdentitySource answered no IdentitySourceArn: {source:?}"
    );
    let (status, read) = emulator
        .mpa(
            "GET",
            &format!("/identity-sources/{}", encode_label(&source_arn)),
            None,
        )
        .await;
    assert_eq!(status, 200, "GetIdentitySource returned {status} with {read:?}");
    assert_eq!(
        text(&read, "IdentitySourceArn"),
        source_arn,
        "GetIdentitySource reports another source: {read:?}"
    );
}

/// One row of the organization table: how Organizations answers, and whether the gate
/// serves the caller in that state.
struct OrganizationCase {
    case: &'static str,
    /// The emulator in that state.
    emulator: fn() -> Emulator,
    permitted: bool,
}

/// The table: every state of the organization record the gate reads, including the two
/// ways the caller becomes the management account of it.
fn organization_cases() -> Vec<OrganizationCase> {
    vec![
        OrganizationCase {
            case: "the account belongs to no organization",
            emulator: Emulator::without_organization,
            permitted: false,
        },
        OrganizationCase {
            case: "another account is the management account",
            emulator: || Emulator::with_management_account("999999999999"),
            permitted: false,
        },
        OrganizationCase {
            case: "an account one digit from the caller's is the management account",
            emulator: || Emulator::with_management_account("000000000001"),
            permitted: false,
        },
        OrganizationCase {
            case: "the caller is the management account, as the emulator mounts it",
            emulator: Emulator::new,
            permitted: true,
        },
        OrganizationCase {
            case: "the caller is the management account of an organization it created",
            emulator: || Emulator::with_management_account(engine::emulator_account_id()),
            permitted: true,
        },
    ]
}

/// Every operation this service gates on the management account, with the listing that
/// reports what it recorded.
struct GatedCreate {
    operation: &'static str,
    uri: &'static str,
    list_uri: &'static str,
    list_member: &'static str,
}

const GATED_CREATES: [GatedCreate; 2] = [
    GatedCreate {
        operation: "CreateApprovalTeam",
        uri: "/approval-teams",
        list_uri: "/approval-teams/?List",
        list_member: "ApprovalTeams",
    },
    GatedCreate {
        operation: "CreateIdentitySource",
        uri: "/identity-sources",
        list_uri: "/identity-sources/?List",
        list_member: "IdentitySources",
    },
];

/// The gate follows the organization Organizations records, over every state of that
/// record and every gated operation: the management account is served and nobody else
/// is, and a refused create records nothing.
#[tokio::test]
async fn the_gate_follows_the_organization_aws_organizations_records() {
    for OrganizationCase {
        case,
        emulator,
        permitted,
    } in organization_cases()
    {
        for gated in &GATED_CREATES {
            let emulator = emulator();
            let policy_arn = emulator.policy_reference().await;
            let body = if gated.operation == "CreateApprovalTeam" {
                team_body("gated-team", &policy_arn)
            } else {
                identity_source_body()
            };

            let (status, answer) = emulator.mpa("POST", gated.uri, Some(body)).await;
            let (status_listing, listed) = emulator.mpa("POST", gated.list_uri, None).await;
            assert_eq!(
                status_listing, 200,
                "case {case:?} / {}: the listing returned {status_listing} with {listed:?}",
                gated.operation
            );
            let recorded = listed
                .get(gated.list_member)
                .and_then(Value::as_array)
                .map_or(0, Vec::len);

            if permitted {
                assert_eq!(
                    status, 200,
                    "case {case:?} / {}: expected HTTP 200, got {status} with {answer:?}",
                    gated.operation
                );
                assert_eq!(
                    recorded, 1,
                    "case {case:?} / {}: the listing reports {recorded} resource(s) after a \
                     served create",
                    gated.operation
                );
                continue;
            }

            assert_eq!(
                status, 403,
                "case {case:?} / {}: expected HTTP 403, got {status} with {answer:?}",
                gated.operation
            );
            assert_eq!(
                error_code(&answer),
                "AccessDeniedException",
                "case {case:?} / {}: wrong error code in {answer:?}",
                gated.operation
            );
            assert_eq!(
                text(&answer, "Message"),
                MANAGEMENT_ACCOUNT_ONLY,
                "case {case:?} / {}: wrong refusal message in {answer:?}",
                gated.operation
            );
            assert_eq!(
                recorded, 0,
                "case {case:?} / {}: the listing reports {recorded} resource(s) after a refused \
                 create",
                gated.operation
            );
        }
    }
}

/// The gate is a READ of Organizations: a refused call and a served one both leave the
/// organization record exactly as it was, so polling the gate cannot change what a later
/// caller is told.
#[tokio::test]
async fn consulting_the_gate_leaves_the_organization_record_unchanged() {
    let emulator = Emulator::new();
    let policy_arn = emulator.policy_reference().await;

    let before = emulator
        .organizations("DescribeOrganization", &json!({}))
        .await;
    assert_eq!(before.0, 200, "DescribeOrganization returned {before:?}");

    let (status, team) = emulator
        .mpa("POST", "/approval-teams", Some(team_body("read-team", &policy_arn)))
        .await;
    assert_eq!(status, 200, "the management account's create returned {team:?}");
    // The same body again: this one is refused by the name conflict, past the gate.
    let (status, conflict) = emulator
        .mpa("POST", "/approval-teams", Some(team_body("read-team", &policy_arn)))
        .await;
    assert_eq!(
        status, 409,
        "a repeated name returned {status} with {conflict:?}"
    );

    let after = emulator
        .organizations("DescribeOrganization", &json!({}))
        .await;
    assert_eq!(
        after, before,
        "the gate changed the organization record: {before:?} became {after:?}"
    );
}
