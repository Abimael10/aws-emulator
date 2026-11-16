// SPDX-License-Identifier: CC0-1.0

//! The refusal `PutBucketWebsite` answers a website configuration carrying MORE routing
//! rules than one configuration may hold, and the control arm either side of that ceiling.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire document a
//! client parses rather than any validator's internals.
//!
//! The rule under test: a `WebsiteConfiguration` carrying more routing rules than the
//! documented ceiling is refused HTTP 400 `InvalidRequest`, and that refusal's sentence is
//! the published one — the COUNT of `RoutingRules` the REQUEST carried, then the fixed text
//! naming the ceiling — while the configuration the bucket already holds is left exactly as
//! it was; a configuration at or under the ceiling is accepted and reported back rule for
//! rule in the order it was declared.
//!
//! The ceiling itself is `PutBucketWebsite`'s own documentation: "Amazon S3 has a limitation
//! of 50 routing rules per website configuration." The model declares no bound on
//! `RoutingRules` (`smithy_query(query="shape",shape="RoutingRules")`), so the ceiling and
//! its sentence are service behaviour rather than a modelled constraint.
//!
//! The rows differ along what the rule generalises over: HOW FAR over the ceiling the
//! document is (one rule, two, ten, twice the ceiling), and WHICH FORM the rules take (a
//! prefix condition with a prefix replacement, no condition at all, a condition on an HTTP
//! error code, a redirect to another host) — the count is what the refusal is about, so no
//! row's wording may depend on what the rules say. The accepted side is tested at the
//! ceiling itself, one rule under it, and at the smallest counts a configuration can carry.

use super::bucket_lifecycle::{create_bucket, router, Answer};
use super::bucket_website::{
    content, get_website, index_document, payload, put_website, reported, routing_rule,
    routing_rules,
};

/// The documented ceiling, as `PutBucketWebsite`'s own documentation states it: "Amazon S3
/// has a limitation of 50 routing rules per website configuration. If you require more than
/// 50 routing rules, you can use object redirect." Written out here, rather than read from
/// the crate, so every expectation below is against the PUBLISHED number and the published
/// sentence instead of against whatever the emulator happens to hold.
const DOCUMENTED_CEILING: usize = 50;

/// The sentence S3 publishes for a configuration carrying more routing rules than one may
/// hold: the count the request carried, then fixed text naming the ceiling.
fn too_many_routing_rules(count: usize) -> String {
    format!(
        "{count} routing rules provided, the number of routing rules in a website \
         configuration is limited to {DOCUMENTED_CEILING}."
    )
}

/// The four forms a routing rule takes across the rows: a prefix condition with a prefix
/// replacement, no condition at all, a condition on an HTTP error code, and a redirect to
/// another host. `rule` is the rule's position, so no two rules of one document are the same
/// text.
fn rule_of_form(form: usize, rule: usize) -> String {
    match form % 4 {
        0 => routing_rule(
            &format!("<Condition><KeyPrefixEquals>old{rule}/</KeyPrefixEquals></Condition>"),
            &format!("<Redirect><ReplaceKeyPrefixWith>new{rule}/</ReplaceKeyPrefixWith></Redirect>"),
        ),
        1 => routing_rule(
            "",
            &format!("<Redirect><ReplaceKeyWith>page-{rule}.html</ReplaceKeyWith></Redirect>"),
        ),
        2 => routing_rule(
            "<Condition><HttpErrorCodeReturnedEquals>404</HttpErrorCodeReturnedEquals></Condition>",
            &format!("<Redirect><ReplaceKeyWith>missing-{rule}.html</ReplaceKeyWith></Redirect>"),
        ),
        _ => routing_rule(
            &format!("<Condition><KeyPrefixEquals>docs{rule}/</KeyPrefixEquals></Condition>"),
            &format!("<Redirect><HostName>moved-{rule}.example.com</HostName></Redirect>"),
        ),
    }
}

/// A website configuration naming an index document and `count` routing rules, each of the
/// shape `form` names.
fn configuration(count: usize, form: usize) -> String {
    let rules: Vec<String> = (0..count).map(|rule| rule_of_form(form, rule)).collect();
    format!("{}{}", index_document("index.html"), routing_rules(&rules))
}

/// How many `<RoutingRule>` elements a reported document carries.
fn reported_rules(answer: &Answer) -> usize {
    String::from_utf8_lossy(&answer.body)
        .matches("<RoutingRule>")
        .count()
}

#[tokio::test]
async fn more_routing_rules_than_the_ceiling_are_refused_with_the_count_the_request_carried() {
    let router = router();
    let bucket = "website-routing-rule-ceiling";
    create_bucket(&router, bucket, None, None).await;

    // The configuration the bucket holds throughout: a full ceiling's worth of rules, which
    // every refusal below must leave exactly as it is — the refusal is whole-document, so
    // not one rule of a rejected put reaches the store.
    let held = configuration(DOCUMENTED_CEILING, 0);
    let setup = put_website(&router, bucket, &held).await;
    assert_eq!(
        setup.status, 200,
        "case {:?}: a configuration at the ceiling must be accepted, got {} with body {:?}",
        "the stored configuration at the ceiling",
        setup.status,
        String::from_utf8_lossy(&setup.body)
    );

    // (how far over the ceiling, which form the rules take): one rule over, two, ten, and
    // twice the ceiling, each written with a different shape of rule so the sentence cannot
    // be reading anything but the count.
    for (over, form) in [(1usize, 0usize), (2, 1), (10, 2), (DOCUMENTED_CEILING, 3)] {
        let count = DOCUMENTED_CEILING + over;
        let case = format!("{count} routing rules of form {form}");
        let refused = put_website(&router, bucket, &configuration(count, form)).await;

        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequest",
            "case {case:?}: expected error code {:?}, got {:?} with body {:?}",
            "InvalidRequest",
            refused.error_code(),
            String::from_utf8_lossy(&refused.body)
        );
        let expected = too_many_routing_rules(count);
        assert_eq!(
            refused.error_message(),
            expected,
            "case {case:?}: expected the published sentence {expected:?}, got {:?}",
            refused.error_message()
        );
        // The refusal is about the document as a whole rather than about one member of it,
        // so it names no argument.
        assert_eq!(
            refused.element("ArgumentName"),
            None,
            "case {case:?}: a whole-document refusal names no argument, got body {:?}",
            String::from_utf8_lossy(&refused.body)
        );

        let read = get_website(&router, bucket).await;
        assert_eq!(
            reported(&read),
            content(&payload(&held)),
            "case {case:?}: a refused put must leave the stored configuration untouched, got \
             body {:?}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            reported_rules(&read),
            DOCUMENTED_CEILING,
            "case {case:?}: expected the {DOCUMENTED_CEILING} stored rules to survive the \
             refusal, got {}",
            reported_rules(&read)
        );
    }
}

#[tokio::test]
async fn routing_rules_at_or_under_the_ceiling_are_stored_and_reported_in_order() {
    let router = router();

    // The other side of the same ceiling: the counts a configuration may carry, at the
    // ceiling itself, one under it, and the smallest counts there are — each read back rule
    // for rule in the order it was declared ([`content`] is sensitive to the position of a
    // repeated sibling).
    for (index, (count, form)) in [
        (1usize, 0usize),
        (2, 1),
        (DOCUMENTED_CEILING / 2, 2),
        (DOCUMENTED_CEILING - 1, 3),
        (DOCUMENTED_CEILING, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("{count} routing rules of form {form}");
        let bucket = format!("website-under-the-ceiling-{index}");
        create_bucket(&router, &bucket, None, None).await;

        let accepted = configuration(count, form);
        let put = put_website(&router, &bucket, &accepted).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: expected the modelled 200, got {} with body {:?}",
            put.status,
            String::from_utf8_lossy(&put.body)
        );

        let read = get_website(&router, &bucket).await;
        assert_eq!(
            reported(&read),
            content(&payload(&accepted)),
            "case {case:?}: expected the configuration back unchanged and in declared order, \
             got body {:?}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            reported_rules(&read),
            count,
            "case {case:?}: expected {count} rules reported, got {}",
            reported_rules(&read)
        );
    }
}
