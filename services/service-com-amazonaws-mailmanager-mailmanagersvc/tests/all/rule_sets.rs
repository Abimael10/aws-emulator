// SPDX-License-Identifier: CC0-1.0

//! The rule-set lifecycle (`CreateRuleSet`, `GetRuleSet`), end to end over the wire.
//!
//! Every assertion here goes through the real request path — `MultiServiceRouter::dispatch`
//! with `awsJson1_0` bodies, `POST /` and `X-Amz-Target: MailManagerSvc.<Operation>` — so
//! what it pins is the emulator's HTTP behaviour, not an internal helper.
//!
//! The tests are tables: each row is one input drawn from the class the rule covers
//! (several `RuleSetName` shapes across the modelled pattern and both length bounds,
//! several `Rules` lists from the empty one to the longest the modelled 0..40 length
//! admits, several tag sets, the idempotency contract, and the rule-set quota).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_mailmanager_mailmanagersvc::state::MAX_RULE_SETS;
use service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for; the ARNs the service returns must carry it.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// One emulator holding one MailManager service on one shared engine.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, and the fixed test credential
        // below is not this process's signing key.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation and return `(http status, parsed body)`.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("MailManagerSvc.{operation}"))
            // A signed SDK request routes by its SigV4 credential scope, whose 4th field
            // is this service's signing name (`ses`) and whose 3rd is the region the
            // response's ARNs must use.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/{REGION}/ses/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid HTTP request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            json!({})
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create a rule set and return its `RuleSetId`, failing the test on any refusal.
    async fn create(&self, body: Value) -> String {
        let (status, out) = self.call("CreateRuleSet", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {body:?}: expected CreateRuleSet to answer 200, got {status} {out:?}"
        );
        out["RuleSetId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {body:?}: CreateRuleSet returned no RuleSetId: {out:?}"))
            .to_string()
    }
}

/// The wire error code of a failure body: the `__type` member, without its namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// A minted `RuleSetId` satisfies `RuleSetId`'s modelled constraint: length 1..100
/// (`smithy_query(query="shape",shape="RuleSetId")` declares no pattern).
fn assert_well_formed_id(case: &str, id: &str) {
    assert!(
        (1..=100).contains(&id.len()),
        "case {case:?}: RuleSetId must be 1..100 characters, got {id:?} of {} characters",
        id.len()
    );
}

/// The `Rules` lists the round-trip table spans, each with the case it is named by.
///
/// They walk the shape of a `Rule` as the model declares it
/// (`smithy_query(query="shape",shape="Rule")`): the empty list at the bottom of `Rules`'
/// modelled 0..40 length, a rule carrying only its **required** `Actions`, a named rule
/// with `Conditions`, one with `Unless` conditions, one carrying several actions, and the
/// longest list the bound admits.
fn rule_lists() -> Vec<(String, Value)> {
    let mut lists: Vec<(String, Value)> = vec![
        ("no rules at all, the bottom of Rules' 0..40 length".to_string(), json!([])),
        (
            "one unnamed rule with only the required Actions".to_string(),
            json!([{ "Actions": [{ "Drop": {} }] }]),
        ),
        (
            "a named rule with a string condition".to_string(),
            json!([{
                "Name": "match-recipient",
                "Conditions": [{
                    "StringExpression": {
                        "Evaluate": { "Attribute": "RECIPIENT" },
                        "Operator": "EQUALS",
                        "Values": ["someone@example.com", "other@example.com"],
                    }
                }],
                "Actions": [{ "AddHeader": { "HeaderName": "X-Marked", "HeaderValue": "yes" } }],
            }]),
        ),
        (
            "a rule whose conditions are empty and whose Unless list is not".to_string(),
            json!([{
                "Name": "unless-only",
                "Conditions": [],
                "Unless": [{
                    "StringExpression": {
                        "Evaluate": { "MimeHeaderAttribute": "X-Internal" },
                        "Operator": "STARTS_WITH",
                        "Values": ["yes"],
                    }
                }],
                "Actions": [{ "Archive": { "TargetArchive": "a-1234", "ActionFailurePolicy": "CONTINUE" } }],
            }]),
        ),
        (
            "two rules, the first carrying several actions".to_string(),
            json!([
                {
                    "Name": "first",
                    "Actions": [
                        { "AddHeader": { "HeaderName": "X-One", "HeaderValue": "1" } },
                        { "AddHeader": { "HeaderName": "X-Two", "HeaderValue": "2" } },
                        { "Drop": {} },
                    ],
                },
                { "Name": "second", "Actions": [{ "Drop": {} }] },
            ]),
        ),
    ];

    // The top of `Rules`' modelled 0..40 length: 40 rules, each named for its position so
    // the stored ORDER is observable in the read-back.
    let longest: Vec<Value> = (0..40)
        .map(|index| json!({ "Name": format!("rule-{index:02}"), "Actions": [{ "Drop": {} }] }))
        .collect();
    lists.push((
        "40 rules, the top of Rules' 0..40 length".to_string(),
        Value::Array(longest),
    ));
    lists
}

/// Every created rule set is readable back, exactly as it was stored, whatever the shape of
/// its name, whatever rules it carries and whatever tags it carries.
///
/// The names span `RuleSetName`'s modelled pattern `^[a-zA-Z0-9_.-]+$` and its length
/// bounds 1..100 (`smithy_query(query="shape",shape="RuleSetName")`): the shortest name the
/// pattern admits, each punctuation character it allows, digits only, mixed case, and the
/// longest name it admits.
#[tokio::test]
async fn create_then_get_round_trips_name_rules_and_tags() {
    let harness = Harness::new();

    let names: [String; 6] = [
        "a".to_string(),
        "with_underscore".to_string(),
        "with.dots.in.it".to_string(),
        "with-hyphens-too".to_string(),
        "0123456789".to_string(),
        "x".repeat(100),
    ];
    // Rows also vary the tag set, which the record stores: none at all, one pair, and
    // several.
    let tag_sets: [Value; 3] = [
        Value::Null,
        json!([{ "Key": "team", "Value": "mail" }]),
        json!([
            { "Key": "team", "Value": "mail" },
            { "Key": "env", "Value": "test" },
            { "Key": "empty-value", "Value": "" },
        ]),
    ];

    let rules = rule_lists();
    let mut minted: Vec<String> = Vec::new();
    for (index, (rules_case, rule_list)) in rules.iter().enumerate() {
        let name = &names[index % names.len()];
        let tags = &tag_sets[index % tag_sets.len()];
        let mut body = json!({ "RuleSetName": name, "Rules": rule_list.clone() });
        if !tags.is_null() {
            body["Tags"] = tags.clone();
        }
        let case = format!("name {name:?} with {rules_case} and tags {tags:?}");

        let rule_set_id = harness.create(body).await;
        assert_well_formed_id(&case, &rule_set_id);
        minted.push(rule_set_id.clone());

        let (status, out) = harness
            .call("GetRuleSet", json!({ "RuleSetId": rule_set_id }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRuleSet to answer 200, got {status} {out:?}"
        );
        assert_eq!(
            out["RuleSetId"].as_str(),
            Some(rule_set_id.as_str()),
            "case {case:?}: GetRuleSet must report the id the create minted, got {out:?}"
        );
        assert_eq!(
            out["RuleSetName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the stored name must be reported verbatim, got {out:?}"
        );
        assert_eq!(
            out["RuleSetArn"].as_str(),
            Some(format!("arn:aws:ses:{REGION}:{ACCOUNT}:mailmanager-rule-set/{rule_set_id}").as_str()),
            "case {case:?}: unexpected RuleSetArn, got {out:?}"
        );
        assert_eq!(
            &out["Rules"], rule_list,
            "case {case:?}: the rules must be reported as submitted, in the submitted order, got {out:?}"
        );

        let created = out["CreatedDate"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: GetRuleSet must report a CreatedDate, got {out:?}")
        });
        let modified = out["LastModificationDate"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: GetRuleSet must report a LastModificationDate, got {out:?}")
        });
        assert!(
            created > 0.0,
            "case {case:?}: CreatedDate must be an instant, got {created}"
        );
        assert_eq!(
            created, modified,
            "case {case:?}: a rule set nothing has modified reports its creation instant as its \
             LastModificationDate, got {created} and {modified}"
        );

        // A second read of the same rule set answers the same thing: the read changed
        // nothing.
        let (again_status, again) = harness
            .call("GetRuleSet", json!({ "RuleSetId": rule_set_id }))
            .await;
        assert_eq!(
            again_status, 200,
            "case {case:?}: expected the second GetRuleSet to answer 200, got {again_status} \
             {again:?}"
        );
        assert_eq!(
            again, out,
            "case {case:?}: two reads of one rule set must report the same thing"
        );
    }

    minted.sort();
    let distinct = {
        let mut ids = minted.clone();
        ids.dedup();
        ids.len()
    };
    assert_eq!(
        distinct,
        rules.len(),
        "every create must mint its own RuleSetId, got {minted:?}"
    );
}

/// Two rule sets created with the SAME name are two rule sets: a rule set is addressed by
/// the `RuleSetId` the create minted, and the model documents `RuleSetName` only as "a
/// user-friendly name for the rule set" with no uniqueness of its own.
#[tokio::test]
async fn the_same_name_can_name_two_rule_sets() {
    let harness = Harness::new();

    // Rows vary what else the two creates share: everything, and the name alone.
    let cases: [(&str, Value, Value); 2] = [
        (
            "identical requests",
            json!({ "RuleSetName": "shared", "Rules": [] }),
            json!({ "RuleSetName": "shared", "Rules": [] }),
        ),
        (
            "same name, different rules",
            json!({ "RuleSetName": "shared.too", "Rules": [] }),
            json!({
                "RuleSetName": "shared.too",
                "Rules": [{ "Actions": [{ "Drop": {} }] }],
            }),
        ),
    ];
    for (case, first_body, second_body) in cases {
        let first = harness.create(first_body).await;
        let second = harness.create(second_body.clone()).await;
        assert_ne!(
            first, second,
            "case {case:?}: two creates must mint two ids"
        );

        let (status, out) = harness
            .call("GetRuleSet", json!({ "RuleSetId": second.clone() }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second rule set must be readable, got {status} {out:?}"
        );
        assert_eq!(
            &out["Rules"], &second_body["Rules"],
            "case {case:?}: the second rule set must carry its OWN rules, got {out:?}"
        );

        let (status, first_out) = harness
            .call("GetRuleSet", json!({ "RuleSetId": first }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first rule set must still be readable, got {status} {first_out:?}"
        );
    }
}

/// `ClientToken` is `@idempotencyToken`: repeating one create returns the id the first call
/// minted and provisions nothing new, while the same token presented with a DIFFERENT
/// request is refused with the modelled `ConflictException` rather than answered with the
/// first request's rule set.
#[tokio::test]
async fn a_client_token_makes_a_retry_return_the_first_rule_set() {
    let harness = Harness::new();

    // Rows: a retry of each distinguishable part of the request — the rules, the name and
    // the tags — plus a plain repeat of the identical request.
    let cases: [(&str, Value, Value); 4] = [
        (
            "identical repeat",
            json!({ "RuleSetName": "retry.one", "Rules": [], "ClientToken": "token-one" }),
            json!({ "RuleSetName": "retry.one", "Rules": [], "ClientToken": "token-one" }),
        ),
        (
            "different rules",
            json!({
                "RuleSetName": "retry.two",
                "Rules": [{ "Actions": [{ "Drop": {} }] }],
                "ClientToken": "token-two",
            }),
            json!({
                "RuleSetName": "retry.two",
                "Rules": [{ "Actions": [{ "AddHeader": { "HeaderName": "X-A", "HeaderValue": "1" } }] }],
                "ClientToken": "token-two",
            }),
        ),
        (
            "different name",
            json!({ "RuleSetName": "retry.three", "Rules": [], "ClientToken": "token-three" }),
            json!({ "RuleSetName": "retry.three.b", "Rules": [], "ClientToken": "token-three" }),
        ),
        (
            "different tags",
            json!({
                "RuleSetName": "retry.four",
                "Rules": [],
                "Tags": [{ "Key": "a", "Value": "1" }],
                "ClientToken": "token-four",
            }),
            json!({
                "RuleSetName": "retry.four",
                "Rules": [],
                "Tags": [{ "Key": "a", "Value": "2" }],
                "ClientToken": "token-four",
            }),
        ),
    ];
    for (case, first_body, second_body) in cases {
        let first = harness.create(first_body.clone()).await;

        // The identical repeat is the retry; every other row differs from the first
        // request, so it is not one.
        let is_retry = first_body == second_body;
        let (status, out) = harness.call("CreateRuleSet", second_body.clone()).await;
        if is_retry {
            assert_eq!(
                status, 200,
                "case {case:?}: a retry of the same request must be served, got {status} {out:?}"
            );
            assert_eq!(
                out["RuleSetId"].as_str(),
                Some(first.as_str()),
                "case {case:?}: a retry must answer with the id the first call minted, got {out:?}"
            );
        } else {
            assert_eq!(
                status, 409,
                "case {case:?}: reusing a ClientToken for a different request must be refused, \
                 got {status} {out:?}"
            );
            assert_eq!(
                error_code(&out),
                "ConflictException",
                "case {case:?}: unexpected error code, got {out:?}"
            );
            assert_eq!(
                out["Message"].as_str(),
                Some(
                    format!(
                        "ClientToken was already used to create rule set {first} with different \
                         parameters."
                    )
                    .as_str()
                ),
                "case {case:?}: unexpected refusal message, got {out:?}"
            );
        }

        // Whichever answer the second call got, the first rule set is unchanged.
        let (status, stored) = harness
            .call("GetRuleSet", json!({ "RuleSetId": first.clone() }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first rule set must still be readable, got {status} {stored:?}"
        );
        assert_eq!(
            stored["RuleSetName"].as_str(),
            first_body["RuleSetName"].as_str(),
            "case {case:?}: the first rule set must keep the name it was created with, got \
             {stored:?}"
        );
        assert_eq!(
            &stored["Rules"], &first_body["Rules"],
            "case {case:?}: the first rule set must keep the rules it was created with, got \
             {stored:?}"
        );
    }
}

/// A `RuleSetId` nothing created is refused with the modelled `ResourceNotFoundException`
/// (`@httpError 404`) and a message naming the id the request asked for, and the refusal
/// creates nothing.
#[tokio::test]
async fn get_rule_set_on_an_unknown_id_is_refused() {
    let harness = Harness::new();
    let live = harness
        .create(json!({ "RuleSetName": "present", "Rules": [] }))
        .await;

    // Rows: ids inside `RuleSetId`'s 1..100 length that name nothing — the shortest one, a
    // hyphenated one, the longest one, and the live id with one character changed.
    let unknown: [String; 4] = [
        "x".to_string(),
        "no-such-rule-set".to_string(),
        "9".repeat(100),
        format!("{}0", &live[..live.len() - 1]),
    ];
    for id in &unknown {
        if id == &live {
            continue;
        }
        let (status, out) = harness.call("GetRuleSet", json!({ "RuleSetId": id })).await;
        assert_eq!(
            status, 404,
            "case {id:?}: expected an unknown RuleSetId to answer 404, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {id:?}: unexpected error code, got {out:?}"
        );
        assert_eq!(
            out["Message"].as_str(),
            Some(format!("Could not find RuleSet with id {id}").as_str()),
            "case {id:?}: unexpected refusal message, got {out:?}"
        );
    }

    let (status, out) = harness
        .call("GetRuleSet", json!({ "RuleSetId": live }))
        .await;
    assert_eq!(
        status, 200,
        "case live-after-misses: the stored rule set must still be readable, got {status} {out:?}"
    );
}

/// A request outside the modelled constraints is refused with a 400 `ValidationException`
/// before anything is stored — never a 501 and never a create.
#[tokio::test]
async fn requests_outside_the_modelled_constraints_are_refused() {
    let harness = Harness::new();

    // Rows walk each published bound of the input shapes and one step past it:
    // `RuleSetName` (len 1..100, pattern `^[a-zA-Z0-9_.-]+$`), `Rules` (len 0..40) and
    // `Rule$Actions` (required, len 1..10).
    let cases: [(&str, Value); 6] = [
        (
            "empty name, one below the minimum length",
            json!({ "RuleSetName": "", "Rules": [] }),
        ),
        (
            "101 characters, one past the maximum name length",
            json!({ "RuleSetName": "x".repeat(101), "Rules": [] }),
        ),
        (
            "a space, outside the name pattern",
            json!({ "RuleSetName": "not allowed", "Rules": [] }),
        ),
        (
            "41 rules, one past the maximum Rules length",
            json!({
                "RuleSetName": "too.many.rules",
                "Rules": (0..41)
                    .map(|_| json!({ "Actions": [{ "Drop": {} }] }))
                    .collect::<Vec<Value>>(),
            }),
        ),
        (
            "a rule with no actions, one below the minimum Actions length",
            json!({ "RuleSetName": "no.actions", "Rules": [{ "Actions": [] }] }),
        ),
        (
            "no Rules member at all, which the model makes required",
            json!({ "RuleSetName": "no.rules.member" }),
        ),
    ];
    for (case, body) in cases {
        let (status, out) = harness.call("CreateRuleSet", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
    }

    // The bounds themselves are accepted, so the refusals above are the bound and not a
    // blanket rejection: the longest name and the longest rule list the model admits.
    let accepted = harness
        .create(json!({
            "RuleSetName": "y".repeat(100),
            "Rules": (0..40)
                .map(|_| json!({ "Actions": [{ "Drop": {} }] }))
                .collect::<Vec<Value>>(),
        }))
        .await;
    assert_well_formed_id("100 characters and 40 rules, at the maximum bounds", &accepted);
}

/// The rule-set store is BOUNDED: a create past the per-account quota is refused with the
/// modelled `ServiceQuotaExceededException`, and the refusal does not corrupt what is
/// already stored.
#[tokio::test]
async fn creates_past_the_rule_set_quota_are_refused() {
    let harness = Harness::new();

    let mut created: Vec<String> = Vec::new();
    for index in 0..MAX_RULE_SETS {
        created.push(
            harness
                .create(json!({ "RuleSetName": format!("quota-{index:04}"), "Rules": [] }))
                .await,
        );
    }

    // Rows: a plain create, and one carrying a ClientToken that was never used — neither
    // may be served past the quota.
    let cases: [(&str, Value); 2] = [
        (
            "plain",
            json!({ "RuleSetName": "quota-overflow", "Rules": [] }),
        ),
        (
            "with a fresh ClientToken",
            json!({
                "RuleSetName": "quota-overflow-2",
                "Rules": [],
                "ClientToken": "never-seen",
            }),
        ),
    ];
    for (case, body) in cases {
        let (status, out) = harness.call("CreateRuleSet", body).await;
        assert_eq!(
            status, 402,
            "case {case:?}: expected a create past the quota to answer 402, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ServiceQuotaExceededException",
            "case {case:?}: unexpected error code, got {out:?}"
        );
        assert_eq!(
            out["Message"].as_str(),
            Some(
                format!(
                    "The maximum number of rule sets ({MAX_RULE_SETS}) has been reached. Delete \
                     a rule set before creating another."
                )
                .as_str()
            ),
            "case {case:?}: unexpected refusal message, got {out:?}"
        );
    }

    // Every rule set created before the quota was reached is still readable.
    for id in &created {
        let (status, out) = harness.call("GetRuleSet", json!({ "RuleSetId": id })).await;
        assert_eq!(
            status, 200,
            "case {id:?}: a stored rule set must survive a refused create, got {status} {out:?}"
        );
    }
}
