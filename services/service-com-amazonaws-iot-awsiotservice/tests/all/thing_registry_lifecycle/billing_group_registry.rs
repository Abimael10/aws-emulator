// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the AWS IoT BILLING GROUP store: `CreateBillingGroup`, the
//! `DescribeBillingGroup` reader that proves what it persisted, and the registry write
//! that resolves a thing's `billingGroupName` against it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a single-resource create of this control plane RECORDS the
//! resource under the name its `@httpLabel` carries, with an identity it mints (an id and
//! an `arn:aws:iot:<region>:<account>:<type>/<name>`), and answers with what it persisted;
//! the single-resource reader of that name reports that same record member for member and
//! refuses a name the store does not hold with the operation's modelled not-found error
//! instead of a success. Here that means `CreateBillingGroup` records a group whose
//! `billingGroupProperties` are the ones the request carried (and none when it carried
//! none), and `DescribeBillingGroup` reports that record's name, id, ARN, properties,
//! `creationDate` metadata and `version` — stable across reads, because a read of a record
//! writes nothing. A repeat create of the same name and configuration mints no new
//! identity; the same name with a different configuration is refused
//! `ResourceAlreadyExistsException` (409) and leaves the record as it was; a name the store
//! does not hold is refused `ResourceNotFoundException` (404). Because the group now
//! EXISTS, a `CreateThing` naming it in `billingGroupName` resolves against this store and
//! the thing's reader reports the group it belongs to.
//!
//! The table spans what the rule generalises over — the form and length of a billing group
//! name within its modelled `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`, whether the create
//! carries `billingGroupProperties` at all and whether those properties carry a
//! description (absent, empty and at the modelled `length 0..2028` ceiling), whether it
//! carries tags, and the region the call is signed for — rather than one recorded request.
//!
//! A child of THIS suite rather than a second suite beside it, for the reason its siblings
//! record: a thing's `billingGroupName` is resolved by the same registry write this
//! suite's creates go through, so what a billing group is and what the registry does with
//! one are compared against one emulator. The `#[path]` in the parent is what keeps it one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    FIRST_BILLING_GROUP_VERSION, MAX_BILLING_GROUP_TAGS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// The one query-value percent-encoder this test crate has: `ListTagsForResource` binds its
// `resourceArn` to the query string, so an ARN's separators travel encoded exactly as an
// SDK client sends them.
use crate::describe_endpoint::percent_encode;

use super::{signed_for_iot, ACCOUNT_ID, REGION};

/// The wire error shape of a create whose name is taken by a differently configured
/// resource.
const ALREADY_EXISTS: &str = "ResourceAlreadyExistsException";
/// The wire error shape of a request naming a resource this account does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";
/// `InvalidRequestException` as this service NAMES it in `x-amzn-errortype`: the shape
/// name, a `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The longest `billingGroupDescription` the model admits: `BillingGroupDescription` is
/// `length 0..2028, pattern ^[\p{Graph}\x20]*$`
/// (`smithy_query(query="shape",shape="BillingGroupProperties")`).
const MAX_DESCRIPTION_LEN: usize = 2028;

/// The longest `billingGroupName` the model admits: `BillingGroupName` is
/// `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`
/// (`smithy_query(query="operation",operation="CreateBillingGroup")`).
const MAX_NAME_LEN: usize = 128;

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
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

    /// The `message` an SDK surfaces for a refusal.
    fn message(&self) -> Option<String> {
        self.text("message")
    }

    /// The `version` a read of the group reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
    }

    /// `billingGroupProperties.billingGroupDescription`, or `None` when either the
    /// properties or the description is absent.
    fn description(&self) -> Option<String> {
        self.body
            .get("billingGroupProperties")?
            .get("billingGroupDescription")?
            .as_str()
            .map(str::to_string)
    }

    /// `billingGroupMetadata.creationDate`, as the epoch seconds the restJson1 protocol
    /// renders a timestamp as.
    fn creation_date(&self) -> Option<f64> {
        self.body
            .get("billingGroupMetadata")?
            .get("creationDate")?
            .as_f64()
    }

    /// The reported tag set, keyed by `Tag$Key`, or `None` when the response carries no
    /// `tags` member at all.
    fn tags(&self) -> Option<BTreeMap<String, Option<String>>> {
        Some(
            self.body
                .get("tags")?
                .as_array()?
                .iter()
                .map(|tag| {
                    (
                        tag.get("Key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tag.get("Value").and_then(Value::as_str).map(str::to_string),
                    )
                })
                .collect(),
        )
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>, region: &str) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{region}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot(region))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateBillingGroup`, bound to `POST /billing-groups/{billingGroupName}`.
    async fn create_in(&self, name: &str, body: &Value, region: &str) -> Reply {
        self.call(
            "POST",
            &format!("/billing-groups/{name}"),
            Some(body),
            region,
        )
        .await
    }

    async fn create(&self, name: &str, body: &Value) -> Reply {
        self.create_in(name, body, REGION).await
    }

    /// `DescribeBillingGroup`, bound to `GET /billing-groups/{billingGroupName}`.
    async fn describe_in(&self, name: &str, region: &str) -> Reply {
        self.call("GET", &format!("/billing-groups/{name}"), None, region)
            .await
    }

    async fn describe(&self, name: &str) -> Reply {
        self.describe_in(name, REGION).await
    }

    /// `ListTagsForResource`, which binds the resource's ARN to the query string.
    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/tags?resourceArn={}", percent_encode(resource_arn)),
            None,
            REGION,
        )
        .await
    }

    /// `CreateThing`, bound to `POST /things/{thingName}`.
    async fn create_thing(&self, name: &str, body: &Value) -> Reply {
        self.call("POST", &format!("/things/{name}"), Some(body), REGION)
            .await
    }

    /// `DescribeThing`, bound to `GET /things/{thingName}`.
    async fn describe_thing(&self, name: &str) -> Reply {
        self.call("GET", &format!("/things/{name}"), None, REGION)
            .await
    }
}

/// The ARN a billing group created in `region` must carry.
fn expected_arn(region: &str, name: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:billinggroup/{name}")
}

/// Whether `id` is of the form `BillingGroupId` admits: `length 1..128, pattern
/// ^[a-zA-Z0-9\-]+$`, which the canonical hyphenated form of a v4 UUID satisfies.
fn is_modelled_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_NAME_LEN
        && id.contains('-')
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// One create request: a label, the group name, the region it is signed for, the request
/// body, and what the body's optional members resolve to once stored.
struct Case {
    label: &'static str,
    name: String,
    region: &'static str,
    body: Value,
    /// What `billingGroupProperties.billingGroupDescription` must report: the body's
    /// value, or `None` when the body carried no properties or no description.
    description: Option<String>,
    /// What `ListTagsForResource` must report for the group's ARN.
    tags: BTreeMap<String, Option<String>>,
}

/// The table every case below is built from. The rows differ along what the rule
/// generalises over: the form and length of the name, whether `billingGroupProperties` is
/// carried at all, what the description is (absent, empty, ordinary, and at the modelled
/// ceiling), whether tags are carried, and the region the call is signed for.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the shortest name the model admits, with no properties at all",
            name: "b".to_string(),
            region: REGION,
            body: json!({}),
            description: None,
            tags: BTreeMap::new(),
        },
        Case {
            label: "properties carried but naming no description",
            name: "metered-fleet".to_string(),
            region: REGION,
            body: json!({ "billingGroupProperties": {} }),
            description: None,
            tags: BTreeMap::new(),
        },
        Case {
            label: "an EMPTY description, which the modelled length admits",
            name: "Metered:Fleet_01-x".to_string(),
            region: REGION,
            body: json!({ "billingGroupProperties": { "billingGroupDescription": "" } }),
            description: Some(String::new()),
            tags: BTreeMap::new(),
        },
        Case {
            label: "a description of printable characters and spaces, beside two tags",
            name: "EU:billing_group-9".to_string(),
            region: REGION,
            body: json!({
                "billingGroupProperties": { "billingGroupDescription": "Line-haul fleet #7 (eu)" },
                "tags": [
                    { "Key": "stage", "Value": "test" },
                    { "Key": "unvalued" },
                ],
            }),
            description: Some("Line-haul fleet #7 (eu)".to_string()),
            tags: BTreeMap::from([
                ("stage".to_string(), Some("test".to_string())),
                ("unvalued".to_string(), None),
            ]),
        },
        Case {
            label: "a description at the modelled length ceiling",
            name: "ceiling-description".to_string(),
            region: REGION,
            body: json!({
                "billingGroupProperties": {
                    "billingGroupDescription": "d".repeat(MAX_DESCRIPTION_LEN),
                },
            }),
            description: Some("d".repeat(MAX_DESCRIPTION_LEN)),
            tags: BTreeMap::new(),
        },
        Case {
            label: "a name at the modelled length ceiling, signed for another region",
            name: "n".repeat(MAX_NAME_LEN),
            region: "eu-west-2",
            body: json!({
                "billingGroupProperties": { "billingGroupDescription": "at the ceiling" },
                "tags": [{ "Key": "owner", "Value": "fleet-team" }],
            }),
            description: Some("at the ceiling".to_string()),
            tags: BTreeMap::from([("owner".to_string(), Some("fleet-team".to_string()))]),
        },
    ]
}

/// RULE: `CreateBillingGroup` records the group its request describes and answers with the
/// identity it persisted; `DescribeBillingGroup` reports that record member for member,
/// and reports the same thing again on a second read.
#[tokio::test]
async fn a_created_billing_group_is_reported_by_its_reader() {
    let emulator = Emulator::new();
    let mut minted_ids: BTreeSet<String> = BTreeSet::new();

    for case in cases() {
        let Case {
            label,
            name,
            region,
            body,
            description,
            tags,
        } = case;

        let created = emulator.create_in(&name, &body, region).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected a 200 create, got {created:?}"
        );
        assert_eq!(
            created.text("billingGroupName").as_deref(),
            Some(name.as_str()),
            "case {label:?}: expected the created name {name:?}, got {created:?}"
        );
        let arn = created.text("billingGroupArn");
        assert_eq!(
            arn.as_deref(),
            Some(expected_arn(region, &name).as_str()),
            "case {label:?}: expected the minted ARN, got {arn:?}"
        );
        let id = created
            .text("billingGroupId")
            .unwrap_or_else(|| panic!("case {label:?}: the create reported no billingGroupId"));
        assert!(
            is_modelled_id(&id),
            "case {label:?}: expected an id of the modelled form, got {id:?}"
        );
        assert!(
            minted_ids.insert(id.clone()),
            "case {label:?}: expected a freshly minted id, got the already minted {id:?}"
        );

        let described = emulator.describe_in(&name, region).await;
        assert_eq!(
            described.status, 200,
            "case {label:?}: expected a 200 read of the created group, got {described:?}"
        );
        assert_eq!(
            described.text("billingGroupName").as_deref(),
            Some(name.as_str()),
            "case {label:?}: expected the read to report the name, got {described:?}"
        );
        assert_eq!(
            described.text("billingGroupId"),
            Some(id.clone()),
            "case {label:?}: expected the read to report the created id {id:?}, got {described:?}"
        );
        assert_eq!(
            described.text("billingGroupArn"),
            arn,
            "case {label:?}: expected the read to report the created ARN {arn:?}, got {described:?}"
        );
        assert_eq!(
            described.version(),
            Some(FIRST_BILLING_GROUP_VERSION),
            "case {label:?}: expected the version of a record written once, got {described:?}"
        );
        assert_eq!(
            described.description(),
            description,
            "case {label:?}: expected the stored description {description:?}, got {described:?}"
        );
        let creation_date = described.creation_date().unwrap_or_else(|| {
            panic!("case {label:?}: the read reported no billingGroupMetadata.creationDate")
        });
        assert!(
            creation_date > 0.0,
            "case {label:?}: expected a creationDate after the epoch, got {creation_date:?}"
        );

        // A read writes nothing, so a second one reports the record the first one did.
        let read_again = emulator.describe_in(&name, region).await;
        assert_eq!(
            read_again.body, described.body,
            "case {label:?}: expected the second read to report {described:?}, got {read_again:?}"
        );

        // The tags the create carried are the tags the resource holds: the tag plane reads
        // the same set out of the same record.
        let reported_tags = emulator
            .list_tags(arn.as_deref().expect("the create reported an ARN"))
            .await;
        assert_eq!(
            reported_tags.status, 200,
            "case {label:?}: expected a 200 tag read, got {reported_tags:?}"
        );
        assert_eq!(
            reported_tags.tags().unwrap_or_default(),
            tags,
            "case {label:?}: expected the created tags {tags:?}, got {reported_tags:?}"
        );
    }
}

/// RULE: the create is idempotent ON the configuration — "If this call is made multiple
/// times using the same billing group name and configuration, the call will succeed" — so
/// a repeat mints no new identity, bumps no version and leaves the stored tags alone.
#[tokio::test]
async fn a_repeat_create_of_the_same_configuration_keeps_the_first_record() {
    let emulator = Emulator::new();

    // The rows differ in what the repeated configuration IS: no properties at all,
    // properties with a description, and a repeat that differs only in the tags it
    // carries, which are metadata of the resource rather than part of its configuration.
    let rows: Vec<(&str, String, Value, Value)> = vec![
        (
            "a create carrying no properties, repeated",
            "repeat-bare".to_string(),
            json!({}),
            json!({}),
        ),
        (
            "a create carrying a description, repeated verbatim",
            "repeat:described_1".to_string(),
            json!({ "billingGroupProperties": { "billingGroupDescription": "same" } }),
            json!({ "billingGroupProperties": { "billingGroupDescription": "same" } }),
        ),
        (
            "a repeat differing only in the tags it carries",
            "repeat-tagged".to_string(),
            json!({
                "billingGroupProperties": { "billingGroupDescription": "same" },
                "tags": [{ "Key": "stage", "Value": "first" }],
            }),
            json!({
                "billingGroupProperties": { "billingGroupDescription": "same" },
                "tags": [{ "Key": "stage", "Value": "second" }],
            }),
        ),
    ];

    for (label, name, first, again) in rows {
        let created = emulator.create(&name, &first).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: expected a 200 create, got {created:?}"
        );
        let before = emulator.describe(&name).await;
        let tags_before = emulator
            .list_tags(&expected_arn(REGION, &name))
            .await
            .tags()
            .unwrap_or_default();

        let repeated = emulator.create(&name, &again).await;
        assert_eq!(
            repeated.status, 200,
            "case {label:?}: expected the repeat create to succeed, got {repeated:?}"
        );
        assert_eq!(
            repeated.body, created.body,
            "case {label:?}: expected the repeat to report {created:?}, got {repeated:?}"
        );

        let after = emulator.describe(&name).await;
        assert_eq!(
            after.body, before.body,
            "case {label:?}: expected the repeat to leave the record at {before:?}, got {after:?}"
        );
        let tags_after = emulator
            .list_tags(&expected_arn(REGION, &name))
            .await
            .tags()
            .unwrap_or_default();
        assert_eq!(
            tags_after, tags_before,
            "case {label:?}: expected the stored tags {tags_before:?}, got {tags_after:?}"
        );
    }
}

/// RULE: the same name with a DIFFERENT configuration is the family's collision — a 409
/// `ResourceAlreadyExistsException` whose sentence states the group family's fixed text
/// over the name the request carried, and the stored record is left exactly as it was.
#[tokio::test]
async fn the_same_name_with_a_different_configuration_is_refused() {
    let emulator = Emulator::new();

    // The rows differ in HOW the second configuration differs: a description added to a
    // group that had none, a description dropped, and a description replaced.
    let rows: Vec<(&str, String, Value, Value)> = vec![
        (
            "a description added where the record has none",
            "conflict-added".to_string(),
            json!({}),
            json!({ "billingGroupProperties": { "billingGroupDescription": "added" } }),
        ),
        (
            "a description dropped from a record that has one",
            "conflict_dropped".to_string(),
            json!({ "billingGroupProperties": { "billingGroupDescription": "first" } }),
            json!({}),
        ),
        (
            "a description replaced by another",
            "conflict:replaced-1".to_string(),
            json!({ "billingGroupProperties": { "billingGroupDescription": "first" } }),
            json!({ "billingGroupProperties": { "billingGroupDescription": "second" } }),
        ),
    ];

    for (label, name, first, conflicting) in rows {
        assert_eq!(
            emulator.create(&name, &first).await.status,
            200,
            "case {label:?}: the first create must succeed"
        );
        let before = emulator.describe(&name).await;

        let refused = emulator.create(&name, &conflicting).await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: expected a 409 conflict, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(ALREADY_EXISTS),
            "case {label:?}: expected {ALREADY_EXISTS}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(format!(
                "Billing Group {name} already exists in current account with different properties"
            )),
            "case {label:?}: expected this family's collision sentence, got {refused:?}"
        );
        assert!(
            refused.body.get("resourceId").is_none()
                && refused.body.get("resourceArn").is_none(),
            "case {label:?}: expected no identity members on the 409, got {refused:?}"
        );

        let after = emulator.describe(&name).await;
        assert_eq!(
            after.body, before.body,
            "case {label:?}: expected the refused create to change nothing, got {after:?}"
        );
    }
}

/// RULE: a reader of ONE billing group refuses a name the store does not hold with the
/// modelled 404 and this family's sentence, rather than reporting an empty group — and a
/// create under a name that is merely a PREFIX or a different case of a stored one is a
/// different group.
#[tokio::test]
async fn a_name_the_store_does_not_hold_is_refused_by_the_reader() {
    let emulator = Emulator::new();

    assert_eq!(
        emulator
            .create("metered-fleet", &json!({}))
            .await
            .status,
        200,
        "the group the misses are measured against must exist"
    );

    // The rows span the ways a name can miss: never created, a prefix of a stored name, a
    // name the stored one is a prefix of, and the same letters in another case.
    let misses = [
        ("a name nothing created", "absent-group"),
        ("a prefix of a stored name", "metered"),
        ("a stored name with a suffix", "metered-fleet-2"),
        ("a stored name in another case", "Metered-Fleet"),
    ];
    for (label, name) in misses {
        let refused = emulator.describe(name).await;
        assert_eq!(
            refused.status, 404,
            "case {label:?}: expected a 404 read, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {label:?}: expected {NOT_FOUND}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(format!("Billing group {name} does not exist")),
            "case {label:?}: expected this family's not-found sentence, got {refused:?}"
        );
    }
}

/// RULE: a create past a published bound records nothing, so the name is still free and
/// the reader still refuses it. `TagList` has no modelled maximum, so the ceiling on how
/// many tags ONE request may carry is this service's own
/// ([`MAX_BILLING_GROUP_TAGS`]).
#[tokio::test]
async fn a_create_carrying_more_tags_than_the_ceiling_records_nothing() {
    let emulator = Emulator::new();

    let over_the_ceiling: Vec<Value> = (0..=MAX_BILLING_GROUP_TAGS)
        .map(|index| json!({ "Key": format!("key-{index}"), "Value": "v" }))
        .collect();
    let refused = emulator
        .create("over-tagged", &json!({ "tags": over_the_ceiling }))
        .await;
    assert_eq!(
        refused.status, 400,
        "expected a 400 for a request past the tag ceiling, got {refused:?}"
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some(QUALIFIED_INVALID_REQUEST),
        "expected the modelled bad request, got {refused:?}"
    );

    let read = emulator.describe("over-tagged").await;
    assert_eq!(
        read.status, 404,
        "expected the refused create to have recorded nothing, got {read:?}"
    );

    // At the ceiling the create is accepted, so the refusal above is the bound and not the
    // tags themselves.
    let at_the_ceiling: Vec<Value> = (0..MAX_BILLING_GROUP_TAGS)
        .map(|index| json!({ "Key": format!("key-{index}"), "Value": "v" }))
        .collect();
    let accepted = emulator
        .create("at-the-ceiling", &json!({ "tags": at_the_ceiling }))
        .await;
    assert_eq!(
        accepted.status, 200,
        "expected a create at the tag ceiling to succeed, got {accepted:?}"
    );
    let reported = emulator
        .list_tags(&expected_arn(REGION, "at-the-ceiling"))
        .await;
    assert_eq!(
        reported.tags().unwrap_or_default().len(),
        MAX_BILLING_GROUP_TAGS,
        "expected every tag at the ceiling to be stored, got {reported:?}"
    );
}

/// RULE: the registry resolves a thing's `billingGroupName` against the groups this
/// account HOLDS — a group that exists is recorded on the thing and reported by its
/// reader, and a name no group answers to is refused `ResourceNotFoundException` (404)
/// with the billing-group family's sentence.
#[tokio::test]
async fn a_thing_joins_a_billing_group_that_exists_and_no_other() {
    let emulator = Emulator::new();

    // The rows differ in the form of the group name the thing names, including one that is
    // never created.
    let groups = ["metered-fleet", "Billing:Group_2-x", "b"];
    for group in groups {
        assert_eq!(
            emulator.create(group, &json!({})).await.status,
            200,
            "case {group:?}: the group the thing joins must exist"
        );
    }

    for (index, group) in groups.iter().enumerate() {
        let thing = format!("sensor-{index}");
        let created = emulator
            .create_thing(&thing, &json!({ "billingGroupName": group }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {group:?}: expected the thing to be registered, got {created:?}"
        );
        let described = emulator.describe_thing(&thing).await;
        assert_eq!(
            described.text("billingGroupName").as_deref(),
            Some(*group),
            "case {group:?}: expected the thing's reader to report its group, got {described:?}"
        );
    }

    let refused = emulator
        .create_thing(
            "sensor-unknown",
            &json!({ "billingGroupName": "no-such-group" }),
        )
        .await;
    assert_eq!(
        refused.status, 404,
        "expected a thing naming no stored group to be refused, got {refused:?}"
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some(NOT_FOUND),
        "expected {NOT_FOUND}, got {refused:?}"
    );
    assert_eq!(
        refused.message(),
        Some("Billing group no-such-group does not exist".to_string()),
        "expected the billing-group family's not-found sentence, got {refused:?}"
    );
    assert_eq!(
        emulator.describe_thing("sensor-unknown").await.status,
        404,
        "expected the refused registration to have recorded nothing"
    );
}
