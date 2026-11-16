// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for HOW a VPC link `Name` that carries nothing is WORDED when it
//! is refused.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch`
//! over a signed `http::Request` — so what is asserted is the wire behaviour a client
//! sees, not a handler called directly.
//!
//! The rules these cases assert:
//!
//! * **A VPC link `Name` supplied with nothing is refused in API Gateway's own words for
//!   that refusal class**: 400 `BadRequestException` carrying
//!   [`VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE`], fixed text that names neither the member,
//!   nor the documented length window, nor the length observed — from every writer of the
//!   member, the create that mints a link as much as the patch that renames one.
//! * **A `Name` that IS present but longer than the documented maximum keeps this
//!   service's member/length-window sentence**, which does name the member, the window and
//!   the length the request carried; the dedicated sentence is the empty case only, and
//!   another member's empty value (an api's `Name`, the same
//!   `StringWithLengthBetween1And128`) is still worded as a window violation.
//! * **A refused patch writes nothing and does not spend the link**: the link keeps the
//!   `Name` and `SecurityGroupIds` it had, and a valid rename straight after the refusal
//!   still applies — both read back through `GetVpcLink`.
//!
//! The rows vary what the rules generalise over: which writer carries the name, the shape
//! of the link it is sent to (how many subnets, security groups and tags it holds, and
//! whether an earlier patch already renamed it), and how far past the published maximum an
//! over-long name reaches. The recorded `UpdateVpcLink(Name="")` that filed the issue is
//! ONE row of the first table.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    API_NAME_LEN, MAX_VPC_LINK_NAME_LEN, VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
};

use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// The VPC-links collection URI (`CreateVpcLink`'s modelled `POST /v2/vpclinks`); one link
/// hangs off it as `{VPC_LINKS}/{id}`, the URI `GetVpcLink` and `UpdateVpcLink` share.
const VPC_LINKS: &str = "/v2/vpclinks";

/// The apis collection URI, for the row that checks the dedicated sentence did not spread
/// to another member that targets the same shape.
const APIS: &str = "/v2/apis";

/// This service's sentence for a member whose value falls outside the length window the
/// shape it targets documents: it names the member, the window and the length the request
/// carried.
fn length_window_message(member: &str, got: usize) -> String {
    let (min, max) = API_NAME_LEN;
    format!("Invalid value for {member}: must have length between {min} and {max}, got {got}.")
}

/// One link the empty name is sent to, named by the case it stands for.
struct Link {
    case: &'static str,
    create: Value,
    /// A rename applied BEFORE the refusal under test, so a name an earlier patch stored
    /// is covered too.
    earlier_patch: Option<Value>,
}

fn links() -> Vec<Link> {
    vec![
        Link {
            case: "a link created with only the required members",
            create: json!({ "name": "minimal", "subnetIds": ["subnet-0a1b2c3d"] }),
            earlier_patch: None,
        },
        Link {
            case: "a link holding several subnets, security groups and tags",
            create: json!({
                "name": "multi-az",
                "subnetIds": [
                    "subnet-0a1b2c3d",
                    "subnet-00112233445566778",
                    "subnet-ffffffffffffffff0"
                ],
                "securityGroupIds": ["sg-0a1b2c3d", "sg-00112233445566778"],
                "tags": { "team": "platform", "env": "test" }
            }),
            earlier_patch: None,
        },
        Link {
            case: "a link an earlier patch already renamed",
            create: json!({
                "name": "first",
                "subnetIds": ["subnet-0123456789abcdef0"],
                "securityGroupIds": ["sg-0123456789abcdef0"]
            }),
            earlier_patch: Some(json!({ "name": "renamed once" })),
        },
        Link {
            case: "a link whose name is outside ASCII",
            create: json!({
                "name": "決済-VPC-Link — ünïcödé 🎉",
                "subnetIds": ["subnet-0a1b2c3d"]
            }),
            earlier_patch: None,
        },
    ]
}

/// The same create request with `Name` replaced by `name`.
fn named(create: &Value, name: &str) -> Value {
    let mut request = create.clone();
    request["name"] = json!(name);
    request
}

/// Assert one answer is the modelled `BadRequestException` carrying exactly `expected`.
fn assert_bad_request(case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status} {body}"
    );
    assert_eq!(
        error_code(body),
        "BadRequestException",
        "case {case:?}: expected BadRequestException, got {body}"
    );
    assert_eq!(
        as_str(body, "message"),
        expected,
        "case {case:?}: expected the refusal {expected:?}, got {body}"
    );
}

/// Create one VPC link and answer its minted `VpcLinkId`.
async fn create_link(emulator: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, created) = call(emulator, "POST", VPC_LINKS, Some(request.clone())).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateVpcLink, got {status} {created}"
    );
    as_str(&created, "vpcLinkId")
}

/// Read one VPC link back, asserting the read succeeded.
async fn get_link(emulator: &MultiServiceRouter, case: &str, vpc_link_id: &str) -> Value {
    let (status, fetched) = call(
        emulator,
        "GET",
        &format!("{VPC_LINKS}/{vpc_link_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetVpcLink, got {status} {fetched}"
    );
    fetched
}

/// RULE: a VPC link `Name` the request supplies as empty is refused 400
/// `BadRequestException` with API Gateway's own sentence for a VPC link name carrying
/// nothing — by the patch that renames a link and by the create that mints one.
#[tokio::test]
async fn a_vpc_link_name_carrying_nothing_is_refused_in_api_gateways_own_words() {
    let emulator = emulator();

    for link in links() {
        let case = link.case;

        // The create that mints a link reads the member off the same shape, so the empty
        // name it carries takes the same refusal.
        let (status, refused) =
            call(&emulator, "POST", VPC_LINKS, Some(named(&link.create, ""))).await;
        assert_bad_request(
            &format!("CreateVpcLink, {case}"),
            status,
            &refused,
            VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
        );

        let vpc_link_id = create_link(&emulator, case, &link.create).await;
        let uri = format!("{VPC_LINKS}/{vpc_link_id}");
        if let Some(earlier) = link.earlier_patch {
            let (status, patched) = call(&emulator, "PATCH", &uri, Some(earlier)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 from the earlier UpdateVpcLink, got {status} \
                 {patched}"
            );
        }

        let (status, refused) = call(&emulator, "PATCH", &uri, Some(json!({ "name": "" }))).await;
        assert_bad_request(
            &format!("UpdateVpcLink, {case}"),
            status,
            &refused,
            VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
        );
    }
}

/// RULE: the dedicated sentence is the EMPTY case of the refusal — a name that is present
/// but longer than the documented maximum, and another member supplied empty, both keep
/// the member/length-window sentence this service words its own constraint rejections with.
#[tokio::test]
async fn a_name_past_the_documented_maximum_keeps_the_member_and_length_sentence() {
    let emulator = emulator();
    let case = "a VPC link whose patched Name runs past the documented maximum";
    let vpc_link_id = create_link(
        &emulator,
        case,
        &json!({ "name": "valid", "subnetIds": ["subnet-0a1b2c3d"] }),
    )
    .await;
    let uri = format!("{VPC_LINKS}/{vpc_link_id}");

    // One character past the published bound, and far past it: every over-long name is
    // worded as the window violation it is.
    for extra in [1usize, 2, 97] {
        let length = MAX_VPC_LINK_NAME_LEN + extra;
        let name = "n".repeat(length);
        let expected = length_window_message("Name", length);

        let (status, refused) = call(&emulator, "PATCH", &uri, Some(json!({ "name": name }))).await;
        assert_bad_request(
            &format!("UpdateVpcLink with a {length}-character Name"),
            status,
            &refused,
            &expected,
        );

        let (status, refused) = call(
            &emulator,
            "POST",
            VPC_LINKS,
            Some(json!({ "name": "n".repeat(length), "subnetIds": ["subnet-0a1b2c3d"] })),
        )
        .await;
        assert_bad_request(
            &format!("CreateVpcLink with a {length}-character Name"),
            status,
            &refused,
            &expected,
        );
    }

    // A different member that targets the same `StringWithLengthBetween1And128` is NOT a
    // VPC link's name, so an empty value for it is still the window violation.
    let (status, refused) = call(
        &emulator,
        "POST",
        APIS,
        Some(json!({ "name": "", "protocolType": "HTTP" })),
    )
    .await;
    assert_bad_request(
        "CreateApi with an empty Name",
        status,
        &refused,
        &length_window_message("Name", 0),
    );
}

/// RULE: the refusal is answered BEFORE anything is written, so the link keeps the `Name`
/// and `SecurityGroupIds` it had and a valid rename straight after it still applies.
#[tokio::test]
async fn a_refused_patch_leaves_the_link_alone_and_a_later_rename_still_applies() {
    let emulator = emulator();

    for link in links() {
        let case = link.case;
        let vpc_link_id = create_link(&emulator, case, &link.create).await;
        let uri = format!("{VPC_LINKS}/{vpc_link_id}");
        let before = get_link(&emulator, case, &vpc_link_id).await;

        let (status, refused) = call(&emulator, "PATCH", &uri, Some(json!({ "name": "" }))).await;
        assert_bad_request(
            &format!("UpdateVpcLink, {case}"),
            status,
            &refused,
            VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
        );

        let after = get_link(&emulator, case, &vpc_link_id).await;
        assert_eq!(
            as_str(&after, "name"),
            as_str(&before, "name"),
            "case {case:?}: expected the refused patch to leave the stored Name, got {after}"
        );
        assert_eq!(
            after.get("securityGroupIds"),
            before.get("securityGroupIds"),
            "case {case:?}: expected the refused patch to leave the stored SecurityGroupIds, \
             got {after}"
        );

        let renamed = "accepted-after-the-refusal";
        let (status, patched) =
            call(&emulator, "PATCH", &uri, Some(json!({ "name": renamed }))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the rename after the refusal, got {status} \
             {patched}"
        );
        assert_eq!(
            as_str(&patched, "name"),
            renamed,
            "case {case:?}: expected UpdateVpcLink to report Name {renamed:?}, got {patched}"
        );
        let read_back = get_link(&emulator, case, &vpc_link_id).await;
        assert_eq!(
            as_str(&read_back, "name"),
            renamed,
            "case {case:?}: expected GetVpcLink to report Name {renamed:?}, got {read_back}"
        );
        assert_eq!(
            read_back.get("securityGroupIds"),
            before.get("securityGroupIds"),
            "case {case:?}: expected the rename to leave the stored SecurityGroupIds, got \
             {read_back}"
        );
    }
}
