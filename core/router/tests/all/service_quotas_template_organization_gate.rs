// SPDX-License-Identifier: CC0-1.0

//! One organization, two services: the account a Service Quotas request TEMPLATE is
//! served to is the account AWS Organizations reports as managing the organization.
//!
//! RULE: a quota request template is an AWS ORGANIZATION-wide object —
//! `AssociateServiceQuotaTemplate` documents it as "Associates your quota request
//! template with your organization. When a new Amazon Web Services account is created in
//! your organization, the quota increase requests in the template are automatically
//! applied to the account" — so whether a caller may read or write it is decided by the
//! topology AWS Organizations records for that caller at the moment of the call, and by
//! nothing this service keeps: an account that belongs to no organization is refused
//! `NoAvailableOrganizationException`, an account that belongs to one some OTHER account
//! manages is refused `AccessDeniedException`, and only the account that manages an
//! all-features organization is served. The refusal comes BEFORE any template state is
//! read or written, so a refused write stores nothing.
//!
//! Every one of those refusals is a client fault, which this service's awsJson front end
//! serves as HTTP 400 with the shape name in `__type` whatever `@httpError` the shape
//! declares — the recorded wire answers it follows are cited on Service Quotas'
//! `scaffolded::wire_http_status`: a lookup naming no quota, service or request is
//! answered 400 carrying `NoSuchResourceException`, a shape the same model annotates
//! `@httpError(404)`, and the statuses this model declares across its client errors are
//! legacy error numbers rather than HTTP semantics (`@httpError(401)` for a reserved tag
//! word, `(405)` for an invalid resource state). The error CODE and the MESSAGE are
//! therefore what tell one refusal from another, and they are what every row below pins.
//!
//! Only a cross-service suite can pin this: the topology is the Organizations store's,
//! the gate is Service Quotas', and the gate reads it over the in-process callback the
//! running emulator gives a handler — so both services are mounted on ONE engine, exactly
//! as `core/router/src/lib.rs` mounts them.
//!
//! The rows span what the rule generalises over: the topology Organizations reports
//! (no organization; an organization managed by another account; one managed by an account
//! one digit from the caller's; one the caller manages with only consolidated billing;
//! one the caller manages with all features), every one of the seven template operations,
//! and — as the control — a non-template read of the same service, which the gate must
//! leave alone in every topology.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_organizations_awsorganizationsv20161128::state::{
    ComAmazonawsOrganizationsAwsorganizationsv20161128State as OrganizationsState,
    DEFAULT_FEATURE_SET, FEATURE_SET_CONSOLIDATED_BILLING,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the quota request template plane is served from (Service Quotas'
/// `state::TEMPLATE_HOME_REGION`), so no row is refused for its Region.
const HOME_REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix Service Quotas routes on (that crate's `OPERATIONS.md`).
const QUOTAS_TARGET_PREFIX: &str = "ServiceQuotasV20190624";

/// The `X-Amz-Target` prefix AWS Organizations routes on (that crate's `OPERATIONS.md`).
const ORGANIZATIONS_TARGET_PREFIX: &str = "AWSOrganizationsV20161128";

/// An adjustable quota of a service the default catalog holds, with a value above its AWS
/// default — so a `Put` that reaches the template is accepted on its own merits.
const SERVICE_CODE: &str = "vpc";
const QUOTA_CODE: &str = "L-F678F1CE";
const DESIRED_VALUE: f64 = 1_000_000.0;

/// What the gate answers a caller that is in an organization it does not manage: the
/// member-account sentence, which is entirely fixed text for that error class.
const MEMBER_ACCOUNT_MESSAGE: &str = "The request was called by a member account. To make this \
                                      request, you must use the organization's master account.";

/// The topology AWS Organizations reports for the calling account.
#[derive(Debug, Clone, Copy)]
enum Topology {
    /// The account belongs to no organization at all.
    Standalone,
    /// The account belongs to an organization this account id manages.
    ManagedBy(&'static str),
    /// The caller manages an organization created with this feature set.
    CallerManages(&'static str),
}

/// Service Quotas and AWS Organizations on one engine.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    /// Both services mounted, with the Organizations store in the topology `topology`
    /// describes.
    ///
    /// A topology the caller can reach over the wire is reached over the wire
    /// (`CreateOrganization` makes the calling account the organization's management
    /// account). An organization some OTHER account manages has no wire path — this
    /// emulator presents one account, and that account is the management account of
    /// anything it creates — so that row is written through the Organizations state
    /// module's own single organization writer, which is the state its handlers answer
    /// from.
    async fn in_topology(topology: Topology) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These callers sign nothing, so they must not be refused for it; the credential
        // scope below is what routes each request and names its Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_servicequotas_servicequotasv20190624::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        let emulator = Self { engine, router };
        match topology {
            // A freshly mounted control plane holds no organization: membership comes
            // from a request and from nothing else.
            Topology::Standalone => {}
            Topology::CallerManages(feature_set) => {
                let (status, created) = emulator
                    .organizations("CreateOrganization", &json!({ "FeatureSet": feature_set }))
                    .await;
                assert_eq!(
                    status, 200,
                    "setup CreateOrganization({feature_set}): expected 200, got {status} with \
                     {created}"
                );
            }
            Topology::ManagedBy(account_id) => {
                let state = emulator.engine.state::<OrganizationsState>();
                let mut store = state.lock();
                store.create_organization(DEFAULT_FEATURE_SET);
                store
                    .organization
                    .as_mut()
                    .expect("the organization the writer just recorded")
                    .master_account_id = account_id.to_string();
            }
        }
        emulator
    }

    /// One `awsJson1_1` call, by target prefix and signing name, at `region`.
    async fn call(
        &self,
        target_prefix: &str,
        signing_name: &str,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{region}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
                ),
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Service Quotas call at the template plane's home Region.
    async fn quotas(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.call(
            QUOTAS_TARGET_PREFIX,
            "servicequotas",
            HOME_REGION,
            operation,
            body,
        )
        .await
    }

    /// One AWS Organizations call.
    async fn organizations(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.call(
            ORGANIZATIONS_TARGET_PREFIX,
            "organizations",
            HOME_REGION,
            operation,
            body,
        )
        .await
    }

    /// Make the CALLER the management account of an all-features organization, whatever
    /// topology it was in — so a template read can be made from the one account the plane
    /// is served to, and answer what the store really holds.
    fn hand_the_organization_to_the_caller(&self) {
        let state = self.engine.state::<OrganizationsState>();
        let mut store = state.lock();
        *store = OrganizationsState::default();
        store.create_organization(DEFAULT_FEATURE_SET);
    }
}

/// The modelled error code an `awsJson1_1` error envelope carries in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `Message` an error envelope states the fact in.
fn message(body: &Value) -> String {
    body.get("Message")
        .and_then(Value::as_str)
        .unwrap_or("<no Message>")
        .to_string()
}

/// Every quota request template operation, with a request the model accepts: the three
/// association operations take no members, the entry operations take a quota and a
/// Region, and the listing takes optional filters.
fn every_template_call() -> Vec<(&'static str, Value)> {
    vec![
        ("AssociateServiceQuotaTemplate", json!({})),
        ("DisassociateServiceQuotaTemplate", json!({})),
        ("GetAssociationForServiceQuotaTemplate", json!({})),
        (
            "PutServiceQuotaIncreaseRequestIntoTemplate",
            json!({
                "ServiceCode": SERVICE_CODE,
                "QuotaCode": QUOTA_CODE,
                "AwsRegion": HOME_REGION,
                "DesiredValue": DESIRED_VALUE,
            }),
        ),
        (
            "GetServiceQuotaIncreaseRequestFromTemplate",
            json!({
                "ServiceCode": SERVICE_CODE,
                "QuotaCode": QUOTA_CODE,
                "AwsRegion": HOME_REGION,
            }),
        ),
        (
            "DeleteServiceQuotaIncreaseRequestFromTemplate",
            json!({
                "ServiceCode": SERVICE_CODE,
                "QuotaCode": QUOTA_CODE,
                "AwsRegion": HOME_REGION,
            }),
        ),
        ("ListServiceQuotaIncreaseRequestsInTemplate", json!({})),
    ]
}

/// The topologies the plane is NOT served in, with the error each one earns. Service
/// Quotas words the feature-set condition as the Service Access it denies on every
/// operation that does not model the condition itself, which is why the association
/// operation carries its own code.
fn refused_topologies() -> Vec<(&'static str, Topology, &'static str, &'static str)> {
    vec![
        (
            "the caller belongs to no organization",
            Topology::Standalone,
            "NoAvailableOrganizationException",
            "NoAvailableOrganizationException",
        ),
        (
            "another account manages the organization",
            Topology::ManagedBy("999999999999"),
            "AccessDeniedException",
            "AccessDeniedException",
        ),
        (
            "an account one digit from the caller's manages the organization",
            Topology::ManagedBy("000000000001"),
            "AccessDeniedException",
            "AccessDeniedException",
        ),
        (
            "the caller manages an organization with consolidated billing only",
            Topology::CallerManages(FEATURE_SET_CONSOLIDATED_BILLING),
            "OrganizationNotInAllFeaturesModeException",
            "AWSServiceAccessNotEnabledException",
        ),
    ]
}

/// Every template operation is refused in every topology but the management account's,
/// with the status and code that state the topology — and the member-account refusal
/// carries that error class's own fixed sentence.
#[tokio::test]
async fn the_template_plane_is_refused_in_every_topology_but_the_management_account_s() {
    for (why, topology, associate_code, other_code) in refused_topologies() {
        let emulator = Emulator::in_topology(topology).await;
        for (operation, body) in every_template_call() {
            let case = format!("{why}: {operation}");
            let expected = if operation == "AssociateServiceQuotaTemplate" {
                associate_code
            } else {
                other_code
            };
            // Every one of these is a client fault, which this awsJson front end serves
            // on HTTP 400 whatever `@httpError` the shape declares.
            let (status, answered) = emulator.quotas(operation, &body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, expected),
                "case {case:?}: expected 400 / {expected}, got {status} with {answered}"
            );
            if expected == "AccessDeniedException" {
                assert_eq!(
                    message(&answered),
                    MEMBER_ACCOUNT_MESSAGE,
                    "case {case:?}: expected the member-account sentence, got {answered}"
                );
            }
        }
    }
}

/// A refusal happens before any template state is read or written: after a refused write
/// in each topology, the account that IS the management account finds the template empty
/// and unassociated.
#[tokio::test]
async fn a_refused_template_write_stores_nothing() {
    for (why, topology, _, _) in refused_topologies() {
        let emulator = Emulator::in_topology(topology).await;
        let put = json!({
            "ServiceCode": SERVICE_CODE,
            "QuotaCode": QUOTA_CODE,
            "AwsRegion": HOME_REGION,
            "DesiredValue": DESIRED_VALUE,
        });
        let (status, refused) = emulator
            .quotas("PutServiceQuotaIncreaseRequestIntoTemplate", &put)
            .await;
        assert_ne!(
            status, 200,
            "case {why:?}: the write must be refused, got {status} with {refused}"
        );
        let (status, refused) = emulator
            .quotas("AssociateServiceQuotaTemplate", &json!({}))
            .await;
        assert_ne!(
            status, 200,
            "case {why:?}: the association must be refused, got {status} with {refused}"
        );

        // Read the template from the one account it is served to: nothing was stored.
        emulator.hand_the_organization_to_the_caller();
        let (status, listed) = emulator
            .quotas("ListServiceQuotaIncreaseRequestsInTemplate", &json!({}))
            .await;
        assert_eq!(
            status, 200,
            "case {why:?}: the management account's listing must be served, got {status} with \
             {listed}"
        );
        assert_eq!(
            listed["ServiceQuotaIncreaseRequestInTemplateList"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {why:?}: the refused write must have stored no entry, got {listed}"
        );
        let (status, association) = emulator
            .quotas("GetAssociationForServiceQuotaTemplate", &json!({}))
            .await;
        assert_eq!(
            (status, error_code(&association).as_str()),
            (400, "ServiceQuotaTemplateNotInUseException"),
            "case {why:?}: the refused association must have associated nothing, got {status} \
             with {association}"
        );
    }
}

/// The account that manages an all-features organization IS served the plane: it writes
/// an entry, reads the stored entry back, and finds it in the listing — so the gate
/// refuses a topology rather than the plane.
#[tokio::test]
async fn the_management_account_of_an_all_features_organization_is_served_the_plane() {
    let emulator = Emulator::in_topology(Topology::CallerManages(DEFAULT_FEATURE_SET)).await;
    let put = json!({
        "ServiceCode": SERVICE_CODE,
        "QuotaCode": QUOTA_CODE,
        "AwsRegion": HOME_REGION,
        "DesiredValue": DESIRED_VALUE,
    });
    let (status, written) = emulator
        .quotas("PutServiceQuotaIncreaseRequestIntoTemplate", &put)
        .await;
    assert_eq!(
        status, 200,
        "the management account's write must be served, got {status} with {written}"
    );
    let entry = &written["ServiceQuotaIncreaseRequestInTemplate"];
    assert_eq!(
        (
            entry["ServiceCode"].as_str(),
            entry["QuotaCode"].as_str(),
            entry["AwsRegion"].as_str(),
            entry["DesiredValue"].as_f64(),
        ),
        (
            Some(SERVICE_CODE),
            Some(QUOTA_CODE),
            Some(HOME_REGION),
            Some(DESIRED_VALUE)
        ),
        "the write must answer with the entry it stored, got {written}"
    );

    let (status, read) = emulator
        .quotas(
            "GetServiceQuotaIncreaseRequestFromTemplate",
            &json!({
                "ServiceCode": SERVICE_CODE,
                "QuotaCode": QUOTA_CODE,
                "AwsRegion": HOME_REGION,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the stored entry must read back, got {status} with {read}"
    );
    assert_eq!(
        read["ServiceQuotaIncreaseRequestInTemplate"], *entry,
        "the read must report the entry the write stored, got {read}"
    );

    let (status, listed) = emulator
        .quotas("ListServiceQuotaIncreaseRequestsInTemplate", &json!({}))
        .await;
    assert_eq!(
        status, 200,
        "the listing must be served, got {status} with {listed}"
    );
    assert_eq!(
        listed["ServiceQuotaIncreaseRequestInTemplateList"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        vec![entry.clone()],
        "the listing must report the stored entry, got {listed}"
    );
}

/// The gate is the TEMPLATE plane's: a non-template read of the same service answers from
/// the quota catalog in every topology, including the ones the template plane refuses.
#[tokio::test]
async fn a_non_template_read_is_served_in_every_topology() {
    let mut topologies: Vec<(&str, Topology)> = refused_topologies()
        .into_iter()
        .map(|(why, topology, _, _)| (why, topology))
        .collect();
    topologies.push((
        "the caller manages an all-features organization",
        Topology::CallerManages(DEFAULT_FEATURE_SET),
    ));

    for (why, topology) in topologies {
        let emulator = Emulator::in_topology(topology).await;
        let (status, quota) = emulator
            .quotas(
                "GetAWSDefaultServiceQuota",
                &json!({ "ServiceCode": SERVICE_CODE, "QuotaCode": QUOTA_CODE }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {why:?}: GetAWSDefaultServiceQuota must be served, got {status} with {quota}"
        );
        assert_eq!(
            quota["Quota"]["QuotaCode"].as_str(),
            Some(QUOTA_CODE),
            "case {why:?}: the default quota read must report the quota asked for, got {quota}"
        );
    }
}
