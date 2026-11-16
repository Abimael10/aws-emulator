// SPDX-License-Identifier: CC0-1.0

//! What the TAGGING surface (`TagResource`, `UntagResource`, `ListTagsOfResource`) holds a
//! request's own members to, before it looks any resource up.
//!
//! Two rules, each driven over a table of inputs rather than over one example:
//!
//! 1. The `ResourceArn` member is that surface's whole selector, and it is resolved by the
//!    service's own table-ARN check rather than by the bounds the model declares on the
//!    member: whatever is wrong with the value — no value at all, one outside the member's
//!    declared length bound, or one that is no table ARN of the endpoint the request reached
//!    — the refusal is HTTP 400 `ValidationException` stating the ARN rule that was broken,
//!    and never the validating walk's clause about the member. The class sentence
//!    `Invalid TableArn` is the whole message when the request carries no value to name, and
//!    a value that IS named is named by the sentence that check words for it. A refusal
//!    about this member states nothing of any other member of the same request: the member
//!    the check reads is what a resource would have been addressed by. (The rest of that
//!    check's vocabulary is driven by `tests/all/required_member_refusals.rs` and
//!    `tests/all/arn_addressed_tag_refusals.rs`.)
//! 2. A required TAG LIST states the entries it asks for, so a request that states no entry
//!    of it asks for nothing and cannot be served: HTTP 400 `ValidationException` with the
//!    surface's own sentence — `Atleast one Tag needs to be provided as Input.` for a tag
//!    write, `Atleast one Tag Key needs to be provided as Input.` for a tag removal. It is
//!    the same sentence whether the request carried the list with no entries or left it out
//!    altogether, and it is an INPUT failure, so it is answered BEFORE the resource the ARN
//!    names is looked up: the same sentence whether that resource is there or not, and never
//!    a not-found. Neither refusal changes what a resource carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call, describe_when_active, emulator, error_code, message, ok, ACTIVE_DEADLINE, POLL_INTERVAL,
    REGION,
};

/// The account every ARN this emulator mints carries, and so the only account an
/// ARN-addressed request can address.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The whole message a `ResourceArn` this surface cannot read is refused with when the
/// request carries no value for it: AWS's class sentence, with no member path and nothing
/// echoed.
const INVALID_TABLE_ARN: &str = "Invalid TableArn";

/// The sentence a value that is no ARN at all earns, up to the value it names.
const NOT_AN_ARN: &str = "One or more parameter values were invalid: ARNs must start with 'arn:': ";

/// Every operation of the tagging surface, each with a body that carries `resource_arn` and
/// is otherwise valid — the members the model marks required on that input.
fn tagging_requests(resource_arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "TagResource",
            json!({"ResourceArn": resource_arn, "Tags": [{"Key": "env", "Value": "prod"}]}),
        ),
        (
            "UntagResource",
            json!({"ResourceArn": resource_arn, "TagKeys": ["env"]}),
        ),
        ("ListTagsOfResource", json!({"ResourceArn": resource_arn})),
    ]
}

/// Create one table and answer the ARN the service minted for it, once the
/// `CREATING` -> `ACTIVE` transition the clock owes it has been served.
async fn create_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, case, name).await;
    created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// An in-scope table ARN of this endpoint for a table name, whether or not a table of that
/// name exists: the form `resolve_resource_arn` accepts, so a refusal below is never about
/// the ARN's shape.
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// The tag set `ListTagsOfResource` reports for `arn`, as `Key=Value` pairs in key order.
async fn listed_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let listed = ok(router, "ListTagsOfResource", json!({"ResourceArn": arn})).await;
    let mut pairs: Vec<String> = listed["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            format!(
                "{}={}",
                tag["Key"].as_str().unwrap_or("<missing>"),
                tag["Value"].as_str().unwrap_or("<missing>")
            )
        })
        .collect();
    pairs.sort();
    assert!(
        listed["Tags"].is_array(),
        "case {case:?}: a tag listing reports a Tags list, got {listed}"
    );
    pairs
}

/// Poll the tag listing until it reports `expected` — a tag write is eventually consistent
/// ("The application or removal of tags using TagResource and UntagResource APIs is
/// eventually consistent" — the `TagResource` doc), so the set a write stored appears one
/// propagation span later. The wait is for the CONDITION and is generously bounded, so a
/// write that never becomes visible fails naming the last set it reported.
async fn tags_when_visible(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    expected: &[&str],
) -> Vec<String> {
    let started = std::time::Instant::now();
    let mut last: Vec<String> = Vec::new();
    while started.elapsed() < ACTIVE_DEADLINE {
        last = listed_tags(router, case, arn).await;
        if last == expected {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the tag set {expected:?} never became visible within \
         {ACTIVE_DEADLINE:?}, last {last:?}"
    )
}

/// The refusal `operation` answers `request` with: its code and its message, over a call
/// that must not succeed.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> (String, String) {
    let (status, answered) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected HTTP 400, got {status} with {answered}"
    );
    (error_code(&answered), message(&answered))
}

// ===========================================================================
// 1. A `ResourceArn` the surface's own ARN check cannot read
// ===========================================================================

#[tokio::test]
async fn a_tagging_request_with_no_resource_arn_is_refused_as_an_invalid_table_arn() {
    let router = emulator();
    // One real table, so these refusals are about the member and not about an empty region.
    let arn = create_table(&router, "a present table", "Addressed").await;

    // (case, operation, request). The rows span the three operations of the surface and
    // what else the same request carries: every other required member valid, several
    // entries rather than one, a member that is itself at fault, and nothing at all. The
    // ARN check runs before any of that, so each row earns the same sentence.
    //
    // Every row leaves the member OUT. A member spelled with a JSON `null` never reaches
    // this rule at all: the protocol cannot read a string from it, so that request is
    // refused while it is being deserialized.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a tag write with no resource ARN",
            "TagResource",
            json!({"Tags": [{"Key": "env", "Value": "prod"}]}),
        ),
        (
            "a tag write of several tags with no resource ARN",
            "TagResource",
            json!({"Tags": [{"Key": "env", "Value": "prod"},
                            {"Key": "team", "Value": "core"}]}),
        ),
        (
            "a tag write with neither a resource ARN nor tags",
            "TagResource",
            json!({}),
        ),
        (
            "a tag write with no resource ARN and an empty tag list",
            "TagResource",
            json!({"Tags": []}),
        ),
        (
            "a tag removal with no resource ARN",
            "UntagResource",
            json!({"TagKeys": ["env"]}),
        ),
        (
            "a tag removal of several keys with no resource ARN",
            "UntagResource",
            json!({"TagKeys": ["env", "team"]}),
        ),
        (
            "a tag removal with neither a resource ARN nor keys",
            "UntagResource",
            json!({}),
        ),
        (
            "a tag listing with no resource ARN",
            "ListTagsOfResource",
            json!({}),
        ),
        (
            "a tag listing with a page cursor and no resource ARN",
            "ListTagsOfResource",
            json!({"NextToken": "AAAAAA"}),
        ),
    ];

    for (case, operation, request) in rows {
        let (code, stated) = refusal(&router, case, operation, request).await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ValidationException", INVALID_TABLE_ARN),
            "case {case:?} ({operation}): expected the ARN class sentence alone, got {code} \
             with {stated:?}"
        );
    }

    // None of those refusals touched the resource that does exist: it carries no tags, and
    // a tag write still lands on it.
    let listed = listed_tags(&router, "a present table", &arn).await;
    assert!(
        listed.is_empty(),
        "case \"a present table\": the refused calls stored nothing, got {listed:?}"
    );
    ok(
        &router,
        "TagResource",
        json!({"ResourceArn": arn, "Tags": [{"Key": "env", "Value": "prod"}]}),
    )
    .await;
    let visible = tags_when_visible(&router, "a present table", &arn, &["env=prod"]).await;
    assert_eq!(
        visible,
        vec!["env=prod".to_string()],
        "case \"a present table\": the write that names the ARN is stored, got {visible:?}"
    );
}

#[tokio::test]
async fn a_resource_arn_outside_the_members_modelled_length_bound_is_refused_by_the_arn_rule() {
    let router = emulator();

    // The member's declared bound: `ResourceArnString` is `string @length 1..1283`
    // (`smithy_query(query="operation", operation="TagResource")`). The rows are each
    // published bound, one past each of them, and the value one BELOW the minimum is the
    // empty string — and whichever side of the bound a value falls, the rule it is held to
    // is the ARN rule, so the refusal states that one and never the member's bound.
    let longest_prefix = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/");
    let at_maximum = format!(
        "{longest_prefix}{}",
        "z".repeat(1283 - longest_prefix.len())
    );
    let past_maximum = format!("{at_maximum}z");

    // (case, the submitted value, the whole message it earns).
    let rows: Vec<(&str, String, String)> = vec![
        (
            "one below the declared minimum length",
            String::new(),
            NOT_AN_ARN.to_string(),
        ),
        (
            "at the declared minimum length",
            "x".to_string(),
            format!("{NOT_AN_ARN}x"),
        ),
        (
            "at the declared maximum length",
            at_maximum.clone(),
            format!("{INVALID_TABLE_ARN}: Invalid ResourceArn provided as input {at_maximum}"),
        ),
    ];

    for (case, value, expected) in &rows {
        for (operation, request) in tagging_requests(value) {
            let (code, stated) = refusal(&router, case, operation, request).await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                ("ValidationException", expected.as_str()),
                "case {case:?} ({operation}): expected the ARN rule's sentence, got {code} with \
                 {stated:?}"
            );
        }
    }

    // One past the maximum, where the value the refusal echoes is elided rather than
    // repeated in full: what is asserted is the rule it states.
    for (operation, request) in tagging_requests(&past_maximum) {
        let (code, stated) = refusal(
            &router,
            "one past the declared maximum length",
            operation,
            request,
        )
        .await;
        assert_eq!(
            code, "ValidationException",
            "case \"one past the declared maximum length\" ({operation}): expected a \
             ValidationException, got {code} with {stated:?}"
        );
        let names_the_rule =
            format!("{INVALID_TABLE_ARN}: Invalid ResourceArn provided as input arn:");
        assert!(
            stated.starts_with(&names_the_rule),
            "case \"one past the declared maximum length\" ({operation}): expected the ARN \
             rule's sentence, got {stated:?}"
        );
    }
}

// ===========================================================================
// 2. A required tag list the request states no entry in
// ===========================================================================

/// One empty-list case: the operation, the required list member the request states no entry
/// of, and the whole message that earns.
struct EmptyListCase {
    case: &'static str,
    operation: &'static str,
    member: &'static str,
    message: &'static str,
}

impl EmptyListCase {
    /// The request body: this case's list member, stated empty, against `arn`.
    fn request(&self, arn: &str) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("ResourceArn".to_string(), json!(arn));
        body.insert(self.member.to_string(), json!([]));
        Value::Object(body)
    }

    /// The same request with the list member left out altogether.
    fn without_the_list(&self, arn: &str) -> Value {
        json!({"ResourceArn": arn})
    }
}

#[tokio::test]
async fn an_empty_tag_list_is_refused_before_the_resource_is_looked_up() {
    let router = emulator();
    let present = create_table(&router, "a present table", "Listed").await;
    // An ARN of the same endpoint naming a table that is not there, so each row is driven
    // against a resource that resolves and one that does not: the sentence is the same,
    // which is what "before the lookup" means.
    let absent = table_arn("absent-table");
    // The resource starts out carrying one tag, so a refusal that reached the store would
    // show up as a change to it.
    ok(
        &router,
        "TagResource",
        json!({"ResourceArn": &present, "Tags": [{"Key": "env", "Value": "prod"}]}),
    )
    .await;
    tags_when_visible(&router, "a present table", &present, &["env=prod"]).await;

    // The rows span the two operations that carry a required tag list, each driven against
    // the two resources above.
    let empty_rows: [EmptyListCase; 2] = [
        EmptyListCase {
            case: "a tag write whose tag list is empty",
            operation: "TagResource",
            member: "Tags",
            message: "Atleast one Tag needs to be provided as Input.",
        },
        EmptyListCase {
            case: "a tag removal whose key list is empty",
            operation: "UntagResource",
            member: "TagKeys",
            message: "Atleast one Tag Key needs to be provided as Input.",
        },
    ];

    for row in &empty_rows {
        // Both ways a request states no entry of the list — the list carried with no
        // entries, and the list left out of the request altogether — earn the same
        // sentence, against a resource that resolves and one that does not.
        for (resource, arn) in [
            ("a table that is there", &present),
            ("a table that is not", &absent),
        ] {
            for (how, request) in [
                ("an empty list", row.request(arn)),
                ("no list at all", row.without_the_list(arn)),
            ] {
                let case = format!("{} as {how} against {resource}", row.case);
                let (code, stated) = refusal(&router, &case, row.operation, request).await;
                assert_eq!(
                    (code.as_str(), stated.as_str()),
                    ("ValidationException", row.message),
                    "case {case:?} ({}): expected the empty-list sentence, got {code} with \
                     {stated:?}",
                    row.operation
                );
            }
        }
    }

    // And a list that DOES state an entry reaches the lookup, where a resource that is not
    // there is reported as the member the request addressed it by.
    for (case, operation, request) in [
        (
            "a tag write naming a table that is not there",
            "TagResource",
            json!({"ResourceArn": &absent, "Tags": [{"Key": "env", "Value": "prod"}]}),
        ),
        (
            "a tag removal naming a table that is not there",
            "UntagResource",
            json!({"ResourceArn": &absent, "TagKeys": ["env"]}),
        ),
    ] {
        let (code, stated) = refusal(&router, case, operation, request).await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            (
                "ResourceNotFoundException",
                format!("Requested resource not found: ResourceArn: {absent} not found").as_str()
            ),
            "case {case:?} ({operation}): expected the lookup refusal, got {code} with {stated:?}"
        );
    }

    // Nothing above changed what the resource carries.
    let listed = listed_tags(&router, "a present table", &present).await;
    assert_eq!(
        listed,
        vec!["env=prod".to_string()],
        "case \"a present table\": every refusal left the stored set alone, got {listed:?}"
    );
}
