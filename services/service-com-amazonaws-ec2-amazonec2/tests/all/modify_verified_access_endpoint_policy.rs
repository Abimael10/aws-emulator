// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyVerifiedAccessEndpointPolicy` and the reader it writes for,
//! `GetVerifiedAccessEndpointPolicy`, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: a policy modification REPLACES, ON THE STORED ENDPOINT THE REQUEST
//! NAMES, EXACTLY THE POLICY MEMBERS THE REQUEST CARRIES — the policy document, whether that
//! document is in force, and the server-side-encryption options — leaves every member it does
//! not carry as it stands, and answers the policy the store then holds, so a later read of the
//! same endpoint reports what the modification left behind. Around that:
//!
//! 1. every shape a modification request can take applies exactly what it carries, and nothing
//!    else about the endpoint (its id, its group, its generated domain, its creation time)
//!    moves; a read reports the stored policy, twice over, because reading it changes nothing;
//! 2. a create decides whether the document it was given is in force: an endpoint created with
//!    a document reports its policy enabled, one created without reports no document and no
//!    policy in force;
//! 3. one `ClientToken` names one modification — a repeat applies nothing further, and the same
//!    token carrying a DIFFERENT request is refused rather than applied twice — and a token past
//!    the bound this service accepts is refused before anything is written;
//! 4. an endpoint id the account holds no endpoint for is refused HTTP 400
//!    `InvalidVerifiedAccessEndpointId.NotFound` by both operations, naming the id the request
//!    gave, and NOTHING is written; a `DryRun` reports the outcome the request would have had
//!    and writes nothing either.
//!
//! Each table row is a distinct input along what the rule generalises over — which members the
//! request carries, whether the endpoint already held a value for them, how long and of which
//! characters the document is, which encryption options are named, in which form the unknown id
//! is given — so no row is the specification.
//!
//! The VPC resources an endpoint names, the Verified Access instance and the group it hangs off
//! are seeded through the same insert seams the creating operations land on, the way the
//! [`super::get_verified_access_endpoint_targets`] suite seeds them; every endpoint under test
//! is created through `CreateVerifiedAccessEndpoint` itself.

use ec2::ops::types::{VerifiedAccessGroup, VerifiedAccessInstance};
use ec2::state::{
    availability_zones_in_region, ComAmazonawsEc2Amazonec2State as Ec2State, SecurityGroupSpec,
    SubnetSpec, VerifiedAccessGroupRecord, MAX_CLIENT_TOKEN_CHARS, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed, member};

/// The Region every call here is signed for.
const REGION: &str = "eu-west-1";

/// A Cedar policy that admits a group of users — the document most rows start from.
const ALLOW_POLICY: &str =
    "permit(principal, action, resource) when { context.identity.groups.contains(\"sales\") };";

/// A second, differently shaped document: a row that replaces one document with another, and
/// the request a replayed client token must be told apart from.
const DENY_POLICY: &str =
    "forbid(principal, action, resource) unless { context.http_request.method == \"GET\" };";

/// A document whose text carries the characters a form body and an XML response each have to
/// carry through: an ampersand, quotes and a multi-byte character.
const AWKWARD_POLICY: &str =
    "permit(principal, action, resource) when { context.identity.tags == \"ventes & support — équipe\" };";

/// The ARN of a customer managed key, for the rows that name encryption options. The key
/// belongs to another service, so the store keeps it as given.
const KMS_KEY_ARN: &str = "arn:aws:kms:eu-west-1:000000000000:key/8d7f6e5d-4c3b-2a19-8765-43210fedcba9";

/// A well-formed Verified Access endpoint id this store holds no endpoint for.
const UNKNOWN_ENDPOINT_ID: &str = "vae-0123456789abcdef0";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// What the seeded store holds, for the tables to name rows by.
struct Fixture {
    /// The Verified Access group every endpoint under test is attached to.
    group_id: String,
    /// The subnet the endpoint's options block names.
    subnet_id: String,
    /// The security group the endpoint is associated with.
    security_group_id: String,
}

/// Stand up an emulator and seed the resources a create references, the way the creating
/// operations would.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, Fixture) {
    let (router, engine) = super::ec2_query::emulator();
    let fixture = seed(&engine);
    (router, engine, fixture)
}

/// Write the referenced resources into the engine's store and report the ids they were given.
fn seed(engine: &engine::InMemoryEngine) -> Fixture {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();

    let vpc_id = guard.insert_vpc("10.0.0.0/16", false, "default", "available", REGION, Vec::new());
    let zone = &availability_zones_in_region(REGION)[0];
    let subnet_id = guard.insert_subnet(SubnetSpec {
        vpc_id: vpc_id.clone(),
        cidr_block: "10.0.0.0/24".to_string(),
        availability_zone: zone.zone_name.clone(),
        availability_zone_id: zone.zone_id.clone(),
        region: REGION.to_string(),
        state: "available".to_string(),
        default_for_az: false,
        map_public_ip_on_launch: false,
        tags: Vec::new(),
    });
    let security_group_id = guard.insert_security_group(SecurityGroupSpec {
        vpc_id,
        group_name: "verified-access".to_string(),
        description: "reaches the application".to_string(),
        region: REGION.to_string(),
        tags: Vec::new(),
        ip_permissions: Vec::new(),
        ip_permissions_egress: Vec::new(),
    });

    let instance_id = guard.insert_verified_access_instance(
        VerifiedAccessInstance {
            description: Some("the applications under Verified Access".to_string()),
            ..VerifiedAccessInstance::default()
        },
        None,
    );
    let group_id = guard.insert_verified_access_group(
        VerifiedAccessGroupRecord {
            view: VerifiedAccessGroup {
                verified_access_instance_id: Some(instance_id),
                description: Some("the group every endpoint here hangs off".to_string()),
                ..VerifiedAccessGroup::default()
            },
            ..VerifiedAccessGroupRecord::default()
        },
        REGION,
        None,
    );

    Fixture {
        group_id,
        subnet_id,
        security_group_id,
    }
}

/// Create one `load-balancer` endpoint on the seeded group, with `extra` beside the members
/// every create here carries, and answer the id it was minted under.
async fn create_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    fixture: &Fixture,
    extra: &[(String, String)],
) -> String {
    let mut params = vec![
        p("VerifiedAccessGroupId", fixture.group_id.clone()),
        p("EndpointType", "load-balancer"),
        p("AttachmentType", "vpc"),
        p("ApplicationDomain", "ledger.example.test"),
        p("LoadBalancerOptions.Protocol", "https"),
        p("LoadBalancerOptions.Port", "443"),
        p(
            "LoadBalancerOptions.LoadBalancerArn",
            "arn:aws:elasticloadbalancing:eu-west-1:000000000000:loadbalancer/app/web/0a1b",
        ),
    ];
    params.extend(indexed(
        "LoadBalancerOptions.SubnetId",
        &[fixture.subnet_id.as_str()],
    ));
    params.extend(indexed(
        "SecurityGroupId",
        &[fixture.security_group_id.as_str()],
    ));
    params.extend_from_slice(extra);
    let (status, body) = call(router, REGION, "CreateVerifiedAccessEndpoint", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create under test must succeed, got {status} with body {body}"
    );
    element(&body, "verifiedAccessEndpointId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the endpoint it minted, got {body}")
    })
}

/// One `ModifyVerifiedAccessEndpointPolicy` call on `endpoint_id`, with `params` beside the id.
async fn modify(
    router: &MultiServiceRouter,
    endpoint_id: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = vec![p("VerifiedAccessEndpointId", endpoint_id)];
    body.extend_from_slice(params);
    call(
        router,
        REGION,
        "ModifyVerifiedAccessEndpointPolicy",
        &body,
    )
    .await
}

/// One `GetVerifiedAccessEndpointPolicy` call, with `params` as its whole request.
async fn read(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "GetVerifiedAccessEndpointPolicy", params).await
}

/// The body `DescribeVerifiedAccessEndpoints` answers for `endpoint_id`, as its own 200.
async fn describe(router: &MultiServiceRouter, case: &str, endpoint_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeVerifiedAccessEndpoints",
        &indexed("VerifiedAccessEndpointId", &[endpoint_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeVerifiedAccessEndpoints, got {status} \
         with body {body}"
    );
    body
}

/// Poll `DescribeVerifiedAccessEndpoints` until the endpoint has SETTLED — until its status is
/// `active`, the terminal status the clock owes an endpoint a create answered `pending` — and
/// answer the body that reported it.
///
/// A test that reads `lastUpdatedTime` has to know nothing is still owed to the endpoint: that
/// move stamps it too. The wait is for the CONDITION rather than for a span of the clock, with a
/// deadline generously past [`TRANSITION_DELAY`] so a loaded machine only waits longer.
async fn settled(router: &MultiServiceRouter, case: &str, endpoint_id: &str) -> String {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let body = describe(router, case, endpoint_id).await;
        if element(&body, "code").as_deref() == Some("active") {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the endpoint must reach the status the clock owes it, last reported \
             {:?} in {body}",
            element(&body, "code")
        );
        tokio::time::sleep(TRANSITION_DELAY / 20).await;
    }
}

/// The members of a described endpoint no policy modification may touch, in one comparable
/// bundle: the service's own ids, the generated domain, the kind and the creation time.
fn untouched_facts(body: &str) -> Vec<Option<String>> {
    [
        "verifiedAccessEndpointId",
        "verifiedAccessGroupId",
        "verifiedAccessInstanceId",
        "endpointType",
        "attachmentType",
        "endpointDomain",
        "applicationDomain",
        "creationTime",
    ]
    .into_iter()
    .map(|name| element(body, name))
    .collect()
}

/// `(the policy document, whether the policy is in force)` as the response `body` reports them
/// — an absent or empty document reads as `None`, which is what an endpoint no document was
/// ever given for reports.
fn reported_policy(body: &str) -> (Option<String>, Option<String>) {
    (
        member(body, "policyDocument").filter(|document| !document.is_empty()),
        member(body, "policyEnabled"),
    )
}

/// `(the policy document, whether the policy is in force)` the STORE kept for `endpoint_id`,
/// read off the state the handlers share — `None` when the store holds no such endpoint.
fn stored_policy(
    engine: &engine::InMemoryEngine,
    endpoint_id: &str,
) -> Option<(Option<String>, bool)> {
    let state = engine.state::<Ec2State>();
    let guard = state.lock();
    guard
        .verified_access_endpoints
        .get(endpoint_id)
        .map(|record| (record.policy_document.clone(), record.policy_enabled))
}

/// Assert that `body` reports `expected_document` and `expected_enabled` as the policy.
fn assert_policy(
    case: &str,
    which: &str,
    body: &str,
    expected_document: Option<&str>,
    expected_enabled: &str,
) {
    let (document, enabled) = reported_policy(body);
    assert_eq!(
        document.as_deref(),
        expected_document,
        "case {case:?}: {which}: expected the policy document {expected_document:?}, got \
         {document:?} in {body}"
    );
    assert_eq!(
        enabled.as_deref(),
        Some(expected_enabled),
        "case {case:?}: {which}: expected policyEnabled {expected_enabled:?}, got {enabled:?} \
         in {body}"
    );
}

/// Assert that the `sseSpecification` of `body` reports `expected` as
/// `(customerManagedKeyEnabled, the key ARN)`.
///
/// The block's own element carries the response namespace, so it is found by its opening tag
/// rather than by an exact `<sseSpecification>`; its two members are the only elements of
/// those names in any body this suite reads.
fn assert_sse(case: &str, which: &str, body: &str, expected: (&str, Option<&str>)) {
    assert!(
        body.contains("<sseSpecification"),
        "case {case:?}: {which} must report the encryption options, got {body}"
    );
    let customer_managed = element(body, "customerManagedKeyEnabled");
    assert_eq!(
        customer_managed.as_deref(),
        Some(expected.0),
        "case {case:?}: {which}: expected customerManagedKeyEnabled {:?}, got \
         {customer_managed:?} in {body}",
        expected.0
    );
    let key_arn = element(body, "kmsKeyArn").filter(|arn| !arn.is_empty());
    assert_eq!(
        key_arn.as_deref(),
        expected.1,
        "case {case:?}: {which}: expected the key ARN {:?}, got {key_arn:?} in {body}",
        expected.1
    );
}

/// RULE 1: each shape a policy modification can take applies exactly the members it carries,
/// leaves the ones it does not carry as they were, answers the policy the store then holds, and
/// `GetVerifiedAccessEndpointPolicy` reports that same policy — twice over, because reading it
/// changes nothing. Nothing else about the endpoint moves.
#[tokio::test]
async fn a_policy_modification_applies_what_it_carries_and_a_read_reports_it() {
    let (router, engine, fixture) = emulator();
    // (case, what the create carried, what the modification carries, the document the endpoint
    // must then hold, whether its policy must then be in force, the encryption options it must
    // then report).
    type Row = (
        &'static str,
        Params,
        Params,
        Option<&'static str>,
        &'static str,
        (&'static str, Option<&'static str>),
    );
    let rows: Vec<Row> = vec![
        (
            "a document given to an endpoint created without one",
            Vec::new(),
            vec![p("PolicyDocument", ALLOW_POLICY)],
            Some(ALLOW_POLICY),
            "false",
            ("false", None),
        ),
        (
            "a document and the status that puts it in force",
            Vec::new(),
            vec![
                p("PolicyDocument", DENY_POLICY),
                p("PolicyEnabled", "true"),
            ],
            Some(DENY_POLICY),
            "true",
            ("false", None),
        ),
        (
            "the status alone, turning off the policy a create was given",
            vec![p("PolicyDocument", ALLOW_POLICY)],
            vec![p("PolicyEnabled", "false")],
            Some(ALLOW_POLICY),
            "false",
            ("false", None),
        ),
        (
            "one document replaced by another, the status left alone",
            vec![p("PolicyDocument", ALLOW_POLICY)],
            vec![p("PolicyDocument", DENY_POLICY)],
            Some(DENY_POLICY),
            "true",
            ("false", None),
        ),
        (
            "a document of awkward characters, on an endpoint that had none",
            Vec::new(),
            vec![
                p("PolicyDocument", AWKWARD_POLICY),
                p("PolicyEnabled", "true"),
            ],
            Some(AWKWARD_POLICY),
            "true",
            ("false", None),
        ),
        (
            "the encryption options alone, the policy left alone",
            vec![p("PolicyDocument", ALLOW_POLICY)],
            vec![
                p("SseSpecification.CustomerManagedKeyEnabled", "true"),
                p("SseSpecification.KmsKeyArn", KMS_KEY_ARN),
            ],
            Some(ALLOW_POLICY),
            "true",
            ("true", Some(KMS_KEY_ARN)),
        ),
        (
            "the encryption options turned back to the service's own key",
            vec![
                p("PolicyDocument", ALLOW_POLICY),
                p("SseSpecification.CustomerManagedKeyEnabled", "true"),
                p("SseSpecification.KmsKeyArn", KMS_KEY_ARN),
            ],
            vec![p("SseSpecification.CustomerManagedKeyEnabled", "false")],
            Some(ALLOW_POLICY),
            "true",
            ("false", None),
        ),
        (
            "a request carrying no policy member at all",
            vec![p("PolicyDocument", DENY_POLICY)],
            Vec::new(),
            Some(DENY_POLICY),
            "true",
            ("false", None),
        ),
        (
            "a request carrying no policy member on an endpoint that holds none",
            Vec::new(),
            Vec::new(),
            None,
            "false",
            ("false", None),
        ),
    ];

    for (case, created_with, modified_with, expected_document, expected_enabled, expected_sse) in
        rows
    {
        let endpoint_id = create_endpoint(&router, case, &fixture, &created_with).await;
        let before = untouched_facts(&describe(&router, case, &endpoint_id).await);

        let (status, body) = modify(&router, &endpoint_id, &modified_with).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ModifyVerifiedAccessEndpointPolicy, got \
             {status} with body {body}"
        );
        assert_policy(
            case,
            "the modification's answer",
            &body,
            expected_document,
            expected_enabled,
        );
        assert_sse(case, "the modification's answer", &body, expected_sse);

        assert_eq!(
            stored_policy(&engine, &endpoint_id),
            Some((
                expected_document.map(str::to_string),
                expected_enabled == "true"
            )),
            "case {case:?}: the store must hold what the modification applied"
        );

        for which in ["the first read", "the second read"] {
            let (status, body) = read(
                &router,
                &[p("VerifiedAccessEndpointId", endpoint_id.clone())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: {which}: expected HTTP 200 from \
                 GetVerifiedAccessEndpointPolicy, got {status} with body {body}"
            );
            assert_policy(case, which, &body, expected_document, expected_enabled);
        }

        let described = describe(&router, case, &endpoint_id).await;
        assert_eq!(
            untouched_facts(&described),
            before,
            "case {case:?}: a policy modification touches nothing else about the endpoint"
        );
        assert_sse(case, "the describe", &described, expected_sse);
    }
}

/// RULE 2: the create decides whether the document it was given is in force — one given a
/// document reports its policy enabled and that document, one given none reports neither.
#[tokio::test]
async fn a_create_decides_whether_the_policy_it_was_given_is_in_force() {
    let (router, engine, fixture) = emulator();
    // (case, what the create carried, the document the read must report, the status it must
    // report).
    let rows: Vec<(&str, Params, Option<&str>, &str)> = vec![
        (
            "created with a document",
            vec![p("PolicyDocument", ALLOW_POLICY)],
            Some(ALLOW_POLICY),
            "true",
        ),
        (
            "created with a document of awkward characters",
            vec![p("PolicyDocument", AWKWARD_POLICY)],
            Some(AWKWARD_POLICY),
            "true",
        ),
        ("created with no document", Vec::new(), None, "false"),
    ];

    for (case, created_with, expected_document, expected_enabled) in rows {
        let endpoint_id = create_endpoint(&router, case, &fixture, &created_with).await;
        let (status, body) = read(
            &router,
            &[p("VerifiedAccessEndpointId", endpoint_id.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetVerifiedAccessEndpointPolicy, got \
             {status} with body {body}"
        );
        assert_policy(
            case,
            "the read of a created endpoint",
            &body,
            expected_document,
            expected_enabled,
        );
        assert_eq!(
            stored_policy(&engine, &endpoint_id),
            Some((
                expected_document.map(str::to_string),
                expected_enabled == "true"
            )),
            "case {case:?}: the store must hold the policy the create was given"
        );
    }
}

/// RULE 3: one `ClientToken` names one modification. A repeat of the same request applies
/// nothing further — not even a second `lastUpdatedTime` — the same token carrying a different
/// request is refused rather than applied twice, and a token past the bound this service
/// accepts is refused before anything is written.
#[tokio::test]
async fn one_client_token_names_one_policy_modification() {
    let (router, engine, fixture) = emulator();
    let case = "a replayed modification";
    let endpoint_id = create_endpoint(&router, case, &fixture, &[]).await;
    let token = "e9f1c0d2-5b7a-4d3e-9f10-2a3b4c5d6e7f";
    let request = vec![
        p("PolicyDocument", ALLOW_POLICY),
        p("PolicyEnabled", "true"),
        p("ClientToken", token),
    ];

    let (status, first) = modify(&router, &endpoint_id, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first modification must succeed, got {status} with body {first}"
    );
    assert_policy(case, "the first answer", &first, Some(ALLOW_POLICY), "true");
    // Read once the endpoint has settled, so the only thing that could move its last update
    // afterwards is a write of this suite's own making.
    let stamped = element(&settled(&router, case, &endpoint_id).await, "lastUpdatedTime");

    let (status, replay) = modify(&router, &endpoint_id, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a replay of the same request must succeed, got {status} with body \
         {replay}"
    );
    assert_policy(case, "the replay's answer", &replay, Some(ALLOW_POLICY), "true");
    assert_eq!(
        element(&settled(&router, case, &endpoint_id).await, "lastUpdatedTime"),
        stamped,
        "case {case:?}: a replay applies nothing, so the endpoint's last update does not move"
    );

    let mismatch = "the same token carrying another request";
    let (status, body) = modify(
        &router,
        &endpoint_id,
        &[
            p("PolicyDocument", DENY_POLICY),
            p("PolicyEnabled", "false"),
            p("ClientToken", token),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {mismatch:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "IdempotentParameterMismatch",
        "case {mismatch:?}: expected IdempotentParameterMismatch, got {body}"
    );
    assert_eq!(
        stored_policy(&engine, &endpoint_id),
        Some((Some(ALLOW_POLICY.to_string()), true)),
        "case {mismatch:?}: a refused modification applies nothing"
    );

    let oversize = "a client token past the bound";
    let (status, body) = modify(
        &router,
        &endpoint_id,
        &[
            p("PolicyDocument", DENY_POLICY),
            p("ClientToken", "t".repeat(MAX_CLIENT_TOKEN_CHARS + 1)),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {oversize:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case {oversize:?}: expected InvalidParameterValue, got {body}"
    );
    assert_eq!(
        stored_policy(&engine, &endpoint_id),
        Some((Some(ALLOW_POLICY.to_string()), true)),
        "case {oversize:?}: a refused modification applies nothing"
    );
}

/// RULE 4: an id the account holds no endpoint for has no policy to modify or report, so both
/// operations refuse it with the endpoint's own not-found code and sentence and write nothing;
/// a `DryRun` of a request that would have succeeded is answered the permitted side of the
/// probe and writes nothing either.
#[tokio::test]
async fn an_unknown_endpoint_and_a_dry_run_are_refused_and_write_nothing() {
    let (router, engine, fixture) = emulator();
    let case = "an endpoint whose policy must not move";
    let endpoint_id = create_endpoint(
        &router,
        case,
        &fixture,
        &[p("PolicyDocument", ALLOW_POLICY)],
    )
    .await;
    let before = stored_policy(&engine, &endpoint_id);

    for presented in [
        UNKNOWN_ENDPOINT_ID,
        "vae-00000000",
        "vae-not-an-id",
        "vagr-0123456789abcdef0",
    ] {
        for (which, (status, body)) in [
            (
                "the modification",
                modify(
                    &router,
                    presented,
                    &[p("PolicyDocument", DENY_POLICY)],
                )
                .await,
            ),
            (
                "the read",
                read(&router, &[p("VerifiedAccessEndpointId", presented)]).await,
            ),
        ] {
            assert_eq!(
                status, 400,
                "case {presented:?}: {which}: expected HTTP 400 for an id naming no endpoint, \
                 got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidVerifiedAccessEndpointId.NotFound",
                "case {presented:?}: {which}: expected the endpoint's own not-found code, got \
                 {body}"
            );
            assert_eq!(
                error_message(&body),
                format!("The verified-access-endpoint ID '{presented}' does not exist"),
                "case {presented:?}: {which}: expected the refusal to name the id the request \
                 gave, got {body}"
            );
        }
    }

    for (which, (status, body)) in [
        (
            "the modification",
            modify(
                &router,
                &endpoint_id,
                &[
                    p("PolicyDocument", DENY_POLICY),
                    p("PolicyEnabled", "false"),
                    p("DryRun", "true"),
                ],
            )
            .await,
        ),
        (
            "the read",
            read(
                &router,
                &[
                    p("VerifiedAccessEndpointId", endpoint_id.clone()),
                    p("DryRun", "true"),
                ],
            )
            .await,
        ),
    ] {
        assert_eq!(
            status, 412,
            "case {case:?}: {which}: expected HTTP 412 from the dry run, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "DryRunOperation",
            "case {case:?}: {which}: expected DryRunOperation, got {body}"
        );
    }
    assert_eq!(
        stored_policy(&engine, &endpoint_id),
        before,
        "case {case:?}: neither a refusal nor a dry run writes anything"
    );
}
