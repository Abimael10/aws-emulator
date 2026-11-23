// SPDX-License-Identifier: CC0-1.0

//! WHICH context keys a policy simulation reports as missing a value.
//!
//! RULE: `MissingContextValues` is "a list of context keys that are required by the included
//! input policies but that were not provided by one of the input parameters … to discover the
//! context keys used by a set of policies, you can call GetContextKeysForCustomPolicy or
//! GetContextKeysForPrincipalPolicy" (`EvaluationResult$MissingContextValues`, and the same
//! sentence on `ResourceSpecificResult$MissingContextValues`) — so a key is REQUIRED because
//! a statement of a document the simulation INCLUDES tests it, and PROVIDED because the
//! request's `ContextEntries` carry a value for it. Both are facts about those documents and
//! that request, so neither depends on what the decision reached: a statement about another
//! action, a statement scoped to another resource and a clause the absent value itself
//! settles each still name a key the included policies require and the caller did not
//! supply, while a key the request supplies is named nowhere.
//!
//! Where the keys are reported follows the two members: each resource's own result reports
//! them together with the ones only the resource-based policy requires — "each resource in
//! the simulation is treated as if it had this policy attached" (`ResourcePolicy`) — and the
//! action's own result reports the ones its own documents require, over the resources whose
//! decision still wanted them; a pair the documents GRANTED is settled and names none there.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: which
//! statement tests the key (the one about the simulated pair, one about another action, one
//! scoped to another resource, one that is neither), how many keys and documents state them,
//! the operator the clause is written with — including the three spellings an ABSENT value
//! satisfies — whether the request supplies a value, in which case spelling, and whether
//! that value matches. Every row is asked of both operations that answer a simulation, since
//! the report is theirs together. No single request is the specification.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns, the decision values
//! are `tests/all/policy_simulation.rs`'s, the principal fixture
//! `tests/all/request_condition_context.rs`'s and the split of a result into its own members
//! and its per-resource ones `tests/all/simulated_resource_policy.rs`'s, so every simulation
//! suite reads a report one way.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{ALLOWED, IMPLICIT_DENY};
use crate::request_condition_context::{create_user, user_arn};
use crate::simulated_resource_policy::parts;
use crate::user_lifecycle::{call, element, elements, router};

/// The two operations that answer a simulation: the report is the same projection for both,
/// and each row below is asked of each.
const OPERATIONS: &[&str] = &["SimulateCustomPolicy", "SimulatePrincipalPolicy"];

/// The action every row simulates — a read of one S3 object, which IS published with a
/// resource type, so a pair naming one object is a request that action could have been made
/// as.
const ACTION: &str = "s3:GetObject";

/// The resource every row simulates that action on.
const OBJECT_ARN: &str = "arn:aws:s3:::audit-reports/q1.csv";

/// An action of the same service that no row simulates: what a statement about ANOTHER
/// action names.
const OTHER_ACTION: &str = "s3:PutObject";

/// A global condition key, written in AWS's own spelling.
const VPCE_KEY: &str = "aws:SourceVpce";

/// The value the clauses below state for it.
const VPCE_VALUE: &str = "vpce-0abc";

/// A service condition key of the simulated action's own service, so a row can state two
/// keys that are not spellings of one.
const ACL_KEY: &str = "s3:x-amz-acl";

/// One row of the table: the documents the simulation includes, the value the request
/// supplies for a key (under the spelling it supplies it), the decision the row reaches and
/// the keys the report must name.
struct Required {
    case: &'static str,
    /// The documents the request passes in `PolicyInputList`.
    documents: &'static [&'static str],
    /// The `(key, value)` the request supplies in `ContextEntries`, when it supplies one.
    supplied: Option<(&'static str, &'static str)>,
    /// The decision reported for the simulated pair.
    decision: &'static str,
    /// The keys the resource's own result must name, in report order.
    missing: &'static [&'static str],
}

/// A grant of the simulated pair, guarded by `operator` over [`VPCE_KEY`].
macro_rules! guarded {
    ($operator:literal, $value:literal) => {
        concat!(
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":""#,
            "s3:GetObject",
            r#"","Resource":""#,
            "arn:aws:s3:::audit-reports/q1.csv",
            r#"","Condition":{""#,
            $operator,
            r#"":{"aws:SourceVpce":""#,
            $value,
            r#""}}}]}"#
        )
    };
}

/// The grant of the simulated pair whose clause is a positive comparison over [`VPCE_KEY`]:
/// an absent value falsifies it, so nothing grants.
const ABOUT_THE_PAIR: &str = guarded!("StringEquals", "vpce-0abc");

/// The same clause written with the negated operator, which an absent value SATISFIES: the
/// grant stands, and the key is still one the document requires.
const NEGATED: &str = guarded!("StringNotEquals", "vpce-0abc");

/// The same comparison under IAM's `IfExists` suffix, which an absent value also satisfies.
const IF_EXISTS: &str = guarded!("StringEqualsIfExists", "vpce-0abc");

/// A grant whose clause asserts the key is ABSENT (`Null` with `true`), which an absent
/// value satisfies.
const ASSERTS_ABSENT: &str = guarded!("Null", "true");

/// A grant of ANOTHER action, scoped to the simulated resource, guarded over [`ACL_KEY`]:
/// the statement is not about the simulated action at all.
const ABOUT_ANOTHER_ACTION: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::audit-reports/q1.csv","Condition":{"StringEquals":{"s3:x-amz-acl":"private"}}}]}"#;

/// A grant of the simulated action scoped to ANOTHER resource, guarded over [`VPCE_KEY`].
const SCOPED_TO_ANOTHER_RESOURCE: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::audit-reports/q2.csv","Condition":{"StringEquals":{"aws:SourceVpce":"vpce-0abc"}}}]}"#;

/// A grant that is about neither the simulated action nor the simulated resource.
const ABOUT_NEITHER: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::audit-reports/q2.csv","Condition":{"StringEquals":{"s3:x-amz-acl":"private"}}}]}"#;

/// Two statements in ONE document, each guarded over its own key: the first about the
/// simulated pair, the second about neither.
const TWO_STATEMENTS: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::audit-reports/q1.csv","Condition":{"StringEquals":{"aws:SourceVpce":"vpce-0abc"}}},{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::audit-reports/q2.csv","Condition":{"StringEquals":{"s3:x-amz-acl":"private"}}}]}"#;

/// The same key written in another case, which IAM does not distinguish: a document stating
/// it alongside [`ABOUT_THE_PAIR`] requires ONE key, not two.
const ANOTHER_CASE_SPELLING: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::audit-reports/q1.csv","Condition":{"StringEquals":{"AWS:SOURCEVPCE":"vpce-0abc"}}}]}"#;

const REQUIRED: &[Required] = &[
    Required {
        case: "a key tested by the statement about the simulated pair",
        documents: &[ABOUT_THE_PAIR],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "a key tested by a statement about another action",
        documents: &[ABOUT_ANOTHER_ACTION],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[ACL_KEY],
    },
    Required {
        case: "a key tested by a statement scoped to another resource",
        documents: &[SCOPED_TO_ANOTHER_RESOURCE],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "a key tested by a statement about neither the action nor the resource",
        documents: &[ABOUT_NEITHER],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[ACL_KEY],
    },
    Required {
        case: "two keys tested by two statements of one document",
        documents: &[TWO_STATEMENTS],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY, ACL_KEY],
    },
    Required {
        case: "two keys tested by two documents",
        documents: &[ABOUT_THE_PAIR, ABOUT_ANOTHER_ACTION],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY, ACL_KEY],
    },
    Required {
        case: "one key tested by two documents",
        documents: &[ABOUT_THE_PAIR, SCOPED_TO_ANOTHER_RESOURCE],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "one key written in two case spellings",
        documents: &[ABOUT_THE_PAIR, ANOTHER_CASE_SPELLING],
        supplied: None,
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "a clause the absent value itself satisfies",
        documents: &[NEGATED],
        supplied: None,
        decision: ALLOWED,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "an IfExists clause the absent value satisfies",
        documents: &[IF_EXISTS],
        supplied: None,
        decision: ALLOWED,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "a clause asserting the key is absent",
        documents: &[ASSERTS_ABSENT],
        supplied: None,
        decision: ALLOWED,
        missing: &[VPCE_KEY],
    },
    Required {
        case: "the key supplied with the value the clause states",
        documents: &[ABOUT_THE_PAIR],
        supplied: Some((VPCE_KEY, VPCE_VALUE)),
        decision: ALLOWED,
        missing: &[],
    },
    Required {
        case: "the key supplied with another value",
        documents: &[ABOUT_THE_PAIR],
        supplied: Some((VPCE_KEY, "vpce-0def")),
        decision: IMPLICIT_DENY,
        missing: &[],
    },
    Required {
        case: "the key supplied under another case spelling",
        documents: &[ABOUT_THE_PAIR],
        supplied: Some(("AWS:SOURCEVPCE", VPCE_VALUE)),
        decision: ALLOWED,
        missing: &[],
    },
    Required {
        case: "a value supplied for another key than the one tested",
        documents: &[ABOUT_THE_PAIR],
        supplied: Some((ACL_KEY, "private")),
        decision: IMPLICIT_DENY,
        missing: &[VPCE_KEY],
    },
];

/// RULE: the keys a result names are the ones the included documents TEST and the request
/// supplied no value for, whichever statement tests them and however the absence settles the
/// clause — reported by the resource's own result, and by the action's own result where its
/// decision is not the allow.
#[tokio::test]
async fn the_report_names_the_keys_the_included_policies_require() {
    let router = router();
    let principal = "required-keys";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for operation in OPERATIONS {
        for case in REQUIRED {
            let context: Vec<(&str, &str)> = case.supplied.map(|pair| vec![pair]).unwrap_or_default();
            let (status, body) =
                simulate(&router, operation, &arn, case.documents, &context, &[]).await;
            assert_eq!(
                status, 200,
                "case {:?}/{operation}: expected 200, got {status}: {body}",
                case.case
            );
            let (own, per_resource) = parts(&body);
            assert_eq!(
                element(own, "EvalDecision"),
                case.decision,
                "case {:?}/{operation}: expected the decision {:?}, got {body}",
                case.case,
                case.decision
            );
            let expected: Vec<String> = case
                .missing
                .iter()
                .map(|key| (*key).to_string())
                .collect();
            let reported = missing_keys(per_resource);
            assert_eq!(
                reported, expected,
                "case {:?}/{operation}: expected the resource's own result to name \
                 {expected:?}, got {reported:?}: {body}",
                case.case
            );
            // The action's own result names the keys its decision still wants: none where
            // the documents granted the pair, since nothing was missing to settle it.
            let expected_own: Vec<String> = if case.decision == ALLOWED {
                Vec::new()
            } else {
                expected.clone()
            };
            let reported_own = missing_keys(own);
            assert_eq!(
                reported_own, expected_own,
                "case {:?}/{operation}: expected the action's own result to name \
                 {expected_own:?}, got {reported_own:?}: {body}",
                case.case
            );
        }
    }
}

/// An identity-based grant of the simulated pair guarded over [`VPCE_KEY`], which no request
/// below supplies: the key the PRINCIPAL's own documents require.
const IDENTITY_GUARDED: &str = ABOUT_THE_PAIR;

/// The key the resource-based policy below tests, which the identity-based document does
/// not.
const TRANSPORT_KEY: &str = "aws:SecureTransport";

/// RULE: a key only the resource-based policy requires is named by the result of the
/// resource that policy is treated as attached to, and not by the action's own result, which
/// names the keys of the documents in force on the principal.
#[tokio::test]
async fn a_key_only_the_resource_policy_requires_is_named_by_that_resource() {
    let router = router();
    let principal = "required-keys-resource";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);
    let resource_policy = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{arn}"}},"Action":"{ACTION}","Resource":"{OBJECT_ARN}","Condition":{{"Bool":{{"{TRANSPORT_KEY}":"true"}}}}}}]}}"#
    );

    for operation in OPERATIONS {
        let extra = vec![("ResourcePolicy".to_string(), resource_policy.clone())];
        let (status, body) =
            simulate(&router, operation, &arn, &[IDENTITY_GUARDED], &[], &extra).await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status}: {body}"
        );
        let (own, per_resource) = parts(&body);
        // Neither side's clause can hold without a value, so nothing granted the pair and
        // both results still name what they were missing.
        assert_eq!(
            element(own, "EvalDecision"),
            IMPLICIT_DENY,
            "case {operation}: expected the unsatisfiable clauses to grant nothing, got {body}"
        );
        assert_eq!(
            missing_keys(own),
            vec![VPCE_KEY.to_string()],
            "case {operation}: expected the action's own result to name only the key its own \
             documents require, got {body}"
        );
        let mut reported = missing_keys(per_resource);
        reported.sort();
        let mut expected = vec![VPCE_KEY.to_string(), TRANSPORT_KEY.to_string()];
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {operation}: expected the resource's own result to name both keys, got \
             {reported:?}: {body}"
        );
    }
}

/// RULE: the report is a fact about the documents and the request, so every result of one
/// response names the same keys — a request asking about several actions reports them per
/// action rather than only on the action whose statement tests them.
#[tokio::test]
async fn every_action_of_one_request_names_the_keys_the_policies_require() {
    let router = router();
    let principal = "required-keys-actions";
    create_user(&router, "the simulated principal", principal, &[]).await;
    let arn = user_arn(principal);

    for operation in OPERATIONS {
        let extra = vec![(
            "ActionNames.member.2".to_string(),
            OTHER_ACTION.to_string(),
        )];
        let (status, body) =
            simulate(&router, operation, &arn, &[ABOUT_THE_PAIR], &[], &extra).await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status}: {body}"
        );
        let actions = elements(&body, "EvalActionName");
        assert_eq!(
            actions,
            vec![ACTION.to_string(), OTHER_ACTION.to_string()],
            "case {operation}: expected one result per action in request order, got {body}"
        );
        // Four lists: each action's own result and its one resource-specific result, each
        // naming the one key the document requires. Neither action is granted, so no
        // result's own list is settled into naming none.
        let reported = missing_keys(&body);
        let expected: Vec<String> = (0..4).map(|_| VPCE_KEY.to_string()).collect();
        assert_eq!(
            reported, expected,
            "case {operation}: expected every result to name {VPCE_KEY}, got {reported:?}: \
             {body}"
        );
    }
}

/// One call of `operation` simulating [`ACTION`] on [`OBJECT_ARN`] against `documents`,
/// which the request supplies in `PolicyInputList`, with `context` as its `ContextEntries`.
///
/// `SimulatePrincipalPolicy` additionally names the principal whose own policies join them —
/// the fixture user, which carries none of its own, so both operations decide from the same
/// supplied documents. `CallerArn` is named for both, since a request that supplies a
/// resource-based policy has to name the caller its `Principal` element is matched against.
async fn simulate(
    router: &MultiServiceRouter,
    operation: &str,
    principal_arn: &str,
    documents: &[&str],
    context: &[(&str, &str)],
    extra: &[(String, String)],
) -> (u16, String) {
    let mut owned: Vec<(String, String)> = vec![
        ("ActionNames.member.1".to_string(), ACTION.to_string()),
        ("ResourceArns.member.1".to_string(), OBJECT_ARN.to_string()),
        ("CallerArn".to_string(), principal_arn.to_string()),
    ];
    for (at, document) in documents.iter().enumerate() {
        owned.push((
            format!("PolicyInputList.member.{}", at + 1),
            (*document).to_string(),
        ));
    }
    for (at, (key, value)) in context.iter().enumerate() {
        let member = at + 1;
        owned.push((
            format!("ContextEntries.member.{member}.ContextKeyName"),
            (*key).to_string(),
        ));
        owned.push((
            format!("ContextEntries.member.{member}.ContextKeyValues.member.1"),
            (*value).to_string(),
        ));
        owned.push((
            format!("ContextEntries.member.{member}.ContextKeyType"),
            "string".to_string(),
        ));
    }
    if operation == "SimulatePrincipalPolicy" {
        owned.push(("PolicySourceArn".to_string(), principal_arn.to_string()));
    }
    owned.extend(extra.iter().cloned());
    let params: Vec<(&str, &str)> = owned
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    call(router, operation, &params).await
}

/// The keys one part of a response names under `MissingContextValues`, in report order.
///
/// The member carries a LIST, so the shared leaf reader is taken over each list's own text
/// rather than over the element that carries it.
fn missing_keys(part: &str) -> Vec<String> {
    const OPEN: &str = "<MissingContextValues>";
    const CLOSE: &str = "</MissingContextValues>";
    let mut found = Vec::new();
    let mut rest = part;
    while let Some(start) = rest.find(OPEN) {
        let text = &rest[start + OPEN.len()..];
        let end = text
            .find(CLOSE)
            .unwrap_or_else(|| panic!("{OPEN} is never closed in {part}"));
        found.extend(elements(&text[..end], "member"));
        rest = &text[end..];
    }
    found
}
