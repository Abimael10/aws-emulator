// SPDX-License-Identifier: CC0-1.0

//! The tag family — `TagResource`, `UntagResource` and `ListTagsForResource` — driven
//! end-to-end over this service's own `restJson1` wire through the parent suite's emulator.
//!
//! Three rules, each asserted over a table that spans the class rather than one recorded
//! shape:
//!
//! 1. ORDERING. These operations address their resource by an ARN the CALLER supplies, so the
//!    caller is authorized against that value as supplied BEFORE the member's modelled
//!    constraints are checked: a value that is not an ARN names no resource any policy
//!    statement could have granted, and is answered `AccessDeniedException` (HTTP 403) naming
//!    the calling principal, the IAM action and the value verbatim — never the
//!    `ValidationException` (400) the member's `@pattern` or `@length` would produce, and
//!    never a refusal of another member the request also got wrong.
//! 2. CONTENT. A value that IS an ARN is past authorization, so what it names is then the
//!    question: one of this service's own resources is looked up — reported, or refused
//!    `ResourceNotFoundException` (404) with this service's own not-found wording — and an
//!    ARN addressing no resource of this service is refused `ValidationException` (400)
//!    stating the value back.
//! 3. ROUND TRIP. A tag written through any resource kind's ARN is reported by the next
//!    `ListTagsForResource` of that ARN, an untag removes exactly the keys it names, and a
//!    resource holds at most the tag set its own output shape can carry.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{Answer, Emulator, ACCOUNT_ID, REGION, SIGNING_NAME};

/// The three operations of the family, with the route each is reached by and the other
/// members a well-formed request of it carries.
///
/// Every one declares a `@required` `resourceArn` and nothing else that names a resource
/// (`smithy_query(query="operation",operation="TagResource,UntagResource,\
/// ListTagsForResource")`), which is the class rule 1 and rule 2 are about.
struct TagRoute {
    operation: &'static str,
    path: &'static str,
    /// The request body for `resource_arn`, carrying this operation's other required members.
    body: fn(&str) -> Value,
}

fn tag_routes() -> Vec<TagRoute> {
    vec![
        TagRoute {
            operation: "TagResource",
            path: "/tag-resource",
            body: |arn| json!({ "resourceArn": arn, "tags": { "owner": "dns-team" } }),
        },
        TagRoute {
            operation: "UntagResource",
            path: "/untag-resource",
            body: |arn| json!({ "resourceArn": arn, "tagKeys": ["owner"] }),
        },
        TagRoute {
            operation: "ListTagsForResource",
            path: "/get-all-tags",
            body: |arn| json!({ "resourceArn": arn }),
        },
    ]
}

/// The sentence AWS denies a caller with: the principal that signed, the IAM action it asked
/// for and the resource it named, each as the request carried them.
fn not_authorized(operation: &str, resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {SIGNING_NAME}:{operation} on resource: {resource}",
        ::service_common::caller_principal::ambient_user_arn()
    )
}

/// The tags an answer reports, ordered so two answers are comparable.
fn reported_tags(answer: &Answer) -> BTreeMap<String, String> {
    answer
        .member("tags")
        .as_object()
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A tag map as a request body carries it.
fn tag_body(pairs: &[(&str, &str)]) -> Value {
    let tags: serde_json::Map<String, Value> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), json!(value)))
        .collect();
    Value::Object(tags)
}

#[tokio::test]
async fn a_resource_that_is_not_an_arn_is_denied_before_any_member_is_validated() {
    let emulator = Emulator::new();

    // Rows: values that are not ARNs, spanning the forms a caller gets wrong — a bare word
    // (the recorded shape), a path with no fields at all, a prefix carrying too few fields, a
    // six-field value whose resource half is empty, a value whose fields are separated the
    // wrong way, and one past `ResourceArn`'s own 2048-character maximum, which proves the
    // denial precedes the `@length` constraint as well as the `@pattern` one.
    let unnamed: Vec<String> = vec![
        "not-an-arn".to_string(),
        "global-resolver/gr-000000000001".to_string(),
        "arn:aws:route53globalresolver".to_string(),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:"),
        format!("arn/aws/{SIGNING_NAME}/{REGION}/{ACCOUNT_ID}/global-resolver/gr-1"),
        "x".repeat(2049),
    ];

    for resource in &unnamed {
        for route in tag_routes() {
            let case = format!("{} on {:.40?}", route.operation, resource);
            let answer = emulator
                .call("POST", route.path, Some((route.body)(resource)))
                .await;
            assert_eq!(
                answer.status, 403,
                "case {case:?}: a value naming no resource is an authorization failure, got {} \
                 with {}",
                answer.status, answer.body
            );
            assert!(
                answer.error_type.contains("AccessDeniedException"),
                "case {case:?}: expected an AccessDeniedException, got {:?} with {}",
                answer.error_type,
                answer.body
            );
            assert_eq!(
                answer.text("message"),
                not_authorized(route.operation, resource),
                "case {case:?}: expected the denial to name the principal, the action and the \
                 value as supplied, got {:?}",
                answer.text("message")
            );
        }
    }

    // The ordering holds over the OTHER members too: a request that also omits a required
    // member of its own is still answered by the denial, because authorization is decided
    // before the request is read as its operation's input.
    for route in tag_routes() {
        let case = format!("{} with the resource alone", route.operation);
        let answer = emulator
            .call(
                "POST",
                route.path,
                Some(json!({ "resourceArn": "not-an-arn" })),
            )
            .await;
        assert_eq!(
            answer.status, 403,
            "case {case:?}: expected 403, got {} with {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.text("message"),
            not_authorized(route.operation, "not-an-arn"),
            "case {case:?}: expected the denial, got {:?}",
            answer.text("message")
        );
    }

    // And a request that names NO resource at all is not denied: nothing was named, so the
    // absent `@required` member is the refusal it gets.
    for route in tag_routes() {
        let case = format!("{} with no resourceArn", route.operation);
        let answer = emulator.call("POST", route.path, Some(json!({}))).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: an unnamed resource is a missing member, not a denial, got {} with {}",
            answer.status, answer.body
        );
        assert!(
            answer.text("message").contains("resourceArn"),
            "case {case:?}: expected the refusal to name the absent member, got {:?}",
            answer.text("message")
        );
    }
}

#[tokio::test]
async fn an_arn_that_addresses_no_resource_of_this_service_is_refused_on_the_input() {
    let emulator = Emulator::new();

    // Rows: well-formed ARNs that address nothing of this service — another vendor's bucket
    // (partition-global, so its Region and account fields are empty), another vendor's role,
    // this vendor in another partition but a resource kind this service does not hold, this
    // vendor with a known kind and no id, and this vendor with the kind alone.
    let foreign = [
        "arn:aws:s3:::sim2real-bucket".to_string(),
        format!("arn:aws:iam::{ACCOUNT_ID}:role/tagger"),
        format!("arn:aws-us-gov:{SIGNING_NAME}:us-gov-west-1:{ACCOUNT_ID}:firewall-rule/fr-1"),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:global-resolver/"),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:dns-view"),
    ];

    for resource in &foreign {
        for route in tag_routes() {
            let case = format!("{} on {resource:?}", route.operation);
            let answer = emulator
                .call("POST", route.path, Some((route.body)(resource)))
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: an ARN addressing nothing of this service is a bad request, got \
                 {} with {}",
                answer.status, answer.body
            );
            assert!(
                answer.error_type.contains("ValidationException"),
                "case {case:?}: expected a ValidationException, got {:?} with {}",
                answer.error_type,
                answer.body
            );
            assert_eq!(
                answer.text("message"),
                format!("Invalid ARN format: {resource}"),
                "case {case:?}: expected the value stated back, got {:?}",
                answer.text("message")
            );
        }
    }
}

#[tokio::test]
async fn an_arn_of_this_service_naming_an_absent_resource_reaches_the_not_found_refusal() {
    let emulator = Emulator::new();

    // Rows: every resource kind this service holds, each addressed by an ARN of an id the
    // store does not hold — one shaped like an id this service mints, one of another kind's
    // shape, and a single character. The kind in the message is the API's spelling of the
    // resource the ARN's resource type named.
    let kinds = [
        ("global-resolver", "GlobalResolver"),
        ("dns-view", "DnsView"),
        ("firewall-domain-list", "FirewallDomainList"),
    ];
    let missing = ["gr-ffffffffffff", "dnsv-0000000000ff", "x"];

    for (resource_type, kind) in kinds {
        for id in missing {
            let resource =
                format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:{resource_type}/{id}");
            for route in tag_routes() {
                let case = format!("{} on {resource:?}", route.operation);
                let answer = emulator
                    .call("POST", route.path, Some((route.body)(&resource)))
                    .await;
                assert_eq!(
                    answer.status, 404,
                    "case {case:?}: an ARN of this service past authorization reaches the \
                     lookup, got {} with {}",
                    answer.status, answer.body
                );
                assert!(
                    answer.error_type.contains("ResourceNotFoundException"),
                    "case {case:?}: expected a ResourceNotFoundException, got {:?} with {}",
                    answer.error_type,
                    answer.body
                );
                assert_eq!(
                    answer.text("message"),
                    format!("[GR-ERR00309] No {kind} found with the ID: {id}"),
                    "case {case:?}: expected this service's own not-found wording, got {:?}",
                    answer.text("message")
                );
            }
        }
    }
}

#[tokio::test]
async fn tags_round_trip_through_the_arn_of_every_resource_kind_this_service_holds() {
    let emulator = Emulator::new();

    let resolver = emulator
        .create_resolver(json!({
            "name": "tagged-resolver",
            "regions": [REGION],
            "tags": { "created-with": "a-tag" }
        }))
        .await;
    assert_eq!(
        resolver.status, 200,
        "case {:?}: setup must mint a resolver, got {} with {}",
        "a tagged resolver", resolver.status, resolver.body
    );
    let resolver_id = resolver.text("id");
    let view = emulator
        .create_view(&resolver_id, json!({ "name": "tagged-view" }))
        .await;
    assert_eq!(
        view.status, 200,
        "case {:?}: setup must mint a DNS view, got {} with {}",
        "a view to tag", view.status, view.body
    );
    let list = emulator
        .call(
            "POST",
            &format!("/firewall-domain-lists/{resolver_id}"),
            Some(json!({ "name": "tagged-list" })),
        )
        .await;
    assert_eq!(
        list.status, 200,
        "case {:?}: setup must mint a firewall domain list, got {} with {}",
        "a list to tag", list.status, list.body
    );

    // Rows: one resource of every kind the store holds, addressed by the ARN its own create
    // answered, with the tags it already carries. The resolver was created WITH a tag, so the
    // list path reports a create's tags as well as a tag call's.
    let resources = [
        (
            "a Global Resolver created with a tag",
            resolver.text("arn"),
            vec![("created-with", "a-tag")],
        ),
        ("a DNS View created with none", view.text("arn"), vec![]),
        (
            "a firewall domain list created with none",
            list.text("arn"),
            vec![],
        ),
    ];

    for (case, resource, created_with) in resources {
        let initial: BTreeMap<String, String> = created_with
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let listed = emulator.list_tags(&resource).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected the tags of a stored resource to be readable, got {} with {}",
            listed.status, listed.body
        );
        assert_eq!(
            reported_tags(&listed),
            initial,
            "case {case:?}: expected the tags the create carried, got {}",
            listed.body
        );

        // A tag call ADDS the pairs it carries, keeping what the resource already held.
        let added = [
            ("owner", "dns-team"),
            ("env", "prod"),
            ("cost-centre", "4711"),
        ];
        let tagged = emulator.tag(&resource, tag_body(&added)).await;
        assert_eq!(
            tagged.status, 200,
            "case {case:?}: expected the tag call to succeed, got {} with {}",
            tagged.status, tagged.body
        );
        let mut expected = initial.clone();
        for (key, value) in added {
            expected.insert(key.to_string(), value.to_string());
        }
        let listed = emulator.list_tags(&resource).await;
        assert_eq!(
            reported_tags(&listed),
            expected,
            "case {case:?}: expected the tags just written, got {}",
            listed.body
        );

        // A second tag call REPLACES the value of a key the resource already carries and adds
        // the ones it does not.
        let updated = [("env", "staging"), ("team", "dns")];
        let tagged = emulator.tag(&resource, tag_body(&updated)).await;
        assert_eq!(
            tagged.status, 200,
            "case {case:?}: expected the second tag call to succeed, got {} with {}",
            tagged.status, tagged.body
        );
        for (key, value) in updated {
            expected.insert(key.to_string(), value.to_string());
        }
        let listed = emulator.list_tags(&resource).await;
        assert_eq!(
            reported_tags(&listed),
            expected,
            "case {case:?}: expected a repeated key to be replaced, not duplicated, got {}",
            listed.body
        );

        // An untag removes exactly the keys it names; a key the resource does not carry is an
        // absence the request asked for and already holds.
        let removed = ["env", "cost-centre", "never-set"];
        let untagged = emulator.untag(&resource, &removed).await;
        assert_eq!(
            untagged.status, 200,
            "case {case:?}: expected the untag call to succeed, got {} with {}",
            untagged.status, untagged.body
        );
        for key in removed {
            expected.remove(key);
        }
        let listed = emulator.list_tags(&resource).await;
        assert_eq!(
            reported_tags(&listed),
            expected,
            "case {case:?}: expected exactly the named keys to be gone, got {}",
            listed.body
        );

        // Tagging one resource leaves the others alone: the ARN decides which record is
        // written, so the id in it is what the write is keyed by.
        let other = emulator.list_tags(&view.text("arn")).await;
        assert_eq!(
            other.status, 200,
            "case {case:?}: expected the sibling resource to stay readable, got {} with {}",
            other.status, other.body
        );
    }

    // The resolver's own read path still reports the tags it was created with, so tagging
    // wrote to the record the rest of the service reads.
    let fetched = emulator.get_resolver(&resolver_id).await;
    assert_eq!(
        fetched.status, 200,
        "case {:?}: expected the tagged resolver to stay gettable, got {} with {}",
        "the tagged resolver", fetched.status, fetched.body
    );
}

#[tokio::test]
async fn a_resource_holds_at_most_the_tag_set_its_output_shape_can_carry() {
    let emulator = Emulator::new();
    let resolver = emulator
        .create_resolver(json!({ "name": "filled-resolver", "regions": [REGION] }))
        .await;
    assert_eq!(
        resolver.status, 200,
        "case {:?}: setup must mint a resolver, got {} with {}",
        "a resolver to fill", resolver.status, resolver.body
    );
    let resource = resolver.text("arn");

    // `Tags` is `@length(max: 50)` (`smithy_query(query="shape",shape="Tags")`), which is both
    // the most one request may carry and the most a resource can hold: a resource past it
    // could not be reported within its own output shape.
    const CEILING: usize = 50;
    let filled: Vec<(String, String)> = (0..CEILING)
        .map(|slot| (format!("key-{slot:02}"), format!("value-{slot:02}")))
        .collect();
    let pairs: Vec<(&str, &str)> = filled
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let tagged = emulator.tag(&resource, tag_body(&pairs)).await;
    assert_eq!(
        tagged.status, 200,
        "case {:?}: a request at the ceiling is accepted, got {} with {}",
        "exactly the ceiling", tagged.status, tagged.body
    );
    let listed = emulator.list_tags(&resource).await;
    assert_eq!(
        reported_tags(&listed).len(),
        CEILING,
        "case {:?}: expected {CEILING} tags, got {}",
        "exactly the ceiling",
        listed.body
    );

    // One key PAST the ceiling is refused, and nothing is written.
    let over = emulator
        .tag(&resource, tag_body(&[("key-50", "value-50")]))
        .await;
    assert_eq!(
        over.status, 402,
        "case {:?}: expected the modelled quota refusal, got {} with {}",
        "one key past the ceiling", over.status, over.body
    );
    assert!(
        over.error_type.contains("ServiceQuotaExceededException"),
        "case {:?}: expected a ServiceQuotaExceededException, got {:?} with {}",
        "one key past the ceiling",
        over.error_type,
        over.body
    );
    let listed = emulator.list_tags(&resource).await;
    assert_eq!(
        reported_tags(&listed).len(),
        CEILING,
        "case {:?}: a refused tag call writes nothing, got {}",
        "one key past the ceiling",
        listed.body
    );

    // A key the resource ALREADY carries is replaced rather than added, so re-tagging a full
    // resource is still accepted.
    let replaced = emulator
        .tag(&resource, tag_body(&[("key-00", "a-new-value")]))
        .await;
    assert_eq!(
        replaced.status, 200,
        "case {:?}: expected a replacement at the ceiling to be accepted, got {} with {}",
        "replacing a key at the ceiling", replaced.status, replaced.body
    );
    let listed = emulator.list_tags(&resource).await;
    assert_eq!(
        reported_tags(&listed).get("key-00").map(String::as_str),
        Some("a-new-value"),
        "case {:?}: expected the replacement to be reported, got {}",
        "replacing a key at the ceiling",
        listed.body
    );

    // And once a key is removed there is room for another.
    let untagged = emulator.untag(&resource, &["key-01"]).await;
    assert_eq!(
        untagged.status, 200,
        "case {:?}: expected the untag call to succeed, got {} with {}",
        "freeing a slot", untagged.status, untagged.body
    );
    let tagged = emulator
        .tag(&resource, tag_body(&[("key-50", "value-50")]))
        .await;
    assert_eq!(
        tagged.status, 200,
        "case {:?}: expected the freed slot to be usable, got {} with {}",
        "freeing a slot", tagged.status, tagged.body
    );
}
