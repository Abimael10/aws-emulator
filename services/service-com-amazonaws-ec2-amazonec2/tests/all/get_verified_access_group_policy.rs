// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetVerifiedAccessGroupPolicy`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. THE POLICY REPORT OF A GROUP IS THE POLICY THE STORE HOLDS FOR THAT GROUP: the document
//!    a writer of the group last stored, reported verbatim, beside the status of the policy —
//!    which is in effect exactly when the group holds a document for one, so a group that was
//!    given no document, or a blank one, reports no policy and the status that says so;
//! 2. the report is of the ONE group the request names: groups created side by side with
//!    different documents each report their own, and the report comes out of the store rather
//!    than out of the call that created the group;
//! 3. a group id the account holds no group for is refused HTTP 400
//!    `InvalidVerifiedAccessGroupId.NotFound` naming the id the request gave — including the
//!    id of a group this account HELD until it was deleted;
//! 4. a `DryRun` reports the outcome the request would have had — HTTP 412 `DryRunOperation`
//!    for a group that resolves, and the resolution failure for one that does not — and
//!    reports no policy either way, while `DryRun=false` is an ordinary call;
//! 5. the report is a READ: two identical calls answer identically and the store is exactly
//!    as it was — same groups, same documents, same byte charge — so nothing a later caller
//!    sees is changed by having read a policy.
//!
//! Each table row is a distinct input along what the rule generalises over — whether a
//! document was stored and what it holds (text needing XML escaping, a long document, a blank
//! one), which group of several is asked for, which unresolvable id and in which form — so no
//! row is the specification.
//!
//! The groups are created through `CreateVerifiedAccessGroup` over the same wire, so what the
//! reader answers is what a caller really stored; the instances they hang off are seeded
//! through
//! [`ec2::state::ComAmazonawsEc2Amazonec2State::insert_verified_access_instance`], the writer
//! seam `CreateVerifiedAccessInstance` lands on too — the way the group suite beside this one
//! seeds its own.

use ec2::ops::types::VerifiedAccessInstance;
use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, unescaped};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A Cedar policy of the kind a Verified Access group carries.
const CEDAR_POLICY: &str = "permit(principal, action, resource);";

/// A document whose text has to survive XML escaping on the way out: a condition carrying
/// `&&`, `<`, `>` and quotes.
const POLICY_WITH_MARKUP: &str =
    "permit(principal, action, resource) when { context.age < 65 && context.team == \"sales\" };";

/// A well-formed Verified Access group id this store holds no group for, in EC2's long id
/// form.
const UNKNOWN_GROUP_ID: &str = "vagr-0123456789abcdef0";

/// The same, in EC2's short id form — an id of the right family and the other published
/// width.
const UNKNOWN_SHORT_GROUP_ID: &str = "vagr-01234567";

/// An id of ANOTHER Verified Access resource family: it names no group however well it reads
/// as an id.
const ENDPOINT_SHAPED_ID: &str = "vae-0123456789abcdef0";

/// A long policy document: the document is the caller's text, so one far longer than a line
/// has to round-trip whole as well.
fn long_policy() -> String {
    let mut document = String::new();
    for index in 0..40 {
        document.push_str(&format!(
            "permit(principal, action, resource) when {{ context.group == \"team{index}\" }};"
        ));
    }
    document
}

/// Stand up an emulator holding two Verified Access instances a group can be created on.
fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>, Vec<String>) {
    let (router, engine) = super::ec2_query::emulator();
    let instance_ids = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        ["the sales applications", "the support applications"]
            .into_iter()
            .map(|description| {
                guard.insert_verified_access_instance(
                    VerifiedAccessInstance {
                        description: Some(description.to_string()),
                        ..VerifiedAccessInstance::default()
                    },
                    None,
                )
            })
            .collect()
    };
    (router, engine, instance_ids)
}

/// Create a group on `instance_id`, carrying `policy_document` when the case names one, and
/// answer the id it was minted under.
async fn create_group(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    policy_document: Option<&str>,
) -> String {
    let mut params: Params = vec![p("VerifiedAccessInstanceId", instance_id.to_string())];
    if let Some(document) = policy_document {
        params.push(p("PolicyDocument", document.to_string()));
    }
    let (status, body) = call(router, REGION, "CreateVerifiedAccessGroup", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group the report is about must be created, got {status} with body \
         {body}"
    );
    element(&body, "verifiedAccessGroupId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a group id: {body}"))
}

/// One `GetVerifiedAccessGroupPolicy` call for `group_id`, with `extra` as further form
/// fields.
async fn get_policy(
    router: &MultiServiceRouter,
    group_id: &str,
    extra: Params,
) -> (u16, String) {
    let mut params: Params = vec![p("VerifiedAccessGroupId", group_id.to_string())];
    params.extend(extra);
    call(router, REGION, "GetVerifiedAccessGroupPolicy", &params).await
}

/// What the store holds that a read must not disturb: how many groups, the documents they
/// hold, and the caller-chosen bytes charged against the store's budget.
fn snapshot(engine: &engine::InMemoryEngine) -> (usize, Vec<(String, Option<String>)>, usize) {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    let groups = guard
        .verified_access_groups
        .iter()
        .map(|(group_id, record)| (group_id.clone(), record.policy_document.clone()))
        .collect();
    (
        guard.verified_access_groups.len(),
        groups,
        guard.stored_bytes(),
    )
}

/// RULE 1 over the documents a group may hold: the report is that document and the status of
/// the policy it makes.
#[tokio::test]
async fn reports_the_policy_document_the_group_holds() {
    // (case, the document the create carried, the document the report must answer).
    let rows: Vec<(&str, Option<String>, Option<String>)> = vec![
        ("no document at all", None, None),
        (
            "a Cedar policy",
            Some(CEDAR_POLICY.to_string()),
            Some(CEDAR_POLICY.to_string()),
        ),
        (
            "a policy whose text has to be XML-escaped on the wire",
            Some(POLICY_WITH_MARKUP.to_string()),
            Some(POLICY_WITH_MARKUP.to_string()),
        ),
        (
            "a document far longer than a line",
            Some(long_policy()),
            Some(long_policy()),
        ),
        // A document with nothing in it is no policy: there is nothing to show and nothing in
        // effect.
        ("an empty document", Some(String::new()), None),
        ("a blank document", Some("   \t ".to_string()), None),
    ];

    for (case, stored, expected) in rows {
        let (router, _engine, instance_ids) = emulator();
        let group_id = create_group(case, &router, &instance_ids[0], stored.as_deref()).await;

        let (status, body) = get_policy(&router, &group_id, Vec::new()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = element(&body, "policyDocument").map(|text| unescaped(&text));
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stored policy document {expected:?}, got {reported:?} \
             from body {body}"
        );
        assert_eq!(
            element(&body, "policyEnabled").as_deref(),
            Some(if expected.is_some() { "true" } else { "false" }),
            "case {case:?}: the policy is in effect exactly when the group holds one, got {:?} \
             from body {body}",
            element(&body, "policyEnabled")
        );
    }
}

/// RULE 2: each group reports its OWN policy, read out of the store rather than out of the
/// create that stored it.
#[tokio::test]
async fn each_group_reports_its_own_policy() {
    let (router, _engine, instance_ids) = emulator();
    // (case, which seeded instance the group hangs off, the document it was created with).
    let rows: Vec<(&str, usize, Option<&str>)> = vec![
        ("the group with a Cedar policy", 0, Some(CEDAR_POLICY)),
        ("the group beside it, with its own", 1, Some("permit(principal, action, resource) when { context.team == \"support\" };")),
        ("the group with no policy", 1, None),
    ];

    let mut created: Vec<(&str, String, Option<&str>)> = Vec::new();
    for (case, instance, document) in rows {
        let group_id = create_group(case, &router, &instance_ids[instance], document).await;
        created.push((case, group_id, document));
    }

    for (case, group_id, document) in &created {
        let (status, body) = get_policy(&router, group_id, Vec::new()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {group_id}, got {status} with body {body}"
        );
        let reported = element(&body, "policyDocument").map(|text| unescaped(&text));
        assert_eq!(
            reported.as_deref(),
            *document,
            "case {case:?}: expected {group_id} to report its own document {document:?}, got \
             {reported:?}"
        );
    }
}

/// RULE 3: an id the account holds no group for is refused, whatever form it takes.
#[tokio::test]
async fn an_id_that_names_no_group_is_refused() {
    // (case, the id the request names).
    let rows: Vec<(&str, String)> = vec![
        ("a well-formed long id nothing minted", UNKNOWN_GROUP_ID.to_string()),
        ("the same in the short id form", UNKNOWN_SHORT_GROUP_ID.to_string()),
        ("an id of another Verified Access family", ENDPOINT_SHAPED_ID.to_string()),
        ("text that is no EC2 id at all", "not-an-id".to_string()),
        (
            "the id of the Verified Access instance the groups hang off",
            String::new(),
        ),
    ];

    for (case, mut group_id) in rows {
        let (router, engine, instance_ids) = emulator();
        // The one row that names a REAL resource of this account which is still no group: the
        // instance the groups hang off.
        if group_id.is_empty() {
            group_id = instance_ids[0].clone();
        }
        // A group this account really holds, so the refusal is about the id the request named
        // rather than about an empty store.
        create_group(case, &router, &instance_ids[0], Some(CEDAR_POLICY)).await;
        let before = snapshot(&engine);

        let (status, body) = get_policy(&router, &group_id, Vec::new()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {group_id}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVerifiedAccessGroupId.NotFound",
            "case {case:?}: expected the group's own NotFound code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("The verified-access-group ID '{group_id}' does not exist"),
            "case {case:?}: expected the refusal to name the id the request gave, got {:?}",
            error_message(&body)
        );
        assert_eq!(
            snapshot(&engine),
            before,
            "case {case:?}: a refused report must leave the store exactly as it was"
        );
    }
}

/// RULE 3, continued: a group this account HELD reports nothing once it is deleted.
#[tokio::test]
async fn a_deleted_group_has_no_policy_to_report() {
    let case = "a group deleted after its policy was readable";
    let (router, _engine, instance_ids) = emulator();
    let group_id = create_group(case, &router, &instance_ids[0], Some(CEDAR_POLICY)).await;

    let (status, body) = get_policy(&router, &group_id, Vec::new()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the policy must be readable while the group exists, got {status} with \
         body {body}"
    );

    let (status, body) = call(
        &router,
        REGION,
        "DeleteVerifiedAccessGroup",
        &[p("VerifiedAccessGroupId", group_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must succeed, got {status} with body {body}"
    );

    let (status, body) = get_policy(&router, &group_id, Vec::new()).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 once the group is gone, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidVerifiedAccessGroupId.NotFound",
        "case {case:?}: expected the group's own NotFound code, got {body}"
    );
    assert!(
        element(&body, "policyDocument").is_none(),
        "case {case:?}: a refusal reports no policy, got {body}"
    );
}

/// RULE 4: the `DryRun` flag is answered with the outcome the request would have had.
#[tokio::test]
async fn a_dry_run_reports_the_outcome_the_request_would_have_had() {
    // (case, whether the group is one this account holds, the document it holds, the status
    // and the code the flag is answered with).
    let rows: Vec<(&str, bool, Option<&str>, u16, &str)> = vec![
        (
            "a group holding a policy",
            true,
            Some(CEDAR_POLICY),
            412,
            "DryRunOperation",
        ),
        ("a group holding none", true, None, 412, "DryRunOperation"),
        (
            "an id nothing minted",
            false,
            None,
            400,
            "InvalidVerifiedAccessGroupId.NotFound",
        ),
    ];

    for (case, held, document, expected_status, expected_code) in rows {
        let (router, engine, instance_ids) = emulator();
        let group_id = create_group(case, &router, &instance_ids[0], document).await;
        let asked_for = if held {
            group_id.clone()
        } else {
            UNKNOWN_GROUP_ID.to_string()
        };
        let before = snapshot(&engine);

        let (status, body) = get_policy(&router, &asked_for, vec![p("DryRun", "true")]).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert!(
            element(&body, "policyDocument").is_none(),
            "case {case:?}: a dry run reports no policy, got {body}"
        );
        assert_eq!(
            snapshot(&engine),
            before,
            "case {case:?}: a dry run writes nothing"
        );

        // `DryRun=false` is not a dry run: it is an ordinary call, which still reports the
        // policy of a group that resolves.
        let (status, body) = get_policy(&router, &group_id, vec![p("DryRun", "false")]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DryRun=false is an ordinary call, got {status} with body {body}"
        );
        let reported = element(&body, "policyDocument").map(|text| unescaped(&text));
        assert_eq!(
            reported.as_deref(),
            document,
            "case {case:?}: expected the stored document {document:?}, got {reported:?}"
        );
    }
}

/// RULE 5: reading a policy changes nothing a later caller sees.
#[tokio::test]
async fn reading_a_policy_changes_nothing_a_later_caller_sees() {
    let case = "the same report asked for twice";
    let (router, engine, instance_ids) = emulator();
    let with_policy = create_group(case, &router, &instance_ids[0], Some(POLICY_WITH_MARKUP)).await;
    let without_policy = create_group(case, &router, &instance_ids[1], None).await;
    let before = snapshot(&engine);

    // What the two reads are compared on is the policy they report — a response's own request
    // id is per call, so it is not part of what a read answers about the group.
    let reported = |body: &str| {
        (
            element(body, "policyEnabled"),
            element(body, "policyDocument").map(|text| unescaped(&text)),
        )
    };
    let (first_status, first_body) = get_policy(&router, &with_policy, Vec::new()).await;
    let (second_status, second_body) = get_policy(&router, &with_policy, Vec::new()).await;
    assert_eq!(
        (first_status, reported(&first_body)),
        (second_status, reported(&second_body)),
        "case {case:?}: two identical reads must answer identically, got {first_body} then \
         {second_body}"
    );
    assert_eq!(
        reported(&first_body),
        (
            Some("true".to_string()),
            Some(POLICY_WITH_MARKUP.to_string())
        ),
        "case {case:?}: expected the stored policy in effect, got {first_body}"
    );
    assert_eq!(
        snapshot(&engine),
        before,
        "case {case:?}: a read must leave the store exactly as it was"
    );

    // The group beside it is untouched by the reads of the first, and still reports its own
    // (absent) policy.
    let (status, body) = get_policy(&router, &without_policy, Vec::new()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for the group beside it, got {status} with body {body}"
    );
    assert!(
        element(&body, "policyDocument").is_none(),
        "case {case:?}: the group beside it holds no policy, got {body}"
    );
    assert_eq!(
        element(&body, "policyEnabled").as_deref(),
        Some("false"),
        "case {case:?}: a group holding no document has no policy in effect, got {body}"
    );
}
