// SPDX-License-Identifier: CC0-1.0

//! What a Wickr bot read reports for the values a bot only holds once its software
//! has SIGNED IN, driven over HTTP against one running emulator.
//!
//! Every case goes through the real multi-service router — route resolution,
//! restJson1 deserialization, modelled-constraint validation, the handler and
//! response serialization — so what it asserts is the wire behaviour a client
//! sees.
//!
//! The rule under test, and where it comes from:
//!
//! * `Bot`/`GetBotResponse` carry `status` — "the current status of the bot (1 for
//!   pending, 2 for active)", the `BotStatus` `intEnum` whose PENDING (1) is "the
//!   bot has been created but is not yet active, pending initial setup or
//!   activation" and whose ACTIVE (2) is "the bot is active and operational, able
//!   to send and receive messages" — beside three members a bot comes to hold only
//!   through that activation: `uname`, "the unique username hash identifier for the
//!   bot", `pubkey`, "the public key of the bot used for encryption", and
//!   `lastLogin`, "the timestamp of the bot's last login". None of the three is
//!   `@required`. So a read reports each of them only for a bot that has signed in:
//!   a bot still waiting for its first sign-in reports none of them, whatever else
//!   it carries.
//! * `GetBot` ("retrieves detailed information about a specific bot") and
//!   `ListBots` (`GET /networks/{networkId}/bots`, whose `bots` records carry the
//!   same members) report the same stored bot, so they report it the same way: a
//!   listing cannot show a value a read withholds.
//! * The admin API has no operation that signs a bot in, so the status a bot
//!   reaches that way is reached here by writing the status member of the very
//!   record every read answers from — the same seam `bot_password_policy.rs` uses
//!   for a group policy no implemented operation sets yet.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{
    derive_uname, service_state, BOT_STATUS_ACTIVE, BOT_STATUS_PENDING,
};
use smithy_server_runtime::MultiServiceRouter;

/// The status `CreateNetwork` answers a create it carried out with.
const CREATE_NETWORK_STATUS: u16 = 201;

/// A password meeting the default policy of a network's seeded security group, so
/// no create below is decided by the policy.
const CHALLENGE: &str = "longenoughpassword1";

/// The three members a bot holds only once it has signed in, as the model names
/// them on `Bot` and `GetBotResponse`.
const SIGN_IN_MEMBERS: [&str; 3] = ["uname", "pubkey", "lastLogin"];

/// A fresh emulator holding only this service, with the engine its store lives in
/// handed back beside it, so one test's networks are invisible to another's.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the bot projection is.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// One request/response round trip: the HTTP status and the parsed JSON body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, parsed)
}

/// Create a network and return its minted id.
async fn create_network(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await;
    assert_eq!(
        status, CREATE_NETWORK_STATUS,
        "CreateNetwork {name:?}: expected {CREATE_NETWORK_STATUS}, got {status} with {body}"
    );
    body["networkId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNetwork {name:?}: no networkId in {body}"))
        .to_string()
}

/// The id of the network's seeded security group.
async fn default_group_id(router: &MultiServiceRouter, network_id: &str) -> String {
    let (status, body) = call(
        router,
        "GET",
        &format!("/networks/{network_id}/security-groups"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "ListSecurityGroups: expected 200, got {status} with {body}"
    );
    body["securityGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .and_then(|group| group["id"].as_str())
        .unwrap_or_else(|| panic!("ListSecurityGroups: no group id in {body}"))
        .to_string()
}

/// Create one more security group of `network_id` and return its minted id.
async fn create_group(router: &MultiServiceRouter, network_id: &str, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/networks/{network_id}/security-groups"),
        Some(json!({ "name": name, "securityGroupSettings": {} })),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "CreateSecurityGroup {name:?}: expected a success, got {status} with {body}"
    );
    body["securityGroup"]["id"]
        .as_str()
        .or_else(|| body["id"].as_str())
        .unwrap_or_else(|| panic!("CreateSecurityGroup {name:?}: no group id in {body}"))
        .to_string()
}

/// Create one bot and return its minted `botId`.
async fn create_bot(
    router: &MultiServiceRouter,
    network_id: &str,
    group_id: &str,
    username: &str,
    display_name: Option<&str>,
) -> String {
    let mut request = json!({
        "username": username,
        "groupId": group_id,
        "challenge": CHALLENGE,
    });
    if let Some(name) = display_name {
        request["displayName"] = json!(name);
    }
    let (status, body) = call(
        router,
        "POST",
        &format!("/networks/{network_id}/bots"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateBot {username:?}: expected 200, got {status} with {body}"
    );
    body["botId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateBot {username:?}: no botId in {body}"))
        .to_string()
}

/// Read one bot back, asserting the read was served.
async fn get_bot(router: &MultiServiceRouter, case: &str, network_id: &str, bot_id: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/networks/{network_id}/bots/{bot_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetBot expected 200, got {status} with {body}"
    );
    body
}

/// The bots one network's listing reports, asserting the listing was served.
async fn list_bots(router: &MultiServiceRouter, case: &str, network_id: &str) -> Vec<Value> {
    let (status, body) = call(router, "GET", &format!("/networks/{network_id}/bots"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBots expected 200, got {status} with {body}"
    );
    body["bots"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: bots is required, got {body}"))
}

/// The listed record of one bot.
fn listed<'a>(case: &str, listing: &'a [Value], bot_id: &str) -> &'a Value {
    listing
        .iter()
        .find(|bot| bot["botId"] == json!(bot_id))
        .unwrap_or_else(|| panic!("case {case:?}: no record of bot {bot_id} in {listing:?}"))
}

/// Move one stored bot to the status its software reaches by signing in.
///
/// No operation of this admin API signs a bot in, so the status is written on the
/// one record every read answers from — which is exactly what a sign-in would
/// move.
fn sign_bot_in(
    engine: &Arc<engine::InMemoryEngine>,
    network_id: &str,
    bot_id: &str,
    status: i32,
) {
    let state = service_state(&**engine).expect("the service store is reachable");
    let mut store = state.lock();
    let bot = store
        .bot_of_mut(network_id, bot_id)
        .unwrap_or_else(|| panic!("the network holds no bot {bot_id:?}"));
    bot.status = status;
}

/// One bot of the table: how it was created, and whether it has signed in.
struct Case {
    /// What the row is about.
    name: &'static str,
    username: &'static str,
    display_name: Option<&'static str>,
    /// Whether the bot joins the network's seeded group or one created beside it.
    in_default_group: bool,
    /// The status the stored bot ends up in: created PENDING, signed in or not.
    signed_in: bool,
}

/// The table both tests drive: rows differing along the values the rule
/// generalises over — the form and length of the `username`, whether a
/// `displayName` was given, which security group the bot joined, and whether the
/// bot has signed in.
const CASES: [Case; 8] = [
    Case {
        name: "a pending bot of the seeded group",
        username: "probebot",
        display_name: Some("Probe Bot"),
        in_default_group: true,
        signed_in: false,
    },
    Case {
        name: "a pending bot with no display name",
        username: "namelessbot",
        display_name: None,
        in_default_group: true,
        signed_in: false,
    },
    Case {
        name: "a pending bot of a group created beside the seeded one",
        username: "grouped-probe-bot",
        display_name: Some("Grouped Probe Bot"),
        in_default_group: false,
        signed_in: false,
    },
    Case {
        name: "a pending bot whose username is the suffix alone",
        username: "bot",
        display_name: Some("Minimal"),
        in_default_group: true,
        signed_in: false,
    },
    Case {
        name: "a pending bot whose username is non-ASCII",
        username: "\u{30DC}\u{30C3}\u{30C8}-\u{1F916}-bot",
        display_name: Some("Unicode Bot"),
        in_default_group: false,
        signed_in: false,
    },
    Case {
        name: "a signed-in bot of the seeded group",
        username: "activebot",
        display_name: Some("Active Bot"),
        in_default_group: true,
        signed_in: true,
    },
    Case {
        name: "a signed-in bot of a group created beside the seeded one",
        username: "grouped-active-bot",
        display_name: None,
        in_default_group: false,
        signed_in: true,
    },
    Case {
        name: "a signed-in bot whose username is non-ASCII",
        username: "\u{4E2D}\u{6587}-active-bot",
        display_name: Some("Unicode Active Bot"),
        in_default_group: false,
        signed_in: true,
    },
];

/// Create every row of [`CASES`] in one network and return each row's `botId`, in
/// the table's order.
async fn create_cases(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    network_name: &str,
) -> (String, Vec<String>) {
    let network_id = create_network(router, network_name).await;
    let seeded = default_group_id(router, &network_id).await;
    let beside = create_group(router, &network_id, "Beside").await;
    assert_ne!(
        seeded, beside,
        "the second group must be a different group, got {seeded} twice"
    );

    let mut bot_ids = Vec::new();
    for case in &CASES {
        let group_id = if case.in_default_group {
            &seeded
        } else {
            &beside
        };
        let bot_id = create_bot(
            router,
            &network_id,
            group_id,
            case.username,
            case.display_name,
        )
        .await;
        if case.signed_in {
            sign_bot_in(engine, &network_id, &bot_id, BOT_STATUS_ACTIVE);
        }
        bot_ids.push(bot_id);
    }
    (network_id, bot_ids)
}

/// Whether `value` is the hex encoding of a SHA-256 digest, which is the form a
/// username hash takes.
fn is_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// RULE: a bot reports the members a sign-in mints — its username hash, its public
/// key and its last login — only once it has signed in; a bot still pending
/// reports none of them, whatever else it carries.
#[tokio::test]
async fn a_read_reports_a_uname_only_for_a_bot_that_has_signed_in() {
    let (router, engine) = emulator();
    let (network_id, bot_ids) = create_cases(&router, &engine, "bot-uname-net").await;

    for (case, bot_id) in CASES.iter().zip(&bot_ids) {
        let name = case.name;
        let read = get_bot(&router, name, &network_id, bot_id).await;
        let expected_status = if case.signed_in {
            BOT_STATUS_ACTIVE
        } else {
            BOT_STATUS_PENDING
        };
        assert_eq!(
            read["status"],
            json!(expected_status),
            "case {name:?}: expected status {expected_status}, got {}",
            read["status"]
        );

        if case.signed_in {
            let uname = read["uname"].as_str().unwrap_or_default();
            assert_eq!(
                uname,
                derive_uname(&network_id, case.username),
                "case {name:?}: expected the stored username hash of the bot, got {uname:?}"
            );
            assert!(
                is_hex_digest(uname),
                "case {name:?}: expected a hex-encoded username hash, got {uname:?}"
            );
        } else {
            for member in SIGN_IN_MEMBERS {
                assert_eq!(
                    read[member],
                    Value::Null,
                    "case {name:?}: expected no {member} for a bot that has not signed in, got {}",
                    read[member]
                );
            }
        }

        // Withholding the sign-in members takes nothing else off the record.
        for member in ["botId", "username", "groupId", "hasChallenge", "suspended"] {
            assert_ne!(
                read[member],
                Value::Null,
                "case {name:?}: expected {member} to be reported, got {read}"
            );
        }
        assert_eq!(
            read["username"],
            json!(case.username),
            "case {name:?}: expected the stored username, got {}",
            read["username"]
        );
    }
}

/// RULE: the listing reports each bot exactly as a read of it does, so a pending
/// bot carries no username hash in a listing either — including a listing that
/// mixes bots that have signed in with bots that have not.
#[tokio::test]
async fn the_listing_reports_each_bot_as_a_read_of_it_does() {
    let (router, engine) = emulator();
    let (network_id, bot_ids) = create_cases(&router, &engine, "uname-listing").await;

    let listing = list_bots(&router, "the whole network", &network_id).await;
    assert_eq!(
        listing.len(),
        CASES.len(),
        "expected every created bot to be listed, got {listing:?}"
    );

    for (case, bot_id) in CASES.iter().zip(&bot_ids) {
        let name = case.name;
        let record = listed(name, &listing, bot_id);
        let read = get_bot(&router, name, &network_id, bot_id).await;

        for member in SIGN_IN_MEMBERS {
            assert_eq!(
                record[member], read[member],
                "case {name:?}: the listing and the read must agree about {member}, \
                 expected {}, got {}",
                read[member], record[member]
            );
        }
        if case.signed_in {
            assert_eq!(
                record["uname"],
                json!(derive_uname(&network_id, case.username)),
                "case {name:?}: expected the listed record to carry the username hash, got {record}"
            );
        } else {
            for member in SIGN_IN_MEMBERS {
                assert_eq!(
                    record[member],
                    Value::Null,
                    "case {name:?}: expected the listed record to carry no {member}, got {}",
                    record[member]
                );
            }
        }
    }
}

/// RULE: withholding the username hash is decided by the bot's status and nothing
/// else, so the same bot reports none and then reports it as its status moves —
/// and the value it then reports is the one it had all along.
#[tokio::test]
async fn the_same_bot_reports_its_uname_as_its_status_moves() {
    let (router, engine) = emulator();
    let network_id = create_network(&router, "uname-transition").await;
    let group_id = default_group_id(&router, &network_id).await;
    let username = "transitionbot";
    let bot_id = create_bot(
        &router,
        &network_id,
        &group_id,
        username,
        Some("Transition Bot"),
    )
    .await;
    let hash = derive_uname(&network_id, username);

    // (case, the status the stored bot is in, the username hash a read must then
    // report)
    let steps: [(&str, i32, Option<&str>); 3] = [
        ("as created, waiting for its first sign-in", BOT_STATUS_PENDING, None),
        ("once it has signed in", BOT_STATUS_ACTIVE, Some(&hash)),
        ("back to pending", BOT_STATUS_PENDING, None),
    ];

    for (case, status, expected) in steps {
        sign_bot_in(&engine, &network_id, &bot_id, status);
        let read = get_bot(&router, case, &network_id, &bot_id).await;
        let listing = list_bots(&router, case, &network_id).await;
        let record = listed(case, &listing, &bot_id);
        let wanted = match expected {
            Some(hash) => json!(hash),
            None => Value::Null,
        };
        assert_eq!(
            read["uname"], wanted,
            "case {case:?}: expected GetBot uname {wanted}, got {}",
            read["uname"]
        );
        assert_eq!(
            record["uname"], wanted,
            "case {case:?}: expected the listed uname {wanted}, got {}",
            record["uname"]
        );
    }
}
