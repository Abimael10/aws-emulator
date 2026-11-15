// SPDX-License-Identifier: CC0-1.0

//! WHICH resource a not-found refusal names when the identifier supplied carries no
//! account and no Region of its own.
//!
//! THE RULE: the operations that name the RESOURCE a `NetworkConnectorIdentifier`
//! determines — `GetNetworkConnector`, `DeleteNetworkConnector` and
//! `ListNetworkConnectorVersions` — refuse an identifier that names no connector with
//! `Network connector not found: ` followed by that resource, and a connector `Id` is an
//! ARN's resource part (`NetworkConnectorArn` is
//! `arn:<partition>:lambda:<region>:<account>:network-connector:[a-zA-Z0-9-_]+`,
//! `smithy_query(query="shape",shape="NetworkConnectorArn")`), so an id-addressed request
//! is refused naming the ARN that id determines. A bare id carries neither Region nor
//! account, and the record the handler would have read is GONE, so the remaining ARN
//! segments can only come from the REQUEST and from this emulator's own account: the Region
//! its credential scope names, and the canonical account every emulated ARN carries. A name
//! determines no ARN and an ARN is the resource already, so both are stated exactly as
//! supplied, in every Region.
//!
//! `crate::not_found_message` and `crate::removed_connector_refusal` state the same rule
//! over the identifier FORMS and the reasons a lookup finds nothing, all signed for one
//! Region — and this suite asserts the message through that suite's statement of the rule
//! (`expected_message_in`), so there is one statement of it. The rows here vary the one
//! dimension those suites hold fixed: the Region the request is signed for. An ARN assembled
//! from a fixed Region (or from a Region read off a record that no longer exists) answers
//! every single-Region row correctly and still names a resource in the wrong Region.
//!
//! Each wait POLLS for the condition it waits on rather than for a span, and nothing here
//! asserts on elapsed time.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use super::network_connector_lifecycle::{
    call_signed_for, emulator, expected_arn_in, minimal_create, text, Answer, PREFIX,
};
use super::not_found_message::{expected_message_in, names_a_version};

/// How long a poll loop keeps looking before it gives up. Generous: the condition is owed by
/// the emulator's clock, and nothing here measures how long it took.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits between two looks.
const BETWEEN_LOOKS: Duration = Duration::from_millis(25);

/// The Regions the rows sign for: AWS Regions in the one partition every emulated ARN
/// carries (`arn:aws:…`), spanning the Region-name shapes `NetworkConnectorArn`'s
/// expression admits — two-letter area codes with one- and two-word location parts.
const REGIONS: [&str; 4] = ["us-west-2", "eu-west-1", "ap-southeast-2", "us-east-1"];

/// `GetNetworkConnector` for `identifier`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, identifier: &str) -> Answer {
    call_signed_for(router, region, "GET", &format!("{PREFIX}/{identifier}"), None).await
}

/// `DeleteNetworkConnector` for `identifier`, signed for `region`.
async fn delete_in(router: &MultiServiceRouter, region: &str, identifier: &str) -> Answer {
    call_signed_for(
        router,
        region,
        "DELETE",
        &format!("{PREFIX}/{identifier}"),
        None,
    )
    .await
}

/// `ListNetworkConnectorVersions` for `identifier`, signed for `region`.
async fn list_versions_in(router: &MultiServiceRouter, region: &str, identifier: &str) -> Answer {
    call_signed_for(
        router,
        region,
        "GET",
        &format!("{PREFIX}/{identifier}/versions"),
        None,
    )
    .await
}

/// Look again until `condition` holds of the answer `GetNetworkConnector` gives for
/// `identifier` in `region`, and answer that answer. Panics with `what` when the deadline
/// passes.
async fn poll_get(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
    what: &str,
    condition: impl Fn(&Answer) -> bool,
) -> Answer {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = get_in(router, region, identifier).await;
        if condition(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {region}: {identifier:?} never {what}: last answer {} with {:?}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// Create `name` in `region`, look until it is `ACTIVE`, delete it and look until the
/// removal the clock owes has completed — "after deletion completes, subsequent calls to
/// GetNetworkConnector return ResourceNotFoundException". Answers the `Id` and the `Arn` the
/// connector had, both read off the create's own answer.
async fn created_then_removed(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) -> (String, String) {
    let created = call_signed_for(router, region, "POST", PREFIX, Some(minimal_create(name))).await;
    assert_eq!(
        created.status, 202,
        "case {region}: creating {name:?} must succeed, got {} with {:?}",
        created.status, created.body
    );
    let id = text(&created.body, "/Id");
    let arn = text(&created.body, "/Arn");
    // The create's own answer already names the connector in the Region it was signed for,
    // which is the ARN a later refusal of the same id has to name.
    assert_eq!(
        arn,
        expected_arn_in(region, &id),
        "case {region}: a created connector's Arn carries the Region the request was signed \
         for and the Id the service assigned, got {arn:?}"
    );

    poll_get(router, region, name, "reached ACTIVE", |answer| {
        answer.status == 200 && text(&answer.body, "/State") == "ACTIVE"
    })
    .await;

    let accepted = delete_in(router, region, name).await;
    assert_eq!(
        accepted.status, 202,
        "case {region}: deleting {name:?} must be accepted, got {} with {:?}",
        accepted.status, accepted.body
    );
    poll_get(router, region, name, "became absent", |answer| {
        answer.status == 404
    })
    .await;

    (id, arn)
}

/// Assert `answer` is the modelled 404 (`ResourceNotFoundException`, `@httpError 404`)
/// naming the resource `identifier` determines for a request signed for `region`.
fn assert_refused(case: &str, answer: &Answer, region: &str, identifier: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case:?}: an identifier that names no connector is refused with HTTP 404, got \
         {} with {:?}",
        answer.status, answer.body
    );
    let code = text(&answer.body, "/__type");
    assert!(
        code.ends_with("ResourceNotFoundException"),
        "case {case:?}: expected the error code ResourceNotFoundException, got {code:?}"
    );
    let expected = expected_message_in(region, identifier);
    let message = text(&answer.body, "/Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// RULE: in every Region, a removed connector's identifier is refused naming the resource
/// that identifier determines — the ARN of the signing Region for an id (version suffix
/// kept), the ARN as supplied, the name as supplied — and the delete, the read and the
/// version listing all name the same resource.
#[tokio::test]
async fn the_refusal_names_the_resource_the_identifier_determines_in_every_region() {
    for region in REGIONS {
        // One emulator per row: the connector is created and removed in the Region the row
        // signs for, so nothing is carried over from another Region's store.
        let router = emulator();
        let name = "removed-connector";
        let (id, arn) = created_then_removed(&router, region, name).await;

        // Each row: the identifier the repeat supplies. What it determines is the rule's,
        // stated once in `crate::not_found_message`.
        let rows = [
            ("its connector id", id.clone()),
            ("a version of it, by id", format!("{id}:1")),
            ("its unqualified ARN", arn.clone()),
            ("a version of it, by ARN", format!("{arn}:1")),
            ("the name it held", name.to_string()),
            ("an id that never existed", "nc-00000000000000ff".to_string()),
            ("a name that never existed", "never-held".to_string()),
        ];

        for (what, identifier) in &rows {
            let repeated = delete_in(&router, region, identifier).await;
            assert_refused(
                &format!("{region}: {what} through Delete"),
                &repeated,
                region,
                identifier,
            );
            let reported = get_in(&router, region, identifier).await;
            assert_refused(
                &format!("{region}: {what} through Get"),
                &reported,
                region,
                identifier,
            );
            // A version-qualified identifier is refused by the version listing as a bad
            // parameter before any lookup happens (one version has no versions of its own),
            // which is a different refusal and not this rule's.
            if !names_a_version(identifier) {
                let versions = list_versions_in(&router, region, identifier).await;
                assert_refused(
                    &format!("{region}: {what} through ListVersions"),
                    &versions,
                    region,
                    identifier,
                );
            }
        }
    }
}

/// RULE, the half a single-Region suite cannot see: the ARN an id-addressed refusal names is
/// assembled from the REQUEST — the Region its credential scope names — and not from a fixed
/// Region or one remembered from the connector that is gone. The same id, refused from four
/// Regions, is answered with four different ARNs, each differing from the others only in that
/// segment.
#[tokio::test]
async fn an_id_addressed_refusal_names_the_arn_of_the_region_the_request_was_signed_for() {
    let router = emulator();
    // The connector lived in, and was removed from, ONE Region.
    let home = REGIONS[0];
    let (id, arn) = created_then_removed(&router, home, "signed-elsewhere").await;

    // What the emulator ANSWERED each Region, so the distinctness below rests on its answers
    // rather than on this suite's expectations.
    let mut answered: Vec<String> = Vec::new();
    for region in REGIONS {
        let refused = delete_in(&router, region, &id).await;
        assert_refused(
            &format!("the removed id refused from {region}"),
            &refused,
            region,
            &id,
        );
        let message = text(&refused.body, "/Message");
        assert!(
            message.ends_with(&format!(":{id}")),
            "case {region}: the ARN a refusal names ends in the id the request supplied, got \
             {message:?}"
        );
        // The Region the connector was created in is the home Region's alone: a refusal
        // signed for another Region must not name it.
        assert_eq!(
            message.contains(&arn),
            region == home,
            "case {region}: the refusal names the ARN of the Region the request was signed \
             for, not {arn:?}, got {message:?}"
        );
        answered.push(message);
    }
    let distinct = {
        let mut sorted = answered.clone();
        sorted.sort();
        sorted.dedup();
        sorted.len()
    };
    assert_eq!(
        distinct,
        REGIONS.len(),
        "case one id refused from every Region: each Region is answered with its own ARN, got \
         {answered:?}"
    );

    // And the two identifier forms that carry their own Region are unchanged by the Region
    // the request is signed for: an ARN is the resource already, and a name determines none.
    for region in REGIONS {
        let by_arn = delete_in(&router, region, &arn).await;
        assert_refused(
            &format!("the removed connector's own ARN, refused from {region}"),
            &by_arn,
            region,
            &arn,
        );
        let by_name = delete_in(&router, region, "signed-elsewhere").await;
        assert_refused(
            &format!("the removed connector's name, refused from {region}"),
            &by_name,
            region,
            "signed-elsewhere",
        );
    }
}
