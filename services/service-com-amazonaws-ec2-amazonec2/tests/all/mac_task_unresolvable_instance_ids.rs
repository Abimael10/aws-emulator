// SPDX-License-Identifier: CC0-1.0

//! THE RULE: the Mac modification task creates —
//! `CreateMacSystemIntegrityProtectionModificationTask` and
//! `CreateDelegateMacVolumeOwnershipTask` — refuse EVERY `InstanceId` that names no instance
//! of the Region the request was signed for with one code and one sentence, HTTP 400
//! `InvalidInstanceID.Malformed` / "The instance ID '<id>' is malformed", whatever made the
//! id unresolvable. These two validators draw no distinction between a value that cannot be
//! read as an instance id and one that reads as an id but names nothing, so they never
//! answer `InvalidInstanceID.NotFound` at all: what a caller branches on here is "no such
//! instance" against "this instance cannot do what you asked" — the second being
//! `InvalidInstanceType`, which only an id that DID resolve can earn.
//!
//! Why the class matters to a client: `InvalidInstanceID.NotFound` reads as an
//! eventually-consistent condition worth retrying, where `InvalidInstanceID.Malformed` reads
//! as a programming error to fail fast on. An id this account was never given is the second,
//! so a caller told the first retries a request that can never succeed, and logs a sentence
//! that says the opposite of the service's.
//!
//! The rows differ along what the rule generalises over: WHAT makes the id unresolvable —
//! a value in no published id form, a digit count outside both published forms, the all-zero
//! placeholder, the retired short form, a long form the mint has not reached (with a leading
//! digit of zero, the shape this service's own mint writes, and with one above zero), a long
//! form whose identifier the mint spent on a resource of ANOTHER kind, and the id of a real
//! instance that runs in another Region — crossed with each of the two creates, which name
//! the instance through the same member but carry different other members. Every identifier
//! derived from the store is read out of the store the handlers read rather than written
//! here, so no row can drift from the mint.
//!
//! The `tests/all/mac_modification_tasks.rs` suite holds the rest of the slice (what a task
//! records and reports); this one is about the refusal an unresolvable id earns. The same
//! rule over the operations that DO tell an unreadable id from an absent instance is
//! `tests/all/refusal_classes_and_unreadable_ids.rs`.

use ec2::state::{
    architecture_for, instance_family_of, is_mac_instance_family, offered_instance_types,
    ComAmazonawsEc2Amazonec2State as Ec2State, ARCHITECTURE_ARM64_MAC, LONG_RESOURCE_ID_DIGITS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, emulator, error_code, error_message};

/// The Region the instances of this suite run in, and the Region every call is signed for
/// unless a row says otherwise.
const HOME: &str = "us-west-2";

/// Another Region of the same partition, reached through its own endpoint: the instances of
/// [`HOME`] are invisible there.
const ELSEWHERE: &str = "us-east-1";

/// An AMI id of the long form for the launches here: the launch path does not resolve the
/// image, and nothing about a Mac modification task reads it.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The code both creates answer for an `InstanceId` that names no instance they can act on.
const MALFORMED: &str = "InvalidInstanceID.Malformed";

/// The code an instance that EXISTS but whose type cannot serve the request earns — the
/// refusal that proves the rows below turn on the id rather than on the operation.
const INVALID_INSTANCE_TYPE: &str = "InvalidInstanceType";

/// The HTTP status every refusal here carries: a fault of the request.
const REFUSAL: u16 = 400;

/// Credentials of the shape `MacCredentials` documents, for the delegation rows: the member
/// is `@required`, so a request without one is refused before the instance is resolved.
const MAC_CREDENTIALS: &str = "{\"internalDiskPassword\":\"\",\"rootVolumeUsername\":\"ec2-user\"}";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The two creates, each with the members it needs beside the `InstanceId` the row probes.
fn creates(instance_id: &str) -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        (
            "CreateMacSystemIntegrityProtectionModificationTask",
            vec![
                p("InstanceId", instance_id),
                p("MacSystemIntegrityProtectionStatus", "disabled"),
            ],
        ),
        (
            "CreateDelegateMacVolumeOwnershipTask",
            vec![
                p("InstanceId", instance_id),
                p("MacCredentials", MAC_CREDENTIALS),
            ],
        ),
    ]
}

/// `identifier` as the long-form id of the kind `prefix` names — the form
/// `ComAmazonawsEc2Amazonec2State::next_id` mints.
fn long_id(prefix: &str, identifier: u64) -> String {
    format!(
        "{prefix}-{identifier:0width$x}",
        width = LONG_RESOURCE_ID_DIGITS
    )
}

/// The highest identifier this service's shared mint has handed out, read out of the store
/// the handlers read.
fn issued_identifier(engine: &engine::InMemoryEngine) -> u64 {
    engine.state::<Ec2State>().lock().id_counter
}

/// The Apple silicon Mac instance types the model declares, in catalog order: read off the
/// modelled `InstanceType` catalog rather than written here.
fn apple_silicon_mac_types() -> Vec<&'static str> {
    offered_instance_types()
        .iter()
        .copied()
        .filter(|instance_type| {
            is_mac_instance_family(instance_family_of(instance_type))
                && architecture_for(instance_type) == ARCHITECTURE_ARM64_MAC
        })
        .collect()
}

/// Launch one instance of `instance_type` in `region` and answer its id.
async fn launch(router: &MultiServiceRouter, region: &str, instance_type: &str) -> String {
    let (status, body) = call(
        router,
        region,
        "RunInstances",
        &[
            p("ImageId", IMAGE_ID),
            p("InstanceType", instance_type),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "RunInstances for {instance_type} in {region} must succeed, got {status}: {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("RunInstances must report the instance it minted: {body}"))
}

/// Create one VPC and answer the IDENTIFIER its id carries: an identifier the mint has spent
/// on a resource of another kind, which is therefore no instance's own.
async fn identifier_spent_on_a_vpc(router: &MultiServiceRouter) -> u64 {
    let (status, body) = call(
        router,
        HOME,
        "CreateVpc",
        &[p("CidrBlock", "10.0.0.0/16")],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateVpc must succeed, got {status} with body {body}"
    );
    let vpc_id =
        element(&body, "vpcId").unwrap_or_else(|| panic!("the create must report a vpcId: {body}"));
    vpc_id
        .rsplit_once('-')
        .and_then(|(_, suffix)| u64::from_str_radix(suffix, 16).ok())
        .unwrap_or_else(|| panic!("a VPC id carries a hexadecimal identifier: {vpc_id}"))
}

/// Every id a row probes, each with what makes it unresolvable.
async fn unresolvable_ids(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
) -> Vec<(String, String)> {
    let never_issued = issued_identifier(engine) + 1_000;
    let spent_elsewhere = identifier_spent_on_a_vpc(router).await;
    vec![
        (
            "a value in no published id form".to_string(),
            "i-not-an-id".to_string(),
        ),
        (
            "a digit count outside both published forms".to_string(),
            "i-0123456789".to_string(),
        ),
        (
            "the all-zero placeholder no mint counts to".to_string(),
            "i-00000000000000000".to_string(),
        ),
        (
            "the retired short form, nothing launched under it".to_string(),
            "i-0abc1234".to_string(),
        ),
        (
            "a long form the mint has not reached, in the shape it mints".to_string(),
            long_id("i", never_issued),
        ),
        (
            "a long form whose leading digit is above zero".to_string(),
            "i-1234567890abcdef0".to_string(),
        ),
        (
            "a long form carrying an identifier the mint spent on a VPC".to_string(),
            long_id("i", spent_elsewhere),
        ),
    ]
}

/// RULE: every `InstanceId` that names no instance of the signed Region is refused
/// `InvalidInstanceID.Malformed` with the creates' own sentence, by both creates.
#[tokio::test]
async fn an_instance_id_that_resolves_to_nothing_is_malformed_on_both_mac_creates() {
    let (router, engine) = emulator();
    // One instance of the Region, so the mint has counted something and the store the rows
    // read their identifiers out of is the store the handlers reach.
    let mac_type = *apple_silicon_mac_types()
        .first()
        .expect("the model must declare an Apple silicon Mac instance type");
    let resolvable = launch(&router, HOME, mac_type).await;

    let mut rows = unresolvable_ids(&router, &engine).await;
    // An instance of ANOTHER Region names no instance here, EC2 being reached through a
    // regional endpoint, so it belongs to the same class: the id is real, and unresolvable.
    let elsewhere_instance = launch(&router, ELSEWHERE, mac_type).await;
    rows.push((
        "the id of an instance that runs in another Region".to_string(),
        elsewhere_instance,
    ));

    for (what, instance_id) in &rows {
        for (action, params) in creates(instance_id) {
            let case = format!("{what} on {action}");
            let (status, body) = call(&router, HOME, action, &params).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (REFUSAL, MALFORMED),
                "case {case:?} ({instance_id}): expected {REFUSAL} {MALFORMED}, got {status} \
                 with code {code:?} in body {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                format!("The instance ID '{instance_id}' is malformed"),
                "case {case:?}: expected the creates' own sentence for an id that resolves to \
                 nothing, got {message:?}"
            );
        }
    }

    // And the id that DOES resolve is served by both creates, so what the rows above refuse
    // is the id rather than the operation.
    for (action, params) in creates(&resolvable) {
        let (status, body) = call(&router, HOME, action, &params).await;
        assert_eq!(
            status, 200,
            "case {action:?}: the create must be served for the instance the account holds, \
             got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "instanceId").as_deref(),
            Some(resolvable.as_str()),
            "case {action:?}: the task must report the instance it was created for, got {body}"
        );
    }
}

/// RULE, the other half: only an id that RESOLVES can earn a refusal about the instance
/// itself, so an existing instance whose type runs no macOS is `InvalidInstanceType` while
/// every unresolvable id stays `InvalidInstanceID.Malformed`.
#[tokio::test]
async fn an_instance_that_exists_is_refused_for_its_type_rather_than_for_its_id() {
    let (router, engine) = emulator();
    // A type of no Mac family at all, taken from the modelled catalog.
    let non_mac = *offered_instance_types()
        .iter()
        .find(|instance_type| !is_mac_instance_family(instance_family_of(instance_type)))
        .expect("the model must declare an instance type that is not a Mac");
    let existing = launch(&router, HOME, non_mac).await;

    for (action, params) in creates(&existing) {
        let case = format!("{non_mac}, which runs no macOS, on {action}");
        let (status, body) = call(&router, HOME, action, &params).await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (REFUSAL, INVALID_INSTANCE_TYPE),
            "case {case:?}: an instance that EXISTS is refused for its type, expected \
             {REFUSAL} {INVALID_INSTANCE_TYPE}, got {status} with code {code:?} in body {body}"
        );
    }

    // The same operations, in the same store, still answer the id class for an id that
    // resolves to nothing: the two refusals are told apart by whether the id resolved.
    for (what, instance_id) in unresolvable_ids(&router, &engine).await {
        for (action, params) in creates(&instance_id) {
            let case = format!("{what} on {action}");
            let (status, body) = call(&router, HOME, action, &params).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (REFUSAL, MALFORMED),
                "case {case:?} ({instance_id}): expected {REFUSAL} {MALFORMED}, got {status} \
                 with code {code:?} in body {body}"
            );
        }
    }
}

/// RULE under `DryRun`: the flag reports whether the request WOULD have worked, so a request
/// that would have been refused for its `InstanceId` is refused with that reason instead of
/// `DryRunOperation`, in the same class and the same sentence.
#[tokio::test]
async fn a_dry_run_of_an_unresolvable_id_is_refused_for_the_id() {
    let (router, engine) = emulator();
    let mac_type = *apple_silicon_mac_types()
        .first()
        .expect("the model must declare an Apple silicon Mac instance type");
    let resolvable = launch(&router, HOME, mac_type).await;

    for (what, instance_id) in unresolvable_ids(&router, &engine).await {
        for (action, mut params) in creates(&instance_id) {
            params.push(p("DryRun", "true"));
            let case = format!("{what} on {action} under DryRun");
            let (status, body) = call(&router, HOME, action, &params).await;
            let code = error_code(&body);
            assert_eq!(
                (status, code.as_str()),
                (REFUSAL, MALFORMED),
                "case {case:?} ({instance_id}): expected {REFUSAL} {MALFORMED}, got {status} \
                 with code {code:?} in body {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message,
                format!("The instance ID '{instance_id}' is malformed"),
                "case {case:?}: expected the creates' own sentence, got {message:?}"
            );
        }
    }

    // A dry run whose id DOES resolve reports that the request would have worked.
    for (action, mut params) in creates(&resolvable) {
        params.push(p("DryRun", "true"));
        let (status, body) = call(&router, HOME, action, &params).await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (412, "DryRunOperation"),
            "case {action:?}: a dry run for the instance the account holds reports the \
             request would have worked, got {status} with code {code:?} in body {body}"
        );
    }

    // And nothing was stored by any of it.
    let (status, body) = call(&router, HOME, "DescribeMacModificationTasks", &[]).await;
    assert_eq!(
        status, 200,
        "the describe must be served, got {status} with body {body}"
    );
    assert!(
        super::ec2_query::elements(&body, "macModificationTaskId").is_empty(),
        "a refused create and a dry run must store no task, got {body}"
    );
}
