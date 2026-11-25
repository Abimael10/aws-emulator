// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of AWS IoT BILLING GROUP MEMBERSHIP:
//! `AddThingToBillingGroup` and the `DescribeThing` reader that proves what it persisted.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a membership is a relation between records that EXIST, written by naming each end
//!   either by name or by ARN: an end this account does not hold is
//!   `ResourceNotFoundException` (404) with the registry families' account-qualified
//!   sentence and records nothing, and a request that names no end at all is
//!   `InvalidRequestException` (400);
//! * the relation is SINGLE-VALUED on the thing's side — `DescribeThing$billingGroupName`
//!   and `CreateThing$billingGroupName` each carry one group, not a list
//!   (`smithy_query(query="operation",operation="DescribeThing")`) — so an add re-points a
//!   thing that was in another group, leaves one already in this group as it is, and the
//!   thing's reader reports exactly the group the last accepted add named;
//! * a membership write is a write of the RELATION, not of the group record, so the
//!   group's own version is where its last property write left it.
//!
//! The table spans what the rule generalises over — which end is named by name and which
//! by ARN, the form and length of each name within its modelled `length 1..128, pattern
//! ^[a-zA-Z0-9:_-]+$`, how many groups the thing has been moved between and how many
//! things share a group — rather than one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::FIRST_BILLING_GROUP_VERSION;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id this emulator serves, which the registry families qualify a missing name
/// with (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for, and the one the ARNs a request may
/// name an end by carry.
const REGION: &str = "eu-west-1";

/// The wire error shape of a request naming a resource this account does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The wire error shape of a request the service cannot make sense of.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// What the service states to a request that addressed no billing group, and to one that
/// addressed no thing: each names the two members that may address that end.
const UNADDRESSED_GROUP: &str =
    "either billingGroupName or billingGroupArn must name the billing group to add the thing to";
const UNADDRESSED_THING: &str =
    "either thingName or thingArn must name the thing to add to the billing group";

/// The longest name either end admits: `BillingGroupName` and `ThingName` are both
/// `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`
/// (`smithy_query(query="operation",operation="AddThingToBillingGroup")`).
const MAX_NAME_LEN: usize = 128;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never checked
/// here — the harness turns request authentication off, exactly as an unauthenticated
/// emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The error shape an SDK branches on, as this service names it in the body.
    fn error_code(&self) -> Option<String> {
        self.text("__type")
    }

    /// The `message` an SDK surfaces for a refusal.
    fn message(&self) -> Option<String> {
        self.text("message")
    }

    /// The `version` a read of a billing group reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
    }

    /// The `billingGroupName` a read of a thing reports, or `None` when the thing is in no
    /// billing group.
    fn billing_group(&self) -> Option<String> {
        self.text("billingGroupName")
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// `CreateBillingGroup`, bound to `POST /billing-groups/{billingGroupName}`.
    async fn create_group(&self, name: &str) -> Reply {
        self.call("POST", &format!("/billing-groups/{name}"), Some(&json!({})))
            .await
    }

    /// `CreateThing`, bound to `POST /things/{thingName}`.
    async fn create_thing(&self, name: &str) -> Reply {
        self.call("POST", &format!("/things/{name}"), Some(&json!({})))
            .await
    }

    /// `AddThingToBillingGroup`, bound to `PUT /billing-groups/addThingToBillingGroup` with
    /// both ends in the body.
    async fn add(&self, body: &Value) -> Reply {
        self.call("PUT", "/billing-groups/addThingToBillingGroup", Some(body))
            .await
    }

    /// `DescribeThing`, bound to `GET /things/{thingName}` — the reader that reports which
    /// billing group a thing belongs to.
    async fn describe_thing(&self, name: &str) -> Reply {
        self.call("GET", &format!("/things/{name}"), None).await
    }

    /// `DescribeBillingGroup`, bound to `GET /billing-groups/{billingGroupName}`.
    async fn describe_group(&self, name: &str) -> Reply {
        self.call("GET", &format!("/billing-groups/{name}"), None)
            .await
    }
}

/// The sentence the registry families state a missing billing group with.
fn missing_group_message(name: &str) -> String {
    format!("BillingGroup {ACCOUNT_ID}:{name} not found.")
}

/// The sentence the registry families state a missing thing with.
fn missing_thing_message(name: &str) -> String {
    format!("Thing {ACCOUNT_ID}:{name} not found.")
}

/// `arn:aws:iot:<region>:<account>:billinggroup/<name>` — the ARN a request may name the
/// group end by, in the form `CreateBillingGroup` answers with.
fn billing_group_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:billinggroup/{name}")
}

/// `arn:aws:iot:<region>:<account>:thing/<name>` — the ARN a request may name the thing end
/// by, in the form `CreateThing` answers with.
fn thing_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{name}")
}

/// How an end is named: by its name, or by the ARN whose resource id IS that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    ByName,
    ByArn,
}

/// The `AddThingToBillingGroup` body that names `group` and `thing` the way the row asks.
fn add_body(group: &str, group_named: Named, thing: &str, thing_named: Named) -> Value {
    let mut body = json!({});
    match group_named {
        Named::ByName => body["billingGroupName"] = json!(group),
        Named::ByArn => body["billingGroupArn"] = json!(billing_group_arn(group)),
    }
    match thing_named {
        Named::ByName => body["thingName"] = json!(thing),
        Named::ByArn => body["thingArn"] = json!(thing_arn(thing)),
    }
    body
}

/// One row: the two names, and the form each end is named in.
struct Case {
    label: &'static str,
    group: String,
    group_named: Named,
    thing: String,
    thing_named: Named,
}

/// The table every accepted-add case below is built from. The rows differ along what the
/// rule generalises over: which end is named by name and which by ARN (all four
/// combinations), and the form and length of each name within the modelled
/// `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "both ends by name",
            group: "metered-fleet".to_string(),
            group_named: Named::ByName,
            thing: "laser-1".to_string(),
            thing_named: Named::ByName,
        },
        Case {
            label: "the group by ARN, the thing by name",
            group: "EU:billing_group-9".to_string(),
            group_named: Named::ByArn,
            thing: "sensor_42".to_string(),
            thing_named: Named::ByName,
        },
        Case {
            label: "the group by name, the thing by ARN",
            group: "b".to_string(),
            group_named: Named::ByName,
            thing: "t".to_string(),
            thing_named: Named::ByArn,
        },
        Case {
            label: "both ends by ARN",
            group: "Metered:Fleet_01-x".to_string(),
            group_named: Named::ByArn,
            thing: "Line-haul:Truck_7".to_string(),
            thing_named: Named::ByArn,
        },
        Case {
            label: "both names at the modelled length ceiling",
            group: "g".repeat(MAX_NAME_LEN),
            group_named: Named::ByName,
            thing: "h".repeat(MAX_NAME_LEN),
            thing_named: Named::ByArn,
        },
    ]
}

/// RULE: an accepted `AddThingToBillingGroup` records the membership between the two records
/// the request names — in either form — and the thing's reader then reports exactly that
/// group; the add writes nothing else, so a repeat of it leaves the same one membership and
/// the group record's own version where its last property write left it.
#[tokio::test]
async fn an_add_records_the_membership_the_thing_reader_reports() {
    for case in cases() {
        let Case {
            label,
            group,
            group_named,
            thing,
            thing_named,
        } = case;
        // A fresh emulator per row, so what one row stored cannot decide another.
        let emulator = Emulator::new();
        assert_eq!(
            emulator.create_group(&group).await.status,
            200,
            "case {label:?}: the billing group the membership is written to must exist"
        );
        assert_eq!(
            emulator.create_thing(&thing).await.status,
            200,
            "case {label:?}: the thing the membership is written for must exist"
        );
        let before = emulator.describe_thing(&thing).await;
        assert_eq!(
            before.billing_group(),
            None,
            "case {label:?}: expected a freshly created thing in no billing group, got \
             {before:?}"
        );

        let added = emulator
            .add(&add_body(&group, group_named, &thing, thing_named))
            .await;
        assert_eq!(
            added.status, 200,
            "case {label:?}: AddThingToBillingGroup must succeed, got {added:?}"
        );
        assert_eq!(
            added.body,
            json!({}),
            "case {label:?}: expected the modelled empty response, got {added:?}"
        );

        let read = emulator.describe_thing(&thing).await;
        assert_eq!(
            read.billing_group(),
            Some(group.clone()),
            "case {label:?}: expected the thing to report billing group {group:?}, got {read:?}"
        );
        // A read writes nothing, so a second one reports the same record.
        let again = emulator.describe_thing(&thing).await;
        assert_eq!(
            again.body, read.body,
            "case {label:?}: expected a second read to report the same thing, got {again:?}"
        );

        // The add names no new identity: repeating it leaves the same one membership.
        let repeated = emulator
            .add(&add_body(&group, group_named, &thing, thing_named))
            .await;
        assert_eq!(
            repeated.status, 200,
            "case {label:?}: a repeat add must succeed, got {repeated:?}"
        );
        let after_repeat = emulator.describe_thing(&thing).await;
        assert_eq!(
            after_repeat.body, read.body,
            "case {label:?}: expected the repeat add to change nothing, got {after_repeat:?}"
        );

        // The write is to the relation, not to the group record, so the group's version is
        // still the one its create left.
        let group_read = emulator.describe_group(&group).await;
        assert_eq!(
            group_read.version(),
            Some(FIRST_BILLING_GROUP_VERSION),
            "case {label:?}: expected the group record untouched by the membership write, got \
             {group_read:?}"
        );
    }
}

/// RULE: the relation is single-valued on the thing's side, so an add re-points a thing that
/// was in another billing group, and each thing's membership is its own.
#[tokio::test]
async fn a_thing_belongs_to_one_billing_group_at_a_time() {
    let emulator = Emulator::new();
    // Four groups and two things: enough that a membership appended rather than re-pointed,
    // or one written to the wrong thing, is a different reading.
    let groups = ["first-group", "second:group", "third_group", "fourth-group"];
    for group in groups {
        assert_eq!(
            emulator.create_group(group).await.status,
            200,
            "group {group}: the group a thing is moved into must exist"
        );
    }
    for thing in ["mover", "bystander"] {
        assert_eq!(
            emulator.create_thing(thing).await.status,
            200,
            "thing {thing}: the thing the memberships are written for must exist"
        );
    }

    // The bystander is parked in the first group and never named again: its membership must
    // survive every move the other thing makes.
    assert_eq!(
        emulator
            .add(&json!({ "billingGroupName": groups[0], "thingName": "bystander" }))
            .await
            .status,
        200,
        "the bystander's membership must be recorded"
    );

    for group in groups {
        let moved = emulator
            .add(&json!({ "billingGroupName": group, "thingName": "mover" }))
            .await;
        assert_eq!(
            moved.status, 200,
            "case moving into {group}: the add must succeed, got {moved:?}"
        );
        let read = emulator.describe_thing("mover").await;
        assert_eq!(
            read.billing_group(),
            Some(group.to_string()),
            "case moving into {group}: expected the thing in exactly that group, got {read:?}"
        );
        let bystander = emulator.describe_thing("bystander").await;
        assert_eq!(
            bystander.billing_group(),
            Some(groups[0].to_string()),
            "case moving into {group}: expected the other thing's membership untouched, got \
             {bystander:?}"
        );
    }

    // Every group the thing passed through still exists: the move is a write of the
    // relation, not a delete of a group.
    for group in groups {
        let read = emulator.describe_group(group).await;
        assert_eq!(
            read.status, 200,
            "group {group}: expected the group to survive the moves, got {read:?}"
        );
    }
}

/// RULE: an end this account does not hold is refused `ResourceNotFoundException` (404) with
/// the registry families' account-qualified sentence, and the refused add records nothing.
#[tokio::test]
async fn an_end_the_account_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_group("metered-fleet").await.status,
        200,
        "the group the misses are measured against must exist"
    );
    assert_eq!(
        emulator.create_thing("laser-1").await.status,
        200,
        "the thing the misses are measured against must exist"
    );

    // The rows span which end misses and in which form it was named: a name nothing
    // created, a prefix of a stored name, a stored name in another case, a name at the
    // modelled ceiling, and an ARN whose resource id names nothing. The GROUP end is
    // resolved first — it is the end the request addresses and the one the membership is
    // written to — so the row naming neither existing record is told about the group.
    let misses: Vec<(&str, Value, String)> = vec![
        (
            "a group name nothing created",
            json!({ "billingGroupName": "absent-group", "thingName": "laser-1" }),
            missing_group_message("absent-group"),
        ),
        (
            "a prefix of a stored group name",
            json!({ "billingGroupName": "metered", "thingName": "laser-1" }),
            missing_group_message("metered"),
        ),
        (
            "a stored group name in another case",
            json!({ "billingGroupName": "Metered-Fleet", "thingName": "laser-1" }),
            missing_group_message("Metered-Fleet"),
        ),
        (
            "a group ARN naming nothing",
            json!({
                "billingGroupArn": billing_group_arn("absent-group"),
                "thingName": "laser-1",
            }),
            missing_group_message("absent-group"),
        ),
        (
            "a group name at the modelled ceiling",
            json!({
                "billingGroupName": "g".repeat(MAX_NAME_LEN),
                "thingName": "laser-1",
            }),
            missing_group_message(&"g".repeat(MAX_NAME_LEN)),
        ),
        (
            "a thing name nothing created",
            json!({ "billingGroupName": "metered-fleet", "thingName": "absent-thing" }),
            missing_thing_message("absent-thing"),
        ),
        (
            "a stored thing name with a suffix",
            json!({ "billingGroupName": "metered-fleet", "thingName": "laser-12" }),
            missing_thing_message("laser-12"),
        ),
        (
            "a thing ARN naming nothing",
            json!({
                "billingGroupName": "metered-fleet",
                "thingArn": thing_arn("absent-thing"),
            }),
            missing_thing_message("absent-thing"),
        ),
        (
            "both ends naming nothing",
            json!({ "billingGroupName": "absent-group", "thingName": "absent-thing" }),
            missing_group_message("absent-group"),
        ),
    ];

    for (label, body, message) in misses {
        let refused = emulator.add(&body).await;
        assert_eq!(
            refused.status, 404,
            "case {label:?}: expected a 404 refusal, got {refused:?}"
        );
        assert_eq!(
            refused.error_code().as_deref(),
            Some(NOT_FOUND),
            "case {label:?}: expected {NOT_FOUND}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(message.clone()),
            "case {label:?}: expected {message:?}, got {refused:?}"
        );
    }

    // Nothing above was recorded: the thing that exists is still in no billing group, and
    // the group that exists is still at the version its create left.
    let thing = emulator.describe_thing("laser-1").await;
    assert_eq!(
        thing.billing_group(),
        None,
        "expected the refused adds to have recorded no membership, got {thing:?}"
    );
    let group = emulator.describe_group("metered-fleet").await;
    assert_eq!(
        group.version(),
        Some(FIRST_BILLING_GROUP_VERSION),
        "expected the refused adds to have left the group untouched, got {group:?}"
    );
}

/// RULE: a request that names no billing group, or no thing, asks for no membership at all
/// and is refused `InvalidRequestException` (400) — never a 200 that records nothing.
#[tokio::test]
async fn a_request_that_names_no_end_is_refused() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_group("metered-fleet").await.status,
        200,
        "the group the request may name must exist"
    );
    assert_eq!(
        emulator.create_thing("laser-1").await.status,
        200,
        "the thing the request may name must exist"
    );

    // The rows span the ways a request can name no end: neither member at all, only one of
    // the two ends, and an ARN of ANOTHER resource type, which names no resource of this
    // one. Each row carries the sentence that end's refusal states, so which end the request
    // left unaddressed is what the caller is told; the group end is read first, so a request
    // naming neither is told about the group.
    let unaddressed: Vec<(&str, Value, &str)> = vec![
        ("naming neither end", json!({}), UNADDRESSED_GROUP),
        (
            "naming only the thing",
            json!({ "thingName": "laser-1" }),
            UNADDRESSED_GROUP,
        ),
        (
            "naming only the group",
            json!({ "billingGroupName": "metered-fleet" }),
            UNADDRESSED_THING,
        ),
        (
            "a group ARN of another resource type",
            json!({
                "billingGroupArn": format!(
                    "arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/metered-fleet",
                ),
                "thingName": "laser-1",
            }),
            UNADDRESSED_GROUP,
        ),
        (
            "a thing ARN of another resource type",
            json!({
                "billingGroupName": "metered-fleet",
                "thingArn": format!(
                    "arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/laser-1",
                ),
            }),
            UNADDRESSED_THING,
        ),
        (
            "a group ARN that is not an ARN at all",
            json!({ "billingGroupArn": "metered-fleet", "thingName": "laser-1" }),
            UNADDRESSED_GROUP,
        ),
    ];

    for (label, body, message) in unaddressed {
        let refused = emulator.add(&body).await;
        assert_eq!(
            refused.status, 400,
            "case {label:?}: expected a 400 refusal, got {refused:?}"
        );
        assert_eq!(
            refused.error_code().as_deref(),
            Some(INVALID_REQUEST),
            "case {label:?}: expected {INVALID_REQUEST}, got {refused:?}"
        );
        assert_eq!(
            refused.message().as_deref(),
            Some(message),
            "case {label:?}: expected {message:?}, got {refused:?}"
        );
        let thing = emulator.describe_thing("laser-1").await;
        assert_eq!(
            thing.billing_group(),
            None,
            "case {label:?}: expected the refused add to record nothing, got {thing:?}"
        );
    }
}
