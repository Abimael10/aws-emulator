// SPDX-License-Identifier: CC0-1.0

//! The ONE assertion of a tag-surface refusal in this crate, driven through an operation of
//! each suite that used to state its own copy of it.
//!
//! The rule: a refusal of this service's tagging surface (`/tags/{resourceArn}`) IS its HTTP
//! status, its wire error code and its message — all three, read off the reply the one way
//! (`harness::Reply::refused`, built from `harness::Reply::error` and
//! `harness::Reply::message`) — and on an operation that declares no error for a code, the
//! refusal must not name that code either (`harness::Reply::refused_not_naming`, over
//! `harness::UNMODELLED_CODE`). The suites that used to carry a private copy of that
//! assertion now read it from here, which is what this suite pins: it drives the shared
//! path through an operation each of those suites exercises — `resource_tags.rs` and
//! `tag_not_found_status.rs` (the absent resource and the label outside its grammar, on
//! `TagResource`, `ListTagsForResource` and `UntagResource`),
//! `tag_plane_authorization.rs` (the denial of another account's ARN and the write naming no
//! pair), `tag_api_refusal_code.rs` (the absent required request parameter) and
//! `tag_selection_limit.rs` (the selection past the modelled maximum, in the removal's key
//! list, the write's tag map and the create's) — and asserts every one of them through the
//! shared assertion.
//!
//! The rows differ along what the rule generalises over: the class of refusal (absent
//! resource, label outside the modelled grammar, unauthorized account, empty tag map,
//! absent required request parameter, selection past the maximum), the HTTP status and code
//! that class carries (403, 404 and 400; three distinct codes), the operation and HTTP
//! method it is reached through (`PUT`, `GET`, `DELETE` and the create's `POST`), and the
//! size of the payload the refused request carried.
//!
//! The suites above own what each class of refusal IS; what this one adds is that the three
//! parts of any of them are read off a reply in one place — pinned both over the wire and,
//! for the assertion itself, against replies that differ from a refusal in exactly one of
//! its three parts.

use serde_json::{json, Map, Value};

use crate::harness::{
    harness, list_tags, provider_arn_for, register, reported_tags_of, seed_provider, tag, untag,
    Harness, Reply, ACCOUNT, RED_HAT, UNMODELLED_CODE,
};

/// A Secrets Manager ARN satisfying the modelled `SecretArn` pattern, distinct per label.
fn secret_arn(label: &str) -> String {
    format!("arn:aws:secretsmanager:us-west-2:{ACCOUNT}:secret:rhel-token-{label}")
}

/// The tag set the live resource carries throughout, so the refusals can be shown to have
/// left it alone.
const STORED: &[(&str, &str)] = &[("env", "prod"), ("owner", "platform")];

/// The refusal the surface states for a `resourceArn` addressing no resource of this
/// account (`resource_tags.rs`, `tag_not_found_status.rs`).
const ABSENT: (u16, &str, &str) = (404, "NotFoundException", "Resource was not found");

/// The refusal the surface states for a `resourceArn` outside the label's modelled grammar
/// (`tag_not_found_status.rs`, `tag_api_refusal_code.rs`).
const BAD_ARN: (u16, &str, &str) = (
    400,
    "BadRequestException",
    "Invalid input resource arn: bad syntax",
);

/// The refusal `TagResource` states for a tag map naming no pair
/// (`tag_plane_authorization.rs`).
const EMPTY_TAGS: (u16, &str, &str) = (
    400,
    "BadRequestException",
    "Invalid request input, expected non-empty tags map",
);

/// The refusal `UntagResource` states for a request carrying none of the required request
/// parameters (`tag_api_refusal_code.rs`).
const MISSING_TAG_KEYS: (u16, &str, &str) = (
    400,
    "BadRequestException",
    "Missing required request parameters: [tagKeys]",
);

/// The refusal the surface states for a selection past the modelled tag maximum
/// (`tag_selection_limit.rs`).
const TAG_LIMIT: (u16, &str, &str) = (
    400,
    "BadRequestException",
    "The resultant tag set must not have more than 50 user tags.",
);

/// The status and code the surface denies a request naming another account's resource with
/// (`tag_plane_authorization.rs`); its sentence names the calling principal, so it is built
/// at run time.
const DENIED: (u16, &str) = (403, "AccessDeniedException");

/// The most tags one request may select: the `@length` maximum every member a tag selection
/// travels in carries (`smithy_query(query="operation",operation="UntagResource")` reports
/// `tagKeys` as `length {min: 0, max: 50}`).
const MAX_TAG_SELECTION: usize = 50;

/// An account id other than the one this emulator serves: twelve decimal digits, which is
/// what an ARN's account field holds.
const FOREIGN_ACCOUNT: &str = "123456789012";

/// The sentence the denial carries: the IAM denial template for a resource the caller may
/// not reach at all, over the principal every request to this emulator speaks for — read
/// from the shared definition of that identity rather than written out here.
fn denial_message() -> String {
    format!(
        "User: {} is not authorized to access this resource",
        service_common::caller_principal::ambient_user_arn()
    )
}

/// `count` distinct generated keys.
fn keys(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("key-{index:03}")).collect()
}

/// `count` distinct generated pairs, as the JSON tag map a write or a create carries.
fn tag_map(count: usize) -> Value {
    Value::Object(
        keys(count)
            .into_iter()
            .map(|key| {
                let value = Value::String(format!("value-of-{key}"));
                (key, value)
            })
            .collect::<Map<String, Value>>(),
    )
}

/// One row: the reply a request was answered with, the refusal it must be, and whether the
/// operation it travelled through declares no error for [`UNMODELLED_CODE`].
struct Row {
    case: String,
    reply: Reply,
    refusal: (u16, String, String),
    unmodelled_code_forbidden: bool,
}

fn row(case: &str, reply: Reply, refusal: (u16, &str, &str), through_untag: bool) -> Row {
    let (status, code, message) = refusal;
    Row {
        case: case.to_string(),
        reply,
        refusal: (status, code.to_string(), message.to_string()),
        unmodelled_code_forbidden: through_untag,
    }
}

/// The rows the former copies' operations answer, each with the refusal its own suite owns.
async fn rows(h: &Harness, live: &str) -> Vec<Row> {
    let absent = provider_arn_for("55555555-5555-4555-8555-555555555555");
    let malformed = "not-an-arn-at-all";
    let foreign = live.replace(ACCOUNT, FOREIGN_ACCOUNT);
    let denial = denial_message();
    let denied = (DENIED.0, DENIED.1, denial.as_str());
    let one_key = ["env"];
    let over_limit_keys = keys(MAX_TAG_SELECTION + 1);
    let over_limit_selection: Vec<&str> = over_limit_keys.iter().map(String::as_str).collect();

    let mut rows = Vec::new();

    // `resource_tags.rs` / `tag_not_found_status.rs`: an ARN inside the modelled grammar
    // addressing no resource of this account, on all three operations of the surface.
    rows.push(row(
        "TagResource on an ARN addressing no resource",
        tag(h, &absent, &tag_map(1)).await,
        ABSENT,
        false,
    ));
    rows.push(row(
        "ListTagsForResource on an ARN addressing no resource",
        list_tags(h, &absent).await,
        ABSENT,
        false,
    ));
    rows.push(row(
        "UntagResource on an ARN addressing no resource",
        untag(h, &absent, &one_key).await,
        ABSENT,
        true,
    ));

    // `tag_not_found_status.rs`: a label outside the modelled grammar, on all three.
    rows.push(row(
        "TagResource with a resourceArn outside the modelled grammar",
        tag(h, malformed, &tag_map(2)).await,
        BAD_ARN,
        false,
    ));
    rows.push(row(
        "ListTagsForResource with a resourceArn outside the modelled grammar",
        list_tags(h, malformed).await,
        BAD_ARN,
        false,
    ));
    rows.push(row(
        "UntagResource with a resourceArn outside the modelled grammar",
        untag(h, malformed, &one_key).await,
        BAD_ARN,
        true,
    ));

    // `tag_plane_authorization.rs`: another account's ARN, on all three, and the write
    // naming no pair.
    rows.push(row(
        "TagResource on an ARN in another account",
        tag(h, &foreign, &tag_map(1)).await,
        denied,
        false,
    ));
    rows.push(row(
        "ListTagsForResource on an ARN in another account",
        list_tags(h, &foreign).await,
        denied,
        false,
    ));
    rows.push(row(
        "UntagResource on an ARN in another account",
        untag(h, &foreign, &one_key).await,
        denied,
        true,
    ));
    rows.push(row(
        "TagResource with an empty tags map",
        tag(h, live, &json!({})).await,
        EMPTY_TAGS,
        false,
    ));

    // `tag_api_refusal_code.rs`: the required request parameter the removal does not carry.
    rows.push(row(
        "UntagResource carrying no tagKeys parameter",
        untag(h, live, &[]).await,
        MISSING_TAG_KEYS,
        true,
    ));

    // `tag_selection_limit.rs`: a selection past the modelled maximum, in the removal's key
    // list, the write's tag map and the create's.
    rows.push(row(
        "UntagResource naming one key past the modelled maximum",
        untag(h, live, &over_limit_selection).await,
        TAG_LIMIT,
        true,
    ));
    rows.push(row(
        "TagResource with one pair past the modelled maximum",
        tag(h, live, &tag_map(MAX_TAG_SELECTION + 1)).await,
        TAG_LIMIT,
        false,
    ));
    rows.push(row(
        "RegisterSubscriptionProvider with one tag past the modelled maximum",
        register(
            h,
            &json!({
                "SubscriptionProviderSource": RED_HAT,
                "SecretArn": secret_arn("over-limit"),
                "Tags": tag_map(MAX_TAG_SELECTION + 1),
            }),
        )
        .await,
        TAG_LIMIT,
        false,
    ));

    rows
}

/// The rule over the wire: every class of refusal this surface answers, through an
/// operation of each suite that used to hold its own copy of the assertion, is read off the
/// reply by the shared one.
#[tokio::test]
async fn every_tag_refusal_is_read_through_the_shared_assertion() {
    let h = harness();
    let live = seed_provider(
        &h,
        "00000000-0000-4000-8000-00000000d001",
        &secret_arn("live"),
        STORED,
    );

    for row in rows(&h, &live).await {
        let (status, code, message) = (row.refusal.0, row.refusal.1, row.refusal.2);
        let refusal = (status, code.as_str(), message.as_str());
        if row.unmodelled_code_forbidden {
            row.reply
                .refused_not_naming(&row.case, refusal, UNMODELLED_CODE);
        } else {
            row.reply.refused(&row.case, refusal);
        }
    }

    // Every row above was refused, so the resource the rows addressed still carries exactly
    // the set it was seeded with.
    let case = "the live resource after the refusals";
    let left = reported_tags_of(&h, &live, case).await;
    let want: std::collections::BTreeMap<String, String> = STORED
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    assert_eq!(
        left, want,
        "case {case:?}: expected the seeded set {want:?}, got {left:?}"
    );
}

/// A reply as the harness reads one off the wire: the wire error code travels as the
/// `x-amzn-errortype` header, whose value carries the shape id the code is the suffix of.
fn answered(status: u16, code: &str, message: Option<&str>) -> Reply {
    Reply {
        status,
        error_type: Some(format!(
            "com.amazonaws.licensemanagerlinuxsubscriptions#{code}"
        )),
        body: match message {
            Some(message) => json!({ "message": message }),
            None => Value::Null,
        },
    }
}

/// The assertion itself, independently of any handler: it holds a reply to ALL THREE parts
/// of the refusal, so a reply differing in exactly one of them does not pass as that
/// refusal. A single-part assertion would satisfy the wire rows above but not this.
///
/// The rows differ along which part differs — the status, the code, the message, and a
/// reply carrying no message member at all — with the matching reply as the control.
#[test]
fn the_shared_assertion_holds_a_reply_to_all_three_parts_of_a_refusal() {
    let (status, code, message) = ABSENT;
    // (what the reply differs in, the reply, whether the shared assertion must hold)
    let rows = vec![
        ("nothing", answered(status, code, Some(message)), true),
        ("the status", answered(400, code, Some(message)), false),
        (
            "the code",
            answered(status, "BadRequestException", Some(message)),
            false,
        ),
        (
            "the message",
            answered(status, code, Some("Resource not found")),
            false,
        ),
        ("the absence of a message", answered(status, code, None), false),
    ];

    for (differs, reply, holds) in rows {
        let case = format!("a reply differing from the refusal in {differs}");
        let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reply.refused(&case, ABSENT);
        }))
        .is_ok();
        assert_eq!(
            held, holds,
            "case {case:?}: expected the shared assertion to hold: {holds}, got {held}"
        );
    }
}

/// The second form of the assertion adds one part: a refusal of an operation that declares
/// no error for a code must not name that code, even when its status, code and message are
/// the ones expected.
#[test]
fn the_shared_assertion_refuses_a_reply_naming_an_unmodelled_code() {
    let (status, _, message) = TAG_LIMIT;
    let case = "a refusal answering the code the operation does not model";
    let reply = answered(status, UNMODELLED_CODE, Some(message));
    let refusal = (status, UNMODELLED_CODE, message);
    // Read as a plain refusal, this reply IS that status, code and message.
    reply.refused(case, refusal);
    // Read as a refusal of the operation that declares no error for the code, it is not.
    let held = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reply.refused_not_naming(case, refusal, UNMODELLED_CODE);
    }))
    .is_ok();
    assert!(
        !held,
        "case {case:?}: the shared assertion must not accept a refusal naming \
         {UNMODELLED_CODE}, got error type {:?}",
        reply.error_type
    );
}
