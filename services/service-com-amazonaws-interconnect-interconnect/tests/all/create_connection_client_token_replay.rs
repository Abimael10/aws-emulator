// SPDX-License-Identifier: CC0-1.0

//! What a repeated `CreateConnection$clientToken` is judged against.
//!
//! RULE: a `@idempotencyToken` replay is answered with the connection the token
//! created only when the repeat IS that request — every member of the create the
//! caller owns compared, absence included — and a repeat that differs in any one of
//! them is refused with the operation's modelled `InterconnectValidationException`
//! (400) naming the token and the connection it already created, having provisioned
//! nothing.
//!
//! The table's rows differ in WHICH caller-owned member the repeat changes (and, for
//! the optional ones, in presence against absence), across every Environment of the
//! seeded catalog that takes connections — so the rule is checked on each kind of
//! identifying information an Environment asks for, not on one request.
//!
//! SECOND RULE, the other half of the same resolution: what a `clientToken` names is
//! the resource the create it was carried on produced, and only for as long as that
//! resource is one the account HOLDS. From the moment a `DeleteConnection` is made on
//! it the token names nothing — "the network connectivity provided by the Connection
//! will cease to exist" (`smithy_query(query="doc",shape="DeleteConnection")`) — so a
//! repeat carrying it is the create it reads as, never the torn-down record handed
//! back as the connection this create produced and never a mismatch judged against a
//! create whose resource is gone. Those rows differ in WHEN the repeat is made
//! relative to the delete (inside the teardown, and once it has settled), in whether
//! the repeat is the same request or a different one, and in the Region the request
//! addresses.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::emulator::{
    available_bandwidths, code, creatable_environments, message, remote_account_for, text, Emulator,
};

/// The gateways the rows attach their connections to: an attach point IS a Direct
/// Connect gateway of the account, so the suite creates its own, and a row that
/// changes the attach point on a retry has a second one to change it to.
const GATEWAY_ROWS: [(&str, Option<i64>); 2] = [
    ("ic-token-replay-a", None),
    ("ic-token-replay-b", Some(64_513)),
];

/// An account number that is not the emulator's own, so a retry can carry a
/// DIFFERENT `remoteAccount` that is still of the `account` kind an Environment may
/// declare — the refusal then has to come from the token comparison and from nothing
/// else.
const OTHER_ACCOUNT_IDENTIFIER: &str = "123456789012";

/// An email address that is not the one the harness picks, playing the same part for
/// an Environment declaring the `email` kind.
const OTHER_PARTNER_EMAIL: &str = "other-partner@example.test";

/// One `CreateConnection` request, as a map a row can change exactly one member of.
fn create_request(
    environment_id: &str,
    bandwidth: &str,
    attach_point: &str,
    description: &str,
    remote_account: Option<&Value>,
    tags: &Value,
    token: &str,
) -> Value {
    let mut request = Map::new();
    request.insert("environmentId".to_string(), json!(environment_id));
    request.insert("bandwidth".to_string(), json!(bandwidth));
    request.insert(
        "attachPoint".to_string(),
        json!({ "directConnectGateway": attach_point }),
    );
    request.insert("description".to_string(), json!(description));
    request.insert("tags".to_string(), tags.clone());
    if let Some(remote_account) = remote_account {
        request.insert("remoteAccount".to_string(), remote_account.clone());
    }
    request.insert("clientToken".to_string(), json!(token));
    Value::Object(request)
}

/// A `remoteAccount` that differs from the one `environment` was created with while
/// still being what that Environment asks for — or, for an Environment that declares
/// no `remoteIdentifierType` and so was created without the member at all, a
/// `remoteAccount` where the first call sent none.
///
/// The two kinds are the values `RemoteAccountIdentifierType` models
/// (`smithy_query(query="shape",shape="RemoteAccountIdentifierType")` -> values:
/// ["account", "email"]).
fn other_remote_account(environment: &Value) -> Value {
    match environment["remoteIdentifierType"].as_str() {
        Some("email") => json!({ "identifier": OTHER_PARTNER_EMAIL }),
        _ => json!({ "identifier": OTHER_ACCOUNT_IDENTIFIER }),
    }
}

/// A repeat of `base` differing in exactly ONE caller-owned member, per row, labelled
/// by the member it changes.
///
/// `description` and `tags` are optional on the input, so each contributes a row that
/// REMOVES it as well as one that changes it: absence is a value of the member too.
/// `remoteAccount` changes to another value of the kind the Environment asks for, so
/// nothing but the token comparison can refuse it.
fn repeats_that_are_a_different_request(
    base: &Value,
    environment: &Value,
    other_bandwidth: Option<&String>,
    other_gateway: &str,
    other_environment_id: Option<&String>,
) -> Vec<(String, Value)> {
    let changed = |member: &str, value: Value| {
        let mut repeat = base.clone();
        repeat[member] = value;
        repeat
    };
    let removed = |member: &str| {
        let mut repeat = base.clone();
        repeat
            .as_object_mut()
            .expect("the request is a JSON object")
            .remove(member);
        repeat
    };

    let mut rows = vec![
        (
            "a different description".to_string(),
            changed("description", json!("a description the first call did not send")),
        ),
        ("no description at all".to_string(), removed("description")),
        (
            "different tags".to_string(),
            changed("tags", json!({ "env": "a-value-the-first-call-did-not-send" })),
        ),
        (
            "an extra tag".to_string(),
            changed("tags", json!({ "env": "test", "team": "network" })),
        ),
        ("no tags at all".to_string(), removed("tags")),
        (
            "a different attach point".to_string(),
            changed(
                "attachPoint",
                json!({ "directConnectGateway": other_gateway }),
            ),
        ),
        (
            "a different remoteAccount".to_string(),
            changed("remoteAccount", other_remote_account(environment)),
        ),
    ];
    if let Some(bandwidth) = other_bandwidth {
        rows.push((
            "a different bandwidth".to_string(),
            changed("bandwidth", json!(bandwidth)),
        ));
    }
    if let Some(environment_id) = other_environment_id {
        rows.push((
            "a different environment".to_string(),
            changed("environmentId", json!(environment_id)),
        ));
    }
    rows
}

/// Every connection the store holds, walked page by page, so a refused retry can be
/// shown to have provisioned nothing.
async fn stored_ids(emulator: &Emulator) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = Map::new();
        request.insert("maxResults".to_string(), json!(100));
        if let Some(token) = &token {
            request.insert("nextToken".to_string(), json!(token));
        }
        let listed = emulator
            .ic_ok("ListConnections", Value::Object(request))
            .await;
        for summary in listed["connections"].as_array().cloned().unwrap_or_default() {
            all.insert(text(&summary, "id"));
        }
        token = listed["nextToken"].as_str().map(str::to_string);
        if token.is_none() {
            return all;
        }
    }
}

/// RULE: a repeated `clientToken` replays the connection it created only for the SAME
/// request; a repeat differing in any caller-owned member — `remoteAccount` included —
/// is refused as a token/parameter mismatch and provisions nothing.
#[tokio::test]
async fn a_repeated_client_token_is_judged_on_every_caller_owned_member() {
    let emulator = Emulator::new();
    let catalog = emulator.environments().await;
    let gateways = emulator.ready_gateways(&GATEWAY_ROWS).await;
    let environments = creatable_environments(&catalog);
    assert!(
        !environments.is_empty(),
        "the catalog must offer an environment that takes connections, got {}",
        environments.len()
    );
    // Another `environmentId` of the SAME endpoint, for the row that repeats the token
    // against a different environment. The published catalog offers one Environment per
    // Region, so that row has no subject here and is contributed only for a catalog that
    // offers a second one.
    let environment_ids: Vec<String> = environments
        .iter()
        .map(|environment| text(environment, "environmentId"))
        .collect();

    for (index, environment) in environments.iter().enumerate() {
        let environment_id = text(environment, "environmentId");
        let bandwidths = available_bandwidths(environment);
        let bandwidth = bandwidths[0].clone();
        let other_bandwidth = bandwidths.get(1).cloned();
        let other_environment_id = environment_ids
            .iter()
            .find(|candidate| **candidate != environment_id)
            .cloned();
        let remote_account = remote_account_for(environment);
        let token = format!("token-{index}");
        let base = create_request(
            &environment_id,
            &bandwidth,
            &gateways[index % gateways.len()],
            &format!("replay row {index}"),
            remote_account.as_ref(),
            &json!({ "env": "test" }),
            &token,
        );

        // The call the token names.
        let created = emulator.ic_ok("CreateConnection", base.clone()).await;
        let id = text(&created["connection"], "id");
        let case = format!("{environment_id} / {bandwidth} / {token}");
        assert!(
            !id.is_empty(),
            "case {case:?}: expected the first call to allocate a connection, got {created:?}"
        );

        // The SAME request repeated is that same connection reported again.
        let replay = emulator.ic_ok("CreateConnection", base.clone()).await;
        assert_eq!(
            replay["connection"], created["connection"],
            "case {case:?}: expected the repeat of an identical request to replay the first connection, got {:?}",
            replay["connection"]
        );

        // A repeat that differs in ONE caller-owned member is a different request.
        let before = stored_ids(&emulator).await;
        for (label, repeat) in repeats_that_are_a_different_request(
            &base,
            environment,
            other_bandwidth.as_ref(),
            &gateways[(index + 1) % gateways.len()],
            other_environment_id.as_ref(),
        ) {
            let case = format!("{case} / {label}");
            let (status, body) = emulator.ic("CreateConnection", repeat).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the reused token to be refused, got {status} with {body:?}"
            );
            assert!(
                code(&body).contains("InterconnectValidationException"),
                "case {case:?}: expected InterconnectValidationException, got {:?}",
                code(&body)
            );
            assert!(
                message(&body).contains(&token) && message(&body).contains(&id),
                "case {case:?}: expected the refusal to name the token and the connection it created, got {:?}",
                message(&body)
            );
        }
        let after = stored_ids(&emulator).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected every refused retry to have provisioned nothing, got {after:?} for {before:?} before"
        );

        // The member is part of the REQUEST, not part of the key: the same change
        // under a fresh token allocates a second connection.
        let mut fresh = base.clone();
        fresh["clientToken"] = json!(format!("{token}-fresh"));
        fresh["remoteAccount"] = other_remote_account(environment);
        let (status, body) = emulator.ic("CreateConnection", fresh).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a fresh token carrying another remoteAccount to be accepted, got {status} with {body:?}"
        );
        let fresh_id = text(&body["connection"], "id");
        assert_ne!(
            fresh_id, id,
            "case {case:?}: expected a distinct connection for a distinct client token, got {fresh_id:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A token whose connection has had a DeleteConnection made on it
// ---------------------------------------------------------------------------

/// The Regions the deletion rows address, so the rule is checked on more than one
/// endpoint's store and not on the harness's default one alone.
const DELETION_REGIONS: [&str; 2] = ["us-east-1", "eu-west-1"];

/// The transient `ConnectionState` a `DeleteConnection` hands the connection to the
/// clock in, and the terminal one that teardown settles into
/// (`smithy_query(query="shape",shape="ConnectionState")` -> values: ["available",
/// "requested", "pending", "down", "deleting", "deleted", "failed", "updating"]).
const DELETING_STATE: &str = "deleting";
const DELETED_STATE: &str = "deleted";

/// How long a wait for a called teardown to reach its terminal state runs before the
/// case fails. The row waits for the STATE the clock owes the record, re-reading it
/// until it is there, so the budget is deliberately far longer than the transition
/// takes.
const TEARDOWN_SETTLED_BUDGET: Duration = Duration::from_secs(30);

/// How often that wait re-reads.
const TEARDOWN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// When a row repeats the token, relative to the `DeleteConnection` made on the
/// connection that token created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeated {
    /// In the call right after the delete, while the teardown is still running.
    WhileTearingDown,
    /// Once the teardown has settled into the terminal state.
    OnceTornDown,
}

/// Both repeat points, so every row below spans the whole teardown.
const REPEAT_POINTS: [Repeated; 2] = [Repeated::WhileTearingDown, Repeated::OnceTornDown];

/// One `CreateConnection` body for `region`, placed on the first Environment that
/// Region's own catalog offers connections on and attached to one of the account's own
/// Direct Connect gateways — so a deletion row's only subject is the client token.
async fn deletion_row_request(
    emulator: &Emulator,
    region: &str,
    description: &str,
    token: &str,
) -> Value {
    let catalog = emulator.environments_in(region).await;
    let environments = creatable_environments(&catalog);
    let environment = environments
        .first()
        .unwrap_or_else(|| {
            panic!("setup {region:?}: expected the catalog to offer an environment that takes connections, got {catalog:?}")
        })
        .clone();
    let bandwidths = available_bandwidths(&environment);
    let bandwidth = bandwidths.first().unwrap_or_else(|| {
        panic!("setup {region:?}: expected the environment to offer a bandwidth, got {environment:?}")
    });
    create_request(
        &text(&environment, "environmentId"),
        bandwidth,
        &emulator.attach_point().await,
        description,
        remote_account_for(&environment).as_ref(),
        &json!({ "env": "test" }),
        token,
    )
}

/// The state `region`'s endpoint reports for `identifier` right now, the record being
/// readable by its own identifier for the whole teardown and after it.
async fn state_now(emulator: &Emulator, region: &str, identifier: &str) -> String {
    let read = emulator
        .ic_ok_in(region, "GetConnection", json!({ "identifier": identifier }))
        .await;
    text(&read["connection"], "state")
}

/// Re-read `identifier` until the teardown the delete armed has settled into its
/// terminal state, answering the state observed. Polls for the CONDITION rather than
/// waiting out a span.
async fn settled_state(emulator: &Emulator, region: &str, identifier: &str) -> String {
    let started = Instant::now();
    loop {
        let state = state_now(emulator, region, identifier).await;
        if state == DELETED_STATE {
            return state;
        }
        assert!(
            started.elapsed() < TEARDOWN_SETTLED_BUDGET,
            "setup {identifier:?}: expected the called teardown to settle into {DELETED_STATE:?}, last saw {state:?}"
        );
        tokio::time::sleep(TEARDOWN_POLL_INTERVAL).await;
    }
}

/// Call `DeleteConnection` on `identifier` and, for a row that repeats the token after
/// the teardown, poll until it has settled.
async fn delete_and_reach(
    emulator: &Emulator,
    region: &str,
    identifier: &str,
    repeated: Repeated,
    case: &str,
) {
    let deleted = emulator
        .ic_ok_in(
            region,
            "DeleteConnection",
            json!({ "identifier": identifier }),
        )
        .await;
    assert_eq!(
        text(&deleted["connection"], "state"),
        DELETING_STATE,
        "case {case:?}: expected the delete to arm the teardown, got {:?}",
        deleted["connection"]["state"]
    );
    if repeated == Repeated::OnceTornDown {
        let settled = settled_state(emulator, region, identifier).await;
        assert_eq!(
            settled, DELETED_STATE,
            "case {case:?}: expected the teardown to settle into the terminal state, got {settled:?}"
        );
    }
}

/// RULE: a `clientToken` names the connection its create produced only while that
/// connection is one the account HOLDS, so once a `DeleteConnection` has been made on
/// it — whether the teardown is still running or has settled — the same request
/// repeated allocates a NEW connection, readable back through `GetConnection`, instead
/// of being answered with the torn-down record; the record itself stays readable by its
/// own identifier, and the token then names the connection the repeat produced.
#[tokio::test]
async fn a_token_whose_connection_was_deleted_names_nothing_to_replay() {
    let emulator = Emulator::new();
    let mut repeated_inside_the_window = 0usize;

    for (seed, repeated) in REPEAT_POINTS.into_iter().enumerate() {
        for (offset, region) in DELETION_REGIONS.iter().enumerate() {
            let token = format!("deleted-token-{seed}-{offset}");
            let case = format!("{repeated:?} / {region} / {token}");
            let base = deletion_row_request(
                &emulator,
                region,
                &format!("deletion row {seed} {offset}"),
                &token,
            )
            .await;

            // The call the token names, and the replay it gets while the connection it
            // created is a resource the account holds.
            let created = emulator
                .ic_ok_in(region, "CreateConnection", base.clone())
                .await;
            let id = text(&created["connection"], "id");
            let replay = emulator
                .ic_ok_in(region, "CreateConnection", base.clone())
                .await;
            assert_eq!(
                replay["connection"], created["connection"],
                "case {case:?}: expected the repeat of a live create to replay its connection, got {:?}",
                replay["connection"]
            );

            delete_and_reach(&emulator, region, &id, repeated, &case).await;

            // The repeat: the token names nothing any more, so this is the create it
            // reads as — a connection of its own, in none of the states a delete leaves.
            let fresh = emulator
                .ic_ok_in(region, "CreateConnection", base.clone())
                .await["connection"]
                .clone();
            let fresh_id = text(&fresh, "id");
            let fresh_state = text(&fresh, "state");
            assert_ne!(
                fresh_id, id,
                "case {case:?}: expected the repeat to allocate a connection of its own, got {fresh:?}"
            );
            assert!(
                fresh_state != DELETING_STATE && fresh_state != DELETED_STATE,
                "case {case:?}: expected the repeat to be answered with a connection the account holds, got state {fresh_state:?}"
            );

            // Backed by the store: the connection the repeat reported reads back through
            // the implemented reader.
            let fetched = emulator
                .ic_ok_in(region, "GetConnection", json!({ "identifier": fresh_id }))
                .await;
            assert_eq!(
                text(&fetched["connection"], "id"),
                fresh_id,
                "case {case:?}: expected the connection the repeat produced to read back, got {:?}",
                fetched["connection"]
            );

            // The other half of the rule: the torn-down record is still readable by its
            // own identifier, and the repeat neither resurrected nor removed it. Read
            // right after the repeat, so a `deleting` answer says that repeat was made
            // inside the teardown window.
            let state_after = state_now(&emulator, region, &id).await;
            assert!(
                state_after == DELETING_STATE || state_after == DELETED_STATE,
                "case {case:?}: expected the deleted record to stay readable in a state a delete leaves, got {state_after:?}"
            );
            if repeated == Repeated::WhileTearingDown && state_after == DELETING_STATE {
                repeated_inside_the_window += 1;
            }

            // The token now names the connection the repeat produced, which IS a
            // resource the account holds: a further identical repeat replays that one.
            let second = emulator
                .ic_ok_in(region, "CreateConnection", base.clone())
                .await;
            assert_eq!(
                text(&second["connection"], "id"),
                fresh_id,
                "case {case:?}: expected the token to replay the connection the repeat produced, got {:?}",
                second["connection"]
            );
        }
    }

    assert!(
        repeated_inside_the_window > 0,
        "case \"teardown window\": expected at least one row to repeat its token while the connection was still being torn down, got {repeated_inside_the_window}"
    );
}

/// RULE, the refusal half: a repeat differing in a caller-owned member is a token
/// reused for a different request only while the connection the token created is one
/// the account holds — once a `DeleteConnection` has been made on it the token names no
/// create to compare against, so the differing repeat is accepted as the create it is,
/// carrying the members IT sent.
#[tokio::test]
async fn a_differing_repeat_is_not_judged_against_a_deleted_connection() {
    let emulator = Emulator::new();

    for (seed, repeated) in REPEAT_POINTS.into_iter().enumerate() {
        for (offset, region) in DELETION_REGIONS.iter().enumerate() {
            let token = format!("mismatch-after-delete-{seed}-{offset}");
            let case = format!("{repeated:?} / {region} / {token}");
            let base = deletion_row_request(
                &emulator,
                region,
                &format!("mismatch row {seed} {offset}"),
                &token,
            )
            .await;
            let mut differing = base.clone();
            let changed_description = format!("changed row {seed} {offset}");
            differing["description"] = json!(changed_description);

            let created = emulator
                .ic_ok_in(region, "CreateConnection", base.clone())
                .await;
            let id = text(&created["connection"], "id");

            // While the connection is held, the differing repeat is the mismatch the
            // suite above pins, so what changes below is the state of that connection
            // and nothing else.
            let (status, body) = emulator
                .ic_in(region, "CreateConnection", differing.clone())
                .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a differing repeat to be refused while the connection is held, got {status} with {body:?}"
            );
            assert!(
                code(&body).contains("InterconnectValidationException"),
                "case {case:?}: expected InterconnectValidationException, got {:?}",
                code(&body)
            );
            assert!(
                message(&body).contains(&token) && message(&body).contains(&id),
                "case {case:?}: expected the refusal to name the token and the connection it created, got {:?}",
                message(&body)
            );

            delete_and_reach(&emulator, region, &id, repeated, &case).await;

            // Once the delete has been made, the token names no create to compare
            // against: the differing repeat is accepted, and carries what IT sent.
            let (status, accepted) = emulator
                .ic_in(region, "CreateConnection", differing.clone())
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a differing repeat to be accepted once the token's connection was deleted, got {status} with {accepted:?}"
            );
            let fresh_id = text(&accepted["connection"], "id");
            assert_ne!(
                fresh_id, id,
                "case {case:?}: expected a connection of its own, got {:?}",
                accepted["connection"]
            );
            assert_eq!(
                text(&accepted["connection"], "description"),
                changed_description,
                "case {case:?}: expected the accepted create to carry the description it sent, got {:?}",
                accepted["connection"]["description"]
            );
        }
    }
}
