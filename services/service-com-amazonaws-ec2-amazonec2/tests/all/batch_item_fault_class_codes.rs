// SPDX-License-Identifier: CC0-1.0

//! The CLASS of the fault an EC2 batch item reports, driven over the wire this service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an operation over a batch of Dedicated Hosts answers 200 and reports
//! what it could not act on per item, and that item's embedded code
//! (`UnsuccessfulItemError$Code`) carries the class of the fault — `Client.` for a cause the
//! caller's request carried — while the SAME cause reported as the call's own refusal carries
//! the bare code in `Errors/Error/Code`. A client switches on the embedded string, so a code
//! written in the wrong form is a failure it cannot recognise.
//!
//! The rows span the class: both batch operations over hosts, both kinds of value that cannot
//! be resolved to a host of the account (one outside the printed id form, one inside it that
//! was never issued) and a cause that is not about an id at all — a modification the host's own
//! instance family does not admit. Each is put beside a host the account really holds, so the
//! item is reported while the rest of the batch still runs, and beside the CALL's own refusal
//! for the same unresolvable value, which is where the bare code belongs.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{call, error_code, indexed};
use super::wire;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The instance family the hosts here are allocated for.
const FAMILY: &str = "m5";

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One host the account holds, as its id.
async fn allocate(router: &MultiServiceRouter) -> String {
    let zone = ec2::state::availability_zones_in_region(REGION)[0]
        .zone_name
        .clone();
    let params = vec![
        param("AvailabilityZone", &zone),
        param("InstanceFamily", FAMILY),
        param("Quantity", "1"),
    ];
    let (status, body) = call(router, REGION, "AllocateHosts", &params).await;
    assert_eq!(
        status, 200,
        "AllocateHosts must succeed, got {status} with body {body}"
    );
    super::ec2_query::elements(&body, "item")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("AllocateHosts must answer the host it allocated: {body}"))
}

/// The `(resourceId, code)` of every `Unsuccessful` item of a batch answer.
fn unsuccessful(case: &str, body: &str) -> Vec<(String, String)> {
    let document = wire::document(case, body);
    let root = document.root_element();
    wire::set_items(root, "unsuccessful")
        .into_iter()
        .map(|item| {
            (
                wire::child_text(item, "resourceId").unwrap_or_default(),
                wire::child(item, "error")
                    .and_then(|error| wire::child_text(error, "code"))
                    .unwrap_or_else(|| panic!("case {case:?}: an item carries its error: {body}")),
            )
        })
        .collect()
}

/// Rule: a batch item's embedded code carries the fault class; the call's own refusal does not.
#[tokio::test]
async fn a_batch_item_reports_the_client_faulted_form_of_the_code() {
    let (router, _engine) = super::ec2_query::emulator();

    // (case, the value that names no host of the account, the bare code the CALL's own
    // refusal of that value carries)
    let unresolvable: [(&str, &str, &str); 2] = [
        (
            "a value outside the printed id form",
            "not-a-host",
            "InvalidHostID.Malformed",
        ),
        (
            "an id in the printed form the account was never issued",
            "h-0123456789abcdef0",
            "InvalidHostID.Malformed",
        ),
    ];

    for (case, value, _bare) in unresolvable {
        for (action, settings) in [
            ("ReleaseHosts", Vec::new()),
            ("ModifyHosts", vec![param("AutoPlacement", "on")]),
        ] {
            let case = format!("{case} / {action}");
            // A host of its own per call: the release below gives its host back, so no row is
            // judged against a host an earlier row let go of.
            let held = allocate(&router).await;
            let mut params = indexed("HostId", &[held.as_str(), value]);
            params.extend(settings);
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a batch reports per item and is still served, got {status} \
                 with body {body}"
            );
            let reported = unsuccessful(&case, &body);
            assert_eq!(
                reported,
                vec![(value.to_string(), "Client.InvalidHostID.Malformed".to_string())],
                "case {case:?}: expected the item to carry the client-faulted code, got \
                 {reported:?} in {body}"
            );
        }
    }

    // The same cause as the CALL's own refusal carries the code BARE: a reader of
    // `Errors/Error/Code` is given the code itself, which is what separates the two forms.
    for (case, value, bare) in unresolvable {
        let (status, body) = call(
            &router,
            REGION,
            "DescribeHosts",
            &indexed("HostId", &[value]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the call itself refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, bare,
            "case {case:?}: expected the bare code {bare:?} on the call's own refusal, got \
             {code:?}"
        );
    }

    // And a cause that is not about an id: a modification the host's own instance family does
    // not admit is reported per item too, in the same faulted form.
    let case = "a modification the host's family does not admit";
    let held = allocate(&router).await;
    let (status, body) = call(
        &router,
        REGION,
        "ModifyHosts",
        &[
            param("HostId.1", &held),
            param("InstanceType", "r5.large"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let reported = unsuccessful(case, &body);
    assert_eq!(
        reported,
        vec![(
            held.clone(),
            "Client.InvalidParameterValue".to_string()
        )],
        "case {case:?}: expected the item to carry the client-faulted code, got {reported:?}"
    );
}
