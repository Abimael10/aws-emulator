// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for two facts about a refused VPC link `Name` that the sibling
//! [`super::vpc_link_name_refusal_wording`] does not read: what the length window COUNTS, and
//! what the collection reader reports after the refusal.
//!
//! Every test drives the real `restJson1` request path — `MultiServiceRouter::dispatch` over a
//! signed `http::Request` — so what is asserted is the wire behaviour a client sees, not a
//! handler called directly.
//!
//! The rules these cases assert:
//!
//! * **The documented window is a string LENGTH, so a multi-byte character counts once**:
//!   `Name` targets `StringWithLengthBetween1And128`, documented as "A string with a length
//!   between [1-128]", so a name of 128 characters is accepted however many bytes it encodes
//!   to, and one of 129 characters is refused with this service's member/length-window
//!   sentence naming 129 — the count of characters, not of bytes.
//! * **A refused write is invisible to `GetVpcLinks`**: the collection reader computes its
//!   answer from the links the account holds, so after a refusal it reports exactly what it
//!   reported before it, link for link and member for member, while an accepted create adds
//!   exactly the one link it minted.
//!
//! The rows vary what the rules generalise over: the script and the bytes-per-character of the
//! name (ASCII, two-byte, three-byte and four-byte characters), its length at and one past the
//! published maximum, which writer carries it, and which refusal class the collection is read
//! after.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewayv2_apigatewayv2::state::{
    MAX_VPC_LINK_NAME_LEN, VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
};

use crate::api_lifecycle::{as_str, call, emulator};
use crate::vpc_link_name_refusal_wording::{assert_bad_request, length_window_message};

/// The VPC-links collection URI (`CreateVpcLink`'s and `GetVpcLinks`' modelled
/// `/v2/vpclinks`); one link hangs off it as `{VPC_LINKS}/{id}`, the URI `GetVpcLink` and
/// `UpdateVpcLink` share.
const VPC_LINKS: &str = "/v2/vpclinks";

/// The members of a listed link the CLOCK does not move, so two listings can be compared
/// member for member without racing the provisioning a create started. `VpcLinkStatus` and
/// `VpcLinkStatusMessage` are left out for exactly that reason; the sibling
/// `vpc_link_status_messages` suite owns them.
const STABLE_MEMBERS: [&str; 7] = [
    "vpcLinkId",
    "name",
    "subnetIds",
    "securityGroupIds",
    "tags",
    "createdDate",
    "vpcLinkVersion",
];

/// One name to send, built from a character and a length so a row says which script it is in
/// and how many BYTES that length encodes to.
struct Name {
    case: &'static str,
    character: char,
    characters: usize,
}

impl Name {
    /// The name itself.
    fn value(&self) -> String {
        std::iter::repeat_n(self.character, self.characters).collect()
    }

    /// How many bytes it encodes to, for a report that says why the row is interesting.
    fn bytes(&self) -> usize {
        self.value().len()
    }
}

/// The rows: each script at the documented maximum and one character past it. The characters
/// differ in how many bytes they encode to — one, two, three and four — so a window read off
/// the byte count would accept and refuse different rows than one read off the character
/// count.
fn names() -> Vec<Name> {
    let mut rows = Vec::new();
    for (case, character) in [
        ("an ASCII name", 'n'),
        ("a two-byte name", 'ü'),
        ("a three-byte name", '決'),
        ("a four-byte name", '🎉'),
    ] {
        rows.push(Name {
            case,
            character,
            characters: MAX_VPC_LINK_NAME_LEN,
        });
        rows.push(Name {
            case,
            character,
            characters: MAX_VPC_LINK_NAME_LEN + 1,
        });
    }
    rows
}

/// The links the account holds, as `GetVpcLinks` reports them, each cut down to the members
/// the clock does not move and ordered by id so two listings compare directly.
async fn listing(emulator: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, body) = call(emulator, "GET", VPC_LINKS, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetVpcLinks, got {status} {body}"
    );
    let mut links: Vec<Value> = body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            let mut stable = serde_json::Map::new();
            for member in STABLE_MEMBERS {
                if let Some(value) = item.get(member) {
                    stable.insert(member.to_string(), value.clone());
                }
            }
            Value::Object(stable)
        })
        .collect();
    links.sort_by_key(|link| as_str(link, "vpcLinkId"));
    links
}

/// RULE: the documented window counts characters, so a name of 128 characters is accepted
/// whatever it encodes to and one of 129 is refused with the window sentence naming 129.
#[tokio::test]
async fn the_documented_window_counts_characters_and_not_bytes() {
    let emulator = emulator();

    for row in names() {
        let name = row.value();
        let case = format!(
            "{} of {} character(s) in {} byte(s)",
            row.case,
            row.characters,
            row.bytes()
        );
        let create = json!({ "name": name, "subnetIds": ["subnet-0a1b2c3d"] });
        let (status, body) = call(&emulator, "POST", VPC_LINKS, Some(create)).await;

        if row.characters > MAX_VPC_LINK_NAME_LEN {
            assert_bad_request(
                &format!("CreateVpcLink, {case}"),
                status,
                &body,
                &length_window_message("Name", row.characters),
            );
            continue;
        }

        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateVpcLink, got {status} {body}"
        );
        let vpc_link_id = as_str(&body, "vpcLinkId");
        let uri = format!("{VPC_LINKS}/{vpc_link_id}");

        // The accepted name is STORED, not echoed: the single-link reader answers it back.
        let (status, fetched) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetVpcLink, got {status} {fetched}"
        );
        assert_eq!(
            as_str(&fetched, "name"),
            name,
            "case {case:?}: expected the stored Name to be the one sent, got {fetched}"
        );

        // The patch that renames a link reads the member off the same shape, so the same
        // length is accepted there and the one past the maximum is refused there.
        let (status, patched) = call(&emulator, "PATCH", &uri, Some(json!({ "name": name }))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from UpdateVpcLink, got {status} {patched}"
        );
        let past_the_window: String =
            std::iter::repeat_n(row.character, row.characters + 1).collect();
        let (status, refused) = call(
            &emulator,
            "PATCH",
            &uri,
            Some(json!({ "name": past_the_window })),
        )
        .await;
        assert_bad_request(
            &format!("UpdateVpcLink one character past the maximum, {case}"),
            status,
            &refused,
            &length_window_message("Name", row.characters + 1),
        );
    }
}

/// RULE: `GetVpcLinks` answers from the links the account holds, so a refused write leaves its
/// answer exactly as it was and an accepted create adds exactly the link it minted.
#[tokio::test]
async fn a_refused_write_is_invisible_to_the_collection_reader() {
    let emulator = emulator();

    // One link already exists, so an unchanged listing is a listing of something rather than
    // of nothing.
    let (status, created) = call(
        &emulator,
        "POST",
        VPC_LINKS,
        Some(json!({
            "name": "the-one-link",
            "subnetIds": ["subnet-0a1b2c3d", "subnet-00112233445566778"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "tags": { "owner": "api-team" }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateVpcLink, got {status} {created}"
    );
    let uri = format!("{VPC_LINKS}/{}", as_str(&created, "vpcLinkId"));

    let over_long = "n".repeat(MAX_VPC_LINK_NAME_LEN + 1);
    let window = length_window_message("Name", MAX_VPC_LINK_NAME_LEN + 1);
    // The rows differ in the refusal class, in which writer is refused, and in whether the
    // request addresses a link the account holds.
    let refusals: [(&str, &str, &str, Value, &str); 4] = [
        (
            "a create whose name carries nothing",
            "POST",
            VPC_LINKS,
            json!({ "name": "", "subnetIds": ["subnet-0a1b2c3d"] }),
            VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
        ),
        (
            "a create whose name is one character past the maximum",
            "POST",
            VPC_LINKS,
            json!({ "name": over_long, "subnetIds": ["subnet-0a1b2c3d"] }),
            &window,
        ),
        (
            "a patch whose name carries nothing",
            "PATCH",
            &uri,
            json!({ "name": "" }),
            VPC_LINK_NAME_NULL_OR_EMPTY_MESSAGE,
        ),
        (
            "a patch whose name is one character past the maximum",
            "PATCH",
            &uri,
            json!({ "name": over_long }),
            &window,
        ),
    ];

    for (case, method, target, request, expected) in refusals {
        let before = listing(&emulator, case).await;
        let (status, body) = call(&emulator, method, target, Some(request)).await;
        assert_bad_request(case, status, &body, expected);
        let after = listing(&emulator, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused write to leave the collection as it was, got \
             {after:#?} against {before:#?}"
        );
    }

    // ... and an accepted create adds exactly the one link it minted, which the reader
    // reports with the members the create answered.
    let before = listing(&emulator, "an accepted create").await;
    let (status, minted) = call(
        &emulator,
        "POST",
        VPC_LINKS,
        Some(json!({ "name": "accepted", "subnetIds": ["subnet-0a1b2c3d"] })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateVpcLink, got {status} {minted}"
    );
    let after = listing(&emulator, "an accepted create").await;
    assert_eq!(
        after.len(),
        before.len() + 1,
        "case \"an accepted create\": expected one link more, got {after:#?} against {before:#?}"
    );
    let minted_id = as_str(&minted, "vpcLinkId");
    let listed = after
        .iter()
        .find(|link| as_str(link, "vpcLinkId") == minted_id)
        .unwrap_or_else(|| panic!("expected the minted link {minted_id:?} in {after:#?}"));
    for member in STABLE_MEMBERS {
        assert!(
            listed.get(member).is_some(),
            "case \"an accepted create\": expected the listing to carry the {member} of the \
             link it reports, got {listed:#?}"
        );
        assert_eq!(
            listed.get(member),
            minted.get(member),
            "case \"an accepted create\": expected the listing to report the {member} the \
             create answered, got {listed:#?}"
        );
    }
}
