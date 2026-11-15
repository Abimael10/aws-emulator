// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a REFUSED `CreateStack` leaves behind.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! RULE: a create the service refuses on the request's own grounds creates nothing, so it
//! does not reserve the `Name` either — the name stays one this account holds no stack
//! under, and the corrected create of that SAME name is an ordinary first create that
//! succeeds and is reported back in full. This is the create-retry loop every client
//! performs: submit, read the refusal, correct the request, resubmit under the same name.
//!
//! The rows differ along what the rule generalises over: WHICH ground the first attempt is
//! refused on — a list naming one endpoint type twice, an entry that references no
//! endpoint, a collection outside its modelled length, a string outside its modelled
//! length — and therefore which refusal class the client reads before it retries. The
//! control is that the retry is a full first create: it carries the configuration the
//! service settles itself and reads back identically.
//!
//! No owner of VPC endpoints is mounted on this suite's engine, so a refusal here is the
//! request's own shape and nothing else (`tests/all/stack_access_endpoints.rs` drives the
//! hop that resolves a referenced endpoint).

use serde_json::{json, Value};

use crate::refusal_messages::refusal;
use crate::stack_fleet_lifecycle::Emulator;

/// The status every refusal of this service leaves with, the not-found one included.
const REFUSAL_STATUS: u16 = 400;

/// The only `AccessEndpointType` the model declares
/// (`smithy_query(query="shape", shape="AccessEndpointType")`).
const STREAMING: &str = "STREAMING";

/// A well-formed endpoint id; nothing resolves it in this suite.
const SOME_ENDPOINT_ID: &str = "vpce-0a1b2c3d4e5f60718";

/// One attempt the service must refuse, and the wire code it is refused with.
struct RefusedCreate {
    case: &'static str,
    /// The create members the attempt carries BESIDE `Name`.
    members: Value,
    code: &'static str,
}

/// Refused first attempts spanning the class: an incorrect combination of parameters, an
/// absent member a list entry needs, a list below its modelled minimum, a list above its
/// modelled maximum, and a string past its modelled maximum length.
///
/// The bounds come from `smithy_query(query="operation", operation="CreateStack")`:
/// `AccessEndpoints` is `@length min 1 max 4`, `UserSettings` `@length min 1`,
/// `EmbedHostDomains` `@length min 1 max 20` and `Description` `@length 0..256`.
fn refused_creates() -> Vec<RefusedCreate> {
    vec![
        RefusedCreate {
            case: "two entries of the one endpoint type",
            members: json!({
                "AccessEndpoints": [
                    { "EndpointType": STREAMING, "VpceId": SOME_ENDPOINT_ID },
                    { "EndpointType": STREAMING, "VpceId": SOME_ENDPOINT_ID },
                ],
            }),
            code: "InvalidParameterCombinationException",
        },
        RefusedCreate {
            case: "an entry that references no endpoint",
            members: json!({ "AccessEndpoints": [{ "EndpointType": STREAMING }] }),
            code: "ValidationException",
        },
        RefusedCreate {
            case: "an access endpoint list below its minimum",
            members: json!({ "AccessEndpoints": [] }),
            code: "ValidationException",
        },
        RefusedCreate {
            case: "an access endpoint list above its maximum",
            members: json!({
                "AccessEndpoints": [
                    { "EndpointType": STREAMING, "VpceId": "vpce-0a1b2c3d4e5f60711" },
                    { "EndpointType": STREAMING, "VpceId": "vpce-0a1b2c3d4e5f60712" },
                    { "EndpointType": STREAMING, "VpceId": "vpce-0a1b2c3d4e5f60713" },
                    { "EndpointType": STREAMING, "VpceId": "vpce-0a1b2c3d4e5f60714" },
                    { "EndpointType": STREAMING, "VpceId": "vpce-0a1b2c3d4e5f60715" },
                ],
            }),
            code: "ValidationException",
        },
        RefusedCreate {
            case: "a user setting list below its minimum",
            members: json!({ "UserSettings": [] }),
            code: "ValidationException",
        },
        RefusedCreate {
            case: "an embedded host domain list below its minimum",
            members: json!({ "EmbedHostDomains": [] }),
            code: "ValidationException",
        },
        RefusedCreate {
            case: "a description past its maximum length",
            members: json!({ "Description": "d".repeat(257) }),
            code: "ValidationException",
        },
    ]
}

/// The create request `members` makes under `name`.
fn create_request(name: &str, members: &Value) -> Value {
    let mut body = json!({ "Name": name });
    if let Some(members) = members.as_object() {
        let object = body
            .as_object_mut()
            .expect("the create request is an object");
        for (member, value) in members {
            object.insert(member.clone(), value.clone());
        }
    }
    body
}

#[tokio::test]
async fn a_refused_create_does_not_reserve_the_name_it_was_refused_under() {
    let emulator = Emulator::new();

    for (index, row) in refused_creates().into_iter().enumerate() {
        let RefusedCreate {
            case,
            members,
            code,
        } = row;
        let name = format!("retried-create-{index}");

        refusal(
            &emulator,
            case,
            "CreateStack",
            create_request(&name, &members),
            REFUSAL_STATUS,
            code,
        )
        .await;

        // Nothing was stored: the name is one this account holds no stack under, so the
        // refused attempt reserved nothing.
        refusal(
            &emulator,
            case,
            "DescribeStacks",
            json!({ "Names": [&name] }),
            REFUSAL_STATUS,
            "ResourceNotFoundException",
        )
        .await;

        // The corrected resubmission of the SAME name is an ordinary first create.
        let created = emulator
            .ok(
                case,
                "CreateStack",
                json!({
                    "Name": &name,
                    "AccessEndpoints": [{ "EndpointType": STREAMING, "VpceId": SOME_ENDPOINT_ID }],
                }),
            )
            .await;
        let stack = &created["Stack"];
        assert_eq!(
            stack["Name"],
            json!(&name),
            "case {case:?}: expected the retry to create {name:?}, got {stack}"
        );
        assert_eq!(
            stack["AccessEndpoints"],
            json!([{ "EndpointType": STREAMING, "VpceId": SOME_ENDPOINT_ID }]),
            "case {case:?}: expected the retry to report the endpoint it named, got {stack}"
        );
        // The retry is a create like any other, so the stack it creates carries everything
        // the service settles itself: its errors (none), its state, and the configuration
        // the request left unset.
        assert_eq!(
            stack["StackErrors"],
            json!([]),
            "case {case:?}: expected a created stack to report no errors, got {stack}"
        );
        for member in [
            "Arn",
            "CreatedTime",
            "State",
            "UserSettings",
            "ApplicationSettings",
            "StreamingExperienceSettings",
        ] {
            assert!(
                stack.get(member).is_some(),
                "case {case:?}: expected the created stack to report {member}, got {stack}"
            );
        }

        // ROUND TRIP: the reader answers from the stored record.
        let described = emulator
            .ok(case, "DescribeStacks", json!({ "Names": [&name] }))
            .await;
        assert_eq!(
            described["Stacks"][0], *stack,
            "case {case:?}: expected DescribeStacks to report the created stack {stack}, \
             got {described}"
        );

        // And once the name IS taken, a create of it is refused for that reason.
        refusal(
            &emulator,
            case,
            "CreateStack",
            json!({ "Name": &name }),
            REFUSAL_STATUS,
            "ResourceAlreadyExistsException",
        )
        .await;
    }
}
