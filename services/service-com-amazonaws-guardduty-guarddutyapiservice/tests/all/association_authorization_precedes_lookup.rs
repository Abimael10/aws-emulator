// SPDX-License-Identifier: CC0-1.0

//! WHEN the IAM decision about an association create is taken: before the rule the
//! request names is resolved, over the restJson1 wire.
//!
//! The rule under test: an operation whose model declares `AccessDeniedException` judges
//! the caller's permissions FIRST, so a caller no identity-based policy allows is refused
//! the modelled 403 whether or not what it named exists, and the operation's own answer
//! about existence — the modelled 404 naming the rule asked for — is reached only by a
//! caller the policies allow. Two callers making the IDENTICAL call therefore differ in
//! the CLASS of the answer, not only in its wording, and the refused caller learns nothing
//! about the catalogue.
//!
//! The rows differ along what the rule generalises over: whether the rule the request
//! names is one the catalogue publishes or one it does not, the FORM of the rule id (one
//! segment, several hyphenated segments, a long lower-case hex id, a single character), the
//! `Mode` the create asks for, and what the caller's policy says (nothing at all, every
//! action of this service on everything).
//!
//! Which RESOURCE the refusal names is the `detection_rule_association_denial_resource`
//! suite's rule; this one is about the ORDER the two decisions are taken in, and reuses
//! that suite's ARN builder and published-rule reader, and the `identity_authorization`
//! suite's principals, so the decision here is the real one IAM's own operations wrote.

use serde_json::json;
use service_com_amazonaws_guardduty_guarddutyapiservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};

use crate::detection_rule_association_denial_resource::{associations_of, published_rule_id};
use crate::identity_authorization::{
    allow, default_deny_message, Emulator, Reply, ACCESS_DENIED, ACCESS_DENIED_STATUS, ACCOUNT_KEY,
};

/// The operation under test, as `Operation::NAME` spells it.
const CREATE: &str = "CreateCustomDetectionRuleAssociation";

/// The URI the association collection is bound to
/// (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`: POST
/// `/custom-detection-rule/association`).
const ASSOCIATION_COLLECTION_PATH: &str = "/custom-detection-rule/association";

/// The modelled answer an ALLOWED caller gets for a rule the catalogue does not publish
/// (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`:
/// `ResourceNotFoundException`, `httpError` 404).
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The two members of `AssociationMode` (`smithy_query(shape="AssociationMode")`).
const MODES: [&str; 2] = ["DRY_RUN", "LIVE"];

/// What the operation itself answers a caller IAM allows — the answer a refused caller
/// never reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Served {
    /// The catalogue publishes no such rule: the modelled 404, naming the rule asked for.
    NotFound,
    /// The catalogue publishes it: the create is served and the association is recorded.
    Recorded,
}

/// One create, and the answer its two callers get.
struct Case {
    case: String,
    rule_id: String,
    mode: &'static str,
    served: Served,
}

/// The error CLASS a reply carries, with the shape namespace and any trailing detail the
/// `x-amzn-errortype` header may append stripped off.
fn class(reply: &Reply) -> String {
    let raw = if reply.error_code.is_empty() {
        reply.body["__type"].as_str().unwrap_or_default()
    } else {
        reply.error_code.as_str()
    };
    raw.rsplit('#')
        .next()
        .unwrap_or(raw)
        .split(':')
        .next()
        .unwrap_or(raw)
        .to_string()
}

/// The body of a create naming `rule_id` and `mode`.
fn create_body(rule_id: &str, mode: &str) -> serde_json::Value {
    json!({"ruleId": rule_id, "mode": mode})
}

/// Every rule id a listing reports an association for, as the account's own credential
/// reads them.
async fn associated_rule_ids(emulator: &Emulator) -> Vec<String> {
    let listed = emulator
        .call(ACCOUNT_KEY, "GET", ASSOCIATION_COLLECTION_PATH, None)
        .await;
    assert_eq!(
        listed.status, 200,
        "case \"the recorded associations\": expected 200, got {listed:?}"
    );
    let mut ids: Vec<String> = listed.body["ruleAssociations"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["ruleId"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// RULE: the permission decision is taken before the rule is resolved, so the same call
/// that a zero-permission caller is refused the modelled 403 for reaches the operation's
/// own answer — 404 for a rule the catalogue does not publish, the create's 200 for one it
/// does — when the caller's policies allow the action.
#[tokio::test]
async fn the_permission_decision_is_taken_before_the_rule_is_resolved() {
    let emulator = Emulator::new();
    let zero = emulator.user("assoc_order_zero", None).await;
    let allowed = emulator
        .user(
            "assoc_order_allowed",
            Some(&allow(&format!("{ACTION_PREFIX}:*"), "*")),
        )
        .await;
    let published = published_rule_id(&emulator).await;

    // Rule ids of the forms `RuleId` admits (`@pattern ^[a-z0-9]+(-[a-z0-9]+)*$`,
    // `@length` 1..100 — `smithy_query(operation="CreateCustomDetectionRuleAssociation")`)
    // that the catalogue does not publish, in both modes; and the id it DOES publish,
    // which this suite did not choose, once — a second create for the same rule is the
    // modelled conflict rather than another answer about existence.
    let mut cases: Vec<Case> = Vec::new();
    for rule_id in [
        "nosuchrule",
        "credential-exfiltration-probe",
        "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        "a",
    ] {
        for mode in MODES {
            cases.push(Case {
                case: format!("{rule_id:?} in {mode}, which the catalogue does not publish"),
                rule_id: rule_id.to_string(),
                mode,
                served: Served::NotFound,
            });
        }
    }
    cases.push(Case {
        case: format!("{published:?}, which the catalogue publishes"),
        rule_id: published.clone(),
        mode: MODES[0],
        served: Served::Recorded,
    });

    for case in &cases {
        let body = create_body(&case.rule_id, case.mode);

        // The zero-permission caller: the modelled 403, worded as IAM words a request no
        // identity-based document allows, and carrying nothing about the catalogue.
        let refused = emulator
            .call(
                &zero.access_key_id,
                "POST",
                ASSOCIATION_COLLECTION_PATH,
                Some(&body),
            )
            .await;
        let expected = default_deny_message(
            &zero.arn,
            CREATE,
            &associations_of(&case.rule_id, ANY_RESOURCE),
        );
        assert_eq!(
            (refused.status, class(&refused).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED} for a caller no policy \
             allows, got {} {:?}",
            case.case,
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {:?}: expected the no-identity-based-policy sentence, got {:?}",
            case.case,
            refused.message()
        );
        assert!(
            !refused.message().contains("not found"),
            "case {:?}: a refused caller learns nothing about existence, got {:?}",
            case.case,
            refused.message()
        );

        // The identical call by a caller the policies allow reaches the operation's own
        // answer, which is a DIFFERENT class of answer and not merely different wording.
        let served = emulator
            .call(
                &allowed.access_key_id,
                "POST",
                ASSOCIATION_COLLECTION_PATH,
                Some(&body),
            )
            .await;
        match case.served {
            Served::NotFound => {
                assert_eq!(
                    (served.status, class(&served).as_str()),
                    (NOT_FOUND_STATUS, NOT_FOUND),
                    "case {:?}: expected {NOT_FOUND_STATUS} {NOT_FOUND} for an allowed caller, \
                     got {} {:?}",
                    case.case,
                    served.status,
                    served.body
                );
                assert_eq!(
                    served.message(),
                    format!("Rule not found: {}", case.rule_id),
                    "case {:?}: expected the absence to name the rule asked for, got {:?}",
                    case.case,
                    served.message()
                );
            }
            Served::Recorded => {
                assert_eq!(
                    served.status, 200,
                    "case {:?}: expected the create's own 200 for an allowed caller, got {:?}",
                    case.case, served.body
                );
                assert_eq!(
                    served.body["ruleAssociation"]["ruleId"].as_str(),
                    Some(case.rule_id.as_str()),
                    "case {:?}: expected the association to carry the rule it binds, got {:?}",
                    case.case,
                    served.body
                );
            }
        }
    }

    // Nothing the 403s refused was written, and nothing the 404s named exists: the account
    // holds exactly the association the allowed caller's served create made.
    let recorded = associated_rule_ids(&emulator).await;
    assert_eq!(
        recorded,
        vec![published.clone()],
        "case \"after the refusals\": expected only the served create's association, got \
         {recorded:?}"
    );
}

/// RULE: the refusal is decided on the request's identity, action and resource alone, so
/// what it says about a rule the catalogue publishes and about one it does not differ only
/// in the rule id the resource ARN carries — a refused caller cannot probe the catalogue.
#[tokio::test]
async fn the_refusal_tells_a_published_rule_from_an_absent_one_only_by_its_resource() {
    let emulator = Emulator::new();
    let zero = emulator.user("assoc_order_probe", None).await;
    let published = published_rule_id(&emulator).await;
    let absent = "nosuchrule";

    for mode in MODES {
        let for_published = emulator
            .call(
                &zero.access_key_id,
                "POST",
                ASSOCIATION_COLLECTION_PATH,
                Some(&create_body(&published, mode)),
            )
            .await;
        let for_absent = emulator
            .call(
                &zero.access_key_id,
                "POST",
                ASSOCIATION_COLLECTION_PATH,
                Some(&create_body(absent, mode)),
            )
            .await;
        assert_eq!(
            (for_published.status, for_absent.status),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED_STATUS),
            "case {mode:?}: expected both calls refused {ACCESS_DENIED_STATUS}, got {:?} and {:?}",
            for_published.body,
            for_absent.body
        );
        assert_eq!(
            for_published
                .message()
                .replace(&associations_of(&published, ANY_RESOURCE), "<resource>"),
            for_absent
                .message()
                .replace(&associations_of(absent, ANY_RESOURCE), "<resource>"),
            "case {mode:?}: expected the two refusals to differ only in the resource they \
             name, got {:?} and {:?}",
            for_published.message(),
            for_absent.message()
        );
    }

    // And the probe recorded nothing.
    let recorded = associated_rule_ids(&emulator).await;
    assert!(
        recorded.is_empty(),
        "case \"after the probe\": expected no association, got {recorded:?}"
    );
}
