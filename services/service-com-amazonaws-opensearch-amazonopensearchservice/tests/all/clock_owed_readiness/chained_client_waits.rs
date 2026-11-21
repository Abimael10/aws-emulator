// SPDX-License-Identifier: CC0-1.0

//! What a client that CHAINS this control plane's readiness waits observes — a wait per
//! resource of a sequence, each one followed by the call it gates — driven over the real
//! wire path: a `restJson1` request through the multi-service router into the mounted
//! service, so what is asserted is what a polling client sees.
//!
//! The rule under test, one level above [`super`]'s:
//!
//! > a record the clock owes a terminal status publishes that status on the FIRST read
//! > taken once the ONE span it owes has passed — on that read, not on a later one — so a
//! > client's wait costs one look past the span whatever it is waiting for, and every
//! > operation such a wait gates is served from the state the wait converged on.
//!
//! Every look a case takes is judged by that rule rather than by a stopwatch: a look taken
//! past the span the service itself records for the record ([`super::Kind::span`], the
//! crate's own constant) MUST report the terminal status, while a look inside the span
//! reports the transient one — an implication, so the same case holds on a fast host and
//! on a loaded one, and nothing here sleeps for a span or asserts on how long one took.
//!
//! The rows span the kinds a chain is built out of, in the order a client takes them: the
//! DOMAIN, a PACKAGE, the domain-package ASSOCIATION, a service-managed VPC ENDPOINT and
//! the change to one, the dissociation that FAILS while an index reads the package and the
//! one that REMOVES it, and the quiescent `ServiceSoftwareOptions` read that compares
//! several reads of one domain. Behind those waits sit the calls a client issues once they
//! converge, each asserted here as reached THROUGH the wait: the `CreateIndex` a package's
//! presence decides, both ways round; `UpdateVpcEndpoint` on an endpoint a wait found idle;
//! and the `RollbackServiceSoftwareUpdate` a domain with no landed deployment declines.
//! What each of those answers IS, and that repeated reads of one record agree, are
//! [`super`]'s and the other siblings' own cases: this file asserts that the wait before
//! each call converges and that the call is served from what it converged on.
//!
//! Every harness, call path and read comes from the sibling suites — [`super`],
//! [`crate::domain_lifecycle`] (whose `poll_until` is the ONE wait for a condition the clock
//! owes), [`crate::package_associations`], [`crate::package_dissociation`],
//! [`crate::package_dependency_lifecycle`], [`crate::index_registry`],
//! [`crate::index_deletion`], [`crate::vpc_endpoints`], [`crate::vpc_endpoint_updates`] and
//! [`crate::service_software_rollback`] —
//! so this suite holds no second copy of any of them, and every refusal, projection and
//! table it reaches for is pinned on its own axis by those suites, this one adding only the
//! first-look-past-the-span implication and the calls reached through the wait.

use std::future::Future;
use std::slice;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    DEPLOYMENT_STATUS_COMPLETED, DOMAIN_PACKAGE_STATUS_DISSOCIATING,
    DOMAIN_PACKAGE_STATUS_DISSOCIATION_FAILED, PACKAGE_ASSOCIATION_CHANGE, PACKAGE_IN_USE,
    PACKAGE_TYPE_TXT_DICTIONARY, VPC_ENDPOINT_CHANGE, VPC_ENDPOINT_STATUS_ACTIVE,
    VPC_ENDPOINT_STATUS_UPDATING,
};

use super::{
    association_status, domain_status, endpoint_status, kinds, package_status, service_software,
};
use crate::domain_lifecycle::{create_index_serving_baseline, error_code, poll_until, Emulator};
use crate::index_deletion::deleted_index;
use crate::index_registry::{create as create_index, created_index};
use crate::package_associations::{create_package, member};
use crate::package_dependency_lifecycle::{listed, use_rows};
use crate::package_dissociation::{associate, dissociate_many, listed_status, packages_of_domain};
use crate::service_software_rollback::rollback;
use crate::vpc_endpoint_updates::{read_endpoint, update_endpoint};
use crate::vpc_endpoints::{create_endpoint, minted, vpc_domain};

/// The subnet the endpoint below is created in: the one
/// [`crate::vpc_endpoints::vpc_domain`] places the domain itself in, because an endpoint of
/// a domain is created in that domain's own VPC. A well-formed identifier of the form the
/// service that owns subnets mints, and no such service is mounted here, so nothing resolves
/// it and nothing is refused on that silence
/// (`crate::vpc_security_groups_grounded_in_ec2` pins that half, and the router's own suites
/// the half where they ARE resolvable).
const SUBNET: &str = "subnet-0123456789abcdef0";

/// The security group a change to the endpoint names — another well-formed identifier, so
/// what the case asserts is the change being APPLIED rather than any resolution of it.
const ANOTHER_GROUP: &str = "sg-0123456789abcdef1";

/// Wait for what the clock owes a record the way a client waits for it, and assert the
/// convergence rule on every look: the terminal state ends the wait, a look taken PAST
/// `span` must already report it, and a look taken inside the span reports the transient
/// status the record rests in.
///
/// `called_at` is read AFTER the call that started the work answered, so the instant the
/// clock owes the outcome is at or before `called_at + span` — which is what makes the
/// assertion an implication about one look rather than a measurement of the whole wait.
/// `terminal` is `None` where the terminal state is the record being GONE, which is what a
/// removal converges on.
///
/// The looking itself is [`crate::domain_lifecycle::poll_until`] — the crate's one wait for
/// a condition the clock owes, with its deadline and its pause between looks; what this adds
/// is the assertion each look is judged by.
async fn converges<Look, Fut>(
    case: &str,
    span: Duration,
    transient: &str,
    terminal: Option<&str>,
    called_at: Instant,
    look: Look,
) where
    Look: Fn() -> Fut,
    Fut: Future<Output = Option<String>>,
{
    let due_by = called_at + span;
    poll_until(case, || {
        // The future is created here and awaited inside: creating one starts nothing, so
        // the instant below is still read before the look is taken.
        let looking = look();
        async move {
            let looked_at = Instant::now();
            let reported = looking.await;
            if reported.as_deref() == terminal {
                return true;
            }
            assert!(
                looked_at < due_by,
                "case {case}: the look taken {:?} after the call — past the {span:?} the clock \
                 owes the record — reported {reported:?} instead of {terminal:?}, so a client's \
                 wait would have to poll again",
                looked_at.duration_since(called_at),
            );
            assert_eq!(
                reported.as_deref(),
                Some(transient),
                "case {case}: a look taken inside the span reports the transient status, got \
                 {reported:?}"
            );
            false
        }
    })
    .await;
}

/// The row of [`super::kinds`] a hop is about, found by the name a failure reports it by
/// rather than by its position in that table.
fn kind_named<'table>(table: &'table [super::Kind], case: &str) -> &'table super::Kind {
    table
        .iter()
        .find(|kind| kind.case == case)
        .unwrap_or_else(|| panic!("expected the kinds table to hold the row for {case:?}"))
}

/// The row of [`use_rows`] a case drives, found the same way: what this suite is about is
/// the WAIT a create sits behind, so it names one of the sibling table's ways a schema can
/// name a package's file rather than restating the table.
fn use_row(case: &str) -> crate::package_dependency_lifecycle::UseRow {
    use_rows()
        .into_iter()
        .find(|row| row.case == case)
        .unwrap_or_else(|| panic!("expected the package-reference table to hold {case:?}"))
}

/// RULE: every wait of a client's chain converges on one look past the span the clock owes
/// the record — the domain, the package, the association, the endpoint and the change to
/// it — and the calls those waits gate are served from the state they converged on.
#[tokio::test]
async fn a_chain_of_readiness_waits_each_converge_one_look_past_their_span() {
    let emulator = Emulator::new();
    let table = kinds();
    let domain = "chained-waits";

    // === the domain the chain starts from ===
    let kind = kind_named(&table, "a domain");
    let arn = vpc_domain(&emulator, domain).await;
    let called_at = Instant::now();
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || domain_status(&emulator, domain),
    )
    .await;

    // === the package the next call puts on it ===
    let kind = kind_named(&table, "a package");
    let package_id = create_package(
        &emulator,
        "chained-waits-dictionary",
        PACKAGE_TYPE_TXT_DICTIONARY,
    )
    .await;
    let called_at = Instant::now();
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || package_status(&emulator, &package_id),
    )
    .await;

    // === the association, issued on the first look that found the two settled ===
    let kind = kind_named(&table, "a domain-package association");
    let associated = associate(&emulator, domain, slice::from_ref(&package_id)).await;
    let called_at = Instant::now();
    assert_eq!(
        associated.status, 200,
        "case {}: expected AssociatePackages to be accepted on the settled pair, got {} with {:?}",
        kind.case, associated.status, associated.body
    );
    let answered = member(
        &associated.body["DomainPackageDetailsList"][0],
        "DomainPackageStatus",
        kind.case,
    );
    assert_eq!(
        answered, kind.transient,
        "case {}: the call answers the transient status, expected {:?}, got {answered:?}",
        kind.case, kind.transient
    );
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || association_status(&emulator, domain, &package_id),
    )
    .await;

    // === the service-managed VPC endpoint of the same domain ===
    let kind = kind_named(&table, "a VPC endpoint");
    let created = create_endpoint(
        &emulator,
        json!({ "DomainArn": arn, "VpcOptions": { "SubnetIds": [SUBNET] } }),
    )
    .await;
    let called_at = Instant::now();
    let endpoint = minted(&created, kind.case);
    let endpoint_id = member(&endpoint, "VpcEndpointId", kind.case);
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || endpoint_status(&emulator, &endpoint_id),
    )
    .await;

    // === and the change a client makes once THAT wait converged: served rather than
    // refused for a change in flight, and owed its own span ===
    let case = "a change to that endpoint";
    let updated = update_endpoint(
        &emulator,
        &endpoint_id,
        &json!({ "SecurityGroupIds": [ANOTHER_GROUP] }),
    )
    .await;
    let called_at = Instant::now();
    assert_eq!(
        updated.status, 200,
        "case {case}: expected UpdateVpcEndpoint to be served on the settled endpoint, got {} \
         with {:?}",
        updated.status, updated.body
    );
    assert_eq!(
        updated.body["VpcEndpoint"]["Status"],
        json!(VPC_ENDPOINT_STATUS_UPDATING),
        "case {case}: the endpoint is being modified the instant the call returns, got {:?}",
        updated.body
    );
    converges(
        case,
        VPC_ENDPOINT_CHANGE,
        VPC_ENDPOINT_STATUS_UPDATING,
        Some(VPC_ENDPOINT_STATUS_ACTIVE),
        called_at,
        || endpoint_status(&emulator, &endpoint_id),
    )
    .await;
    let settled = read_endpoint(&emulator, &endpoint_id).await;
    assert_eq!(
        settled["VpcOptions"]["SecurityGroupIds"],
        json!([ANOTHER_GROUP]),
        "case {case}: expected the groups the change named once it landed, got {settled:?}"
    );

    // === the last wait of the chain: the service software of a quiescent domain, which a
    // client reads before it asks for anything to be deployed. That those reads AGREE
    // across several looks is [`super`]'s own case; what this one needs of it is the
    // release the domain runs, which is what the call below is judged against ===
    let case = "the service software of a quiescent domain";
    let quiescent = service_software(&emulator, domain).await;
    assert_eq!(
        quiescent["UpdateStatus"].as_str(),
        Some(DEPLOYMENT_STATUS_COMPLETED),
        "case {case}: expected {DEPLOYMENT_STATUS_COMPLETED:?}, got {quiescent:?}"
    );
    let running = quiescent["CurrentVersion"].clone();
    assert!(
        running.as_str().is_some_and(|label| !label.is_empty()),
        "case {case}: expected the release the domain runs, got {quiescent:?}"
    );

    // === and the operation that wait gates: nothing has been deployed to this domain, so
    // the rollback is DECLINED with the modelled member rather than refused, and the
    // release the domain runs is untouched ===
    let case = "a rollback with no landed deployment to undo";
    let declined = rollback(&emulator, domain).await;
    assert_eq!(
        declined.status, 200,
        "case {case}: expected 200, got {} with {:?}",
        declined.status, declined.body
    );
    let options = declined.body["RollbackServiceSoftwareOptions"].clone();
    assert_eq!(
        (
            options["RollbackAvailable"].as_bool(),
            options["CurrentVersion"].clone(),
        ),
        (Some(false), running.clone()),
        "case {case}: expected the decline to report the release the domain runs, got {options:?}"
    );
    let after = service_software(&emulator, domain).await;
    assert_eq!(
        after, quiescent,
        "case {case}: a declined rollback deploys nothing, got {after:?}"
    );
}

/// RULE, on the chain the package checks take: each of those waits converges on one look
/// past its span too — including the dissociation that FAILS and the one that REMOVES the
/// association — and the calls between them are served from the state each wait converged
/// on: the index a package's presence decides is created exactly while the package is on
/// the domain, and refused once it is off.
#[tokio::test]
async fn the_package_calls_those_waits_gate_are_served_through_them() {
    let emulator = Emulator::new();
    let table = kinds();
    let domain = "chained-waits-packages";
    let row = use_row("a stop filter naming a file inside the package");

    // === the domain, then the package, each on its own span ===
    let kind = kind_named(&table, "a domain");
    // The index calls below are served by a domain whose engine offers the index-management
    // operations, so this one is created on that release.
    create_index_serving_baseline(&emulator, domain).await;
    let called_at = Instant::now();
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || domain_status(&emulator, domain),
    )
    .await;

    let kind = kind_named(&table, "a package");
    let package_id = create_package(
        &emulator,
        "chained-waits-stopwords",
        PACKAGE_TYPE_TXT_DICTIONARY,
    )
    .await;
    let called_at = Instant::now();
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || package_status(&emulator, &package_id),
    )
    .await;

    // === the association the index below sits behind ===
    let kind = kind_named(&table, "a domain-package association");
    let associated = associate(&emulator, domain, slice::from_ref(&package_id)).await;
    let called_at = Instant::now();
    assert_eq!(
        associated.status, 200,
        "case {}: expected AssociatePackages to be accepted, got {} with {:?}",
        kind.case, associated.status, associated.body
    );
    let details = associated.body["DomainPackageDetailsList"][0].clone();
    let path = member(&details, "ReferencePath", kind.case);
    let schema = (row.schema)(&path);

    // The redundant and the concurrent change that arrive while this one is in flight are
    // refused with the modelled `ConflictException` for the whole span, which
    // `crate::package_associations`, `crate::package_dissociation` and
    // `crate::package_dependency_lifecycle` pin on their own axis (a table of delays taken
    // from inside the span); what is asserted HERE is the wait and the calls it gates, so
    // this suite adds no case whose outcome depends on how much of that span one host has
    // left by its second round trip.

    // The change settles at exactly ONE association, in the status the clock owed it.
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || association_status(&emulator, domain, &package_id),
    )
    .await;
    let listed_entries = packages_of_domain(&emulator, domain, kind.case).await;
    let mine: Vec<&Value> = listed_entries
        .iter()
        .filter(|entry| entry["PackageID"] == json!(package_id))
        .collect();
    assert_eq!(
        mine.len(),
        1,
        "case {}: expected exactly one entry for the package, got {listed_entries:?}",
        kind.case
    );

    // === the index that association lets the domain carry ===
    created_index(&emulator, domain, row.index, &schema, row.case).await;

    // === the dissociation that index blocks: accepted, resting in its removal status, and
    // owed the FAILURE by the clock ===
    let case = "a dissociation an index of the domain blocks";
    let asked = dissociate_many(
        &emulator,
        json!({ "DomainName": domain, "PackageList": [package_id.as_str()] }),
    )
    .await;
    let called_at = Instant::now();
    assert_eq!(
        asked.status, 200,
        "case {case}: expected DissociatePackages to be accepted, got {} with {:?}",
        asked.status, asked.body
    );
    converges(
        case,
        PACKAGE_ASSOCIATION_CHANGE,
        DOMAIN_PACKAGE_STATUS_DISSOCIATING,
        Some(DOMAIN_PACKAGE_STATUS_DISSOCIATION_FAILED),
        called_at,
        || listed_status(&emulator, domain, &package_id, case),
    )
    .await;
    let entry = listed(&emulator, domain, &package_id, case)
        .await
        .unwrap_or_else(|| panic!("case {case}: expected the association to survive the failure"));
    let message = member(&entry["ErrorDetails"], "ErrorMessage", case);
    assert_eq!(
        message, PACKAGE_IN_USE,
        "case {case}: expected the service's in-use sentence, got {message:?}"
    );

    // === and the one nothing blocks, once the dependency is gone: owed the REMOVAL ===
    let case = "a dissociation nothing blocks";
    deleted_index(&emulator, domain, row.index, case).await;
    let asked = dissociate_many(
        &emulator,
        json!({ "DomainName": domain, "PackageList": [package_id.as_str()] }),
    )
    .await;
    let called_at = Instant::now();
    assert_eq!(
        asked.status, 200,
        "case {case}: expected the identical DissociatePackages to be accepted, got {} with {:?}",
        asked.status, asked.body
    );
    converges(
        case,
        PACKAGE_ASSOCIATION_CHANGE,
        DOMAIN_PACKAGE_STATUS_DISSOCIATING,
        None,
        called_at,
        || listed_status(&emulator, domain, &package_id, case),
    )
    .await;

    // === the create that wait decides: with the package off the domain the IDENTICAL
    // create is refused. WHICH refusal it is — the modelled `ValidationException` carrying
    // the engine's unreadable-file sentence, over the whole table of ways a schema names a
    // package's file, with nothing minted — is
    // `crate::package_dependency_lifecycle`'s own case; what this one asserts is that the
    // answer turns on the state the wait above converged on ===
    let case = "a create naming a file the dissociated package provided";
    let refused = create_index(
        &emulator,
        domain,
        json!({ "IndexName": row.index, "IndexSchema": schema }),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case}: expected 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case {case}: expected ValidationException, got {:?}",
        refused.body
    );

    // === ... and served again on the first look that finds the re-association in place ===
    let kind = kind_named(&table, "a domain-package association");
    let again = associate(&emulator, domain, slice::from_ref(&package_id)).await;
    let called_at = Instant::now();
    assert_eq!(
        again.status, 200,
        "case {}: expected the re-association to be accepted, got {} with {:?}",
        kind.case, again.status, again.body
    );
    converges(
        kind.case,
        kind.span,
        kind.transient,
        Some(kind.terminal),
        called_at,
        || association_status(&emulator, domain, &package_id),
    )
    .await;
    created_index(&emulator, domain, row.index, &schema, "the re-associated package").await;
}
