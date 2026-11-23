// SPDX-License-Identifier: CC0-1.0

//! The sentences a `PutParameter` whose `Policies` document cannot be applied is refused
//! with, end to end over the front door.
//!
//! RULE under test: **a refusal's message is the sentence the SERVICE words that refusal
//! with, not the paraphrase the error shape's documentation carries — and for these classes it
//! carries only what the refusal is about: the policy-quota refusal states the published
//! maximum, the policy-conflict refusal states the policy kinds that cannot stand apart, and
//! the unsupported-type refusal names the type the request asked for.** None of them carries
//! anything else of the request: not the parameter addressed, not how many policies the
//! document held, not the attributes they carried — so a client that matches or displays the
//! text reads one sentence per class however the request that provoked it was shaped.
//!
//! The rows differ along what the rule generalises over: for the quota, the NUMBER of
//! policies the document carries (one past the maximum, several past it, far past it) and the
//! types they are; for the conflict, the SET the document forms (the notification alone, the
//! notification beside another notification, in either order, a type repeated with and without
//! the expiration) and the sets that stand beside them — a notification type repeats, because a
//! parameter may be notified about one event more than once, while the expiration that sets the
//! one time it expires does not; for the type, the TOKEN the policy declares. Every refused row
//! is also read back, because a refused put must create nothing.
//!
//! The maximum is the API's, not a per-request value: "The maximum is 10"
//! (`smithy_query(query="shape", shape="PoliciesLimitExceededException")`), which
//! [`MAX_POLICIES`] states.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// How many policies one parameter may carry: "You specified more than the maximum number of
/// allowed policies for the parameter. The maximum is 10"
/// (`smithy_query(query="shape", shape="PoliciesLimitExceededException")`).
const MAX_POLICIES: usize = 10;

/// The modelled class a document carrying more policies than that is refused as
/// (httpError 400, awsQueryError `PoliciesLimitExceededException`).
const OVER_QUOTA: &str = "PoliciesLimitExceededException";

/// The modelled class a set of policies that cannot stand together is refused as
/// (httpError 400, awsQueryError `IncompatiblePolicyException`).
const CONFLICT: &str = "IncompatiblePolicyException";

/// The sentence the service words the policy quota with: fixed text carrying the published
/// maximum and nothing of the request.
fn over_quota_message() -> String {
    format!("Number of policies attached to a parameter cannot exceed {MAX_POLICIES}")
}

/// The sentence the service words a notification with no expiration to notify about with:
/// wholly fixed, naming the two policy kinds that stand or fall together.
const MISSING_EXPIRATION: &str = "Expiration notification policies are missing expiration policy";

/// The modelled class a policy of an unsupported type is refused as (httpError 400,
/// awsQueryError `InvalidPolicyTypeException`).
const UNSUPPORTED_TYPE: &str = "InvalidPolicyTypeException";

/// The sentence that refusal carries: the type the request asked for, and nothing else of it.
fn unsupported_type_message(policy_type: &str) -> String {
    format!("Policy type {policy_type} is not supported.")
}

/// The supported policy types (`smithy_query(query="doc", shape="PutParameterRequest",
/// member="Policies")`: Expiration, ExpirationNotification, NoChangeNotification).
const EXPIRATION: &str = "Expiration";

/// See [`EXPIRATION`].
const EXPIRATION_NOTIFICATION: &str = "ExpirationNotification";

/// See [`EXPIRATION`].
const NO_CHANGE_NOTIFICATION: &str = "NoChangeNotification";

/// A `Policies` document carrying one policy per entry of `types`, each with the attributes
/// that kind of policy takes.
fn policy_document(types: &[&str]) -> String {
    json!(types
        .iter()
        .map(|policy_type| json!({
            "Type": policy_type,
            "Version": "1.0",
            "Attributes": attributes(policy_type),
        }))
        .collect::<Vec<Value>>())
    .to_string()
}

/// The `Attributes` of one policy: an expiration names the instant it comes due, and a
/// notification names how long before it fires.
fn attributes(policy_type: &str) -> Value {
    if policy_type == EXPIRATION {
        json!({ "Timestamp": "2035-01-01T00:00:00.000Z" })
    } else {
        json!({ "Before": "7", "Unit": "Days" })
    }
}

/// An advanced-tier put of `policies` against `name`.
async fn put(router: &MultiServiceRouter, name: &str, policies: &str) -> (u16, Value) {
    call(
        router,
        "PutParameter",
        &json!({
            "Name": name,
            "Type": "String",
            "Value": "never",
            "Tier": "Advanced",
            "Policies": policies,
        }),
    )
    .await
}

/// Assert the put created nothing: the name resolves to no parameter at all.
async fn created_nothing(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, read) = call(router, "GetParameter", &json!({ "Name": name })).await;
    assert_eq!(
        (status, error_code(&read)),
        (400, "ParameterNotFound".to_string()),
        "case {case:?}: a refused put must create nothing, got {status} {read}"
    );
}

/// A document carrying more policies than a parameter may hold is refused with the quota
/// sentence — the same sentence however far past the maximum the document is, and carrying
/// nothing of the request.
#[tokio::test]
async fn a_document_past_the_policy_maximum_is_refused_with_the_quota_sentence() {
    let router = router();

    // (case, how many policies the document carries)
    let over: &[(&str, usize)] = &[
        ("one policy past the maximum", MAX_POLICIES + 1),
        ("two policies past it", MAX_POLICIES + 2),
        ("far past it", MAX_POLICIES * 3),
    ];
    for (case, count) in over {
        let name = format!("/Policies/Quota/{count}");
        let types: Vec<&str> = (0..*count).map(|_| EXPIRATION_NOTIFICATION).collect();
        let (status, body) = put(&router, &name, &policy_document(&types)).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (400, OVER_QUOTA.to_string(), over_quota_message()),
            "case {case:?} ({count} policies): expected the quota refusal, got {status} {body}"
        );
        // Fixed text: neither the parameter it addressed nor the number of policies it
        // carried is in the sentence.
        let message = error_message(&body);
        assert!(
            !message.contains(&name) && !message.contains(&count.to_string()),
            "case {case:?}: the quota sentence carries nothing of the request, got {message:?}"
        );
        created_nothing(&router, case, &name).await;
    }

    // At the maximum the quota is not exceeded, so whatever else such a document is refused
    // for, it is not this: the sentence belongs to documents PAST the maximum alone.
    let name = format!("/Policies/Quota/{MAX_POLICIES}");
    let types: Vec<&str> = (0..MAX_POLICIES).map(|_| EXPIRATION_NOTIFICATION).collect();
    let (status, body) = put(&router, &name, &policy_document(&types)).await;
    assert_ne!(
        (error_code(&body), error_message(&body)),
        (OVER_QUOTA.to_string(), over_quota_message()),
        "case \"at the maximum\": {MAX_POLICIES} policies are within the quota, got {status} \
         {body}"
    );

    // A document the parameter may hold is served, so the refusals above are about the
    // quota and not about policies.
    let name = "/Policies/Quota/within";
    let (status, body) = put(
        &router,
        name,
        &policy_document(&[EXPIRATION, EXPIRATION_NOTIFICATION, NO_CHANGE_NOTIFICATION]),
    )
    .await;
    assert_eq!(
        (status, body["Version"].as_i64()),
        (200, Some(1)),
        "expected a document within the quota to be served, got {status} {body}"
    );
}

/// A document whose expiration notification has no expiration to notify about is refused with
/// the conflict sentence — the same sentence whatever else the document carries, and carrying
/// nothing of the request.
#[tokio::test]
async fn a_notification_with_no_expiration_is_refused_with_the_conflict_sentence() {
    let router = router();

    // (case, the policy types the document carries, the sentence it is refused with — `None`
    // for a set that stands)
    let rows: &[(&str, &[&str], Option<&str>)] = &[
        (
            "the notification alone",
            &[EXPIRATION_NOTIFICATION],
            Some(MISSING_EXPIRATION),
        ),
        (
            "the notification beside another notification",
            &[EXPIRATION_NOTIFICATION, NO_CHANGE_NOTIFICATION],
            Some(MISSING_EXPIRATION),
        ),
        (
            "the same set in the other order",
            &[NO_CHANGE_NOTIFICATION, EXPIRATION_NOTIFICATION],
            Some(MISSING_EXPIRATION),
        ),
        (
            // A notification with nothing to notify about can never fire, however many of
            // them the document carries.
            "the notification twice, with no expiration at all",
            &[EXPIRATION_NOTIFICATION, EXPIRATION_NOTIFICATION],
            Some(MISSING_EXPIRATION),
        ),
        (
            // With the expiration present each notification CAN fire, and a parameter may be
            // notified about one expiration more than once — several leads before it — so a
            // repeated notification is two notifications rather than one stated twice.
            "the notification twice beside its expiration",
            &[EXPIRATION, EXPIRATION_NOTIFICATION, EXPIRATION_NOTIFICATION],
            None,
        ),
        (
            "the no-change notification twice, each its own period",
            &[NO_CHANGE_NOTIFICATION, NO_CHANGE_NOTIFICATION],
            None,
        ),
        (
            // The expiration sets the ONE time the parameter expires, so a second one states
            // that time twice over.
            "the expiration twice",
            &[EXPIRATION, EXPIRATION],
            Some("There can be only one expiration policy"),
        ),
        (
            "the notification beside the expiration it is measured from",
            &[EXPIRATION, EXPIRATION_NOTIFICATION],
            None,
        ),
        ("the expiration alone", &[EXPIRATION], None),
        (
            "a no-change notification alone, which is measured from the parameter itself",
            &[NO_CHANGE_NOTIFICATION],
            None,
        ),
    ];

    for (index, (case, types, refusal)) in rows.iter().enumerate() {
        let name = format!("/Policies/Conflict/{index}");
        let (status, body) = put(&router, &name, &policy_document(types)).await;
        let Some(sentence) = refusal else {
            assert_eq!(
                (status, body["Version"].as_i64()),
                (200, Some(1)),
                "case {case:?}: {types:?} stand together, got {status} {body}"
            );
            continue;
        };
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (400, CONFLICT.to_string(), (*sentence).to_string()),
            "case {case:?}: expected the conflict refusal, got {status} {body}"
        );
        assert!(
            !error_message(&body).contains(&name),
            "case {case:?}: the conflict sentence carries nothing of the request, got {}",
            error_message(&body)
        );
        created_nothing(&router, case, &name).await;
    }
}

/// A policy of a type Parameter Store does not support is refused with the sentence naming
/// that type — the type as the request wrote it, and nothing else of the request.
#[tokio::test]
async fn a_policy_of_an_unsupported_type_is_refused_naming_the_type() {
    let router = router();

    // (case, the type the document's policy declares) — a name of no policy kind, a supported
    // kind misspelled, a supported kind in another casing (the types are the exact tokens the
    // model declares), and one carrying the prose spelling a refusal uses.
    let cases: &[(&str, &str)] = &[
        ("a type that names no policy kind", "NotAPolicyType"),
        ("a supported kind misspelled", "Expirations"),
        ("a supported kind in another casing", "expiration"),
        (
            "the prose spelling of a supported kind",
            "Expiration notification",
        ),
    ];

    for (index, (case, policy_type)) in cases.iter().enumerate() {
        let name = format!("/Policies/Type/{index}");
        let (status, body) = put(&router, &name, &policy_document(&[policy_type])).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (
                400,
                UNSUPPORTED_TYPE.to_string(),
                unsupported_type_message(policy_type)
            ),
            "case {case:?} ({policy_type}): expected the unsupported-type refusal, got {status} \
             {body}"
        );
        assert!(
            !error_message(&body).contains(&name),
            "case {case:?}: the sentence carries nothing of the request but the type, got {}",
            error_message(&body)
        );
        created_nothing(&router, case, &name).await;
    }
}
