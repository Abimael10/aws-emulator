// SPDX-License-Identifier: CC0-1.0

//! What `CreateCodeReview` STORES and `BatchGetCodeReviews` reads back, end to end over
//! the wire: the code review configurations an agent space holds.
//!
//! Every call goes through the real `MultiServiceRouter` the emulator boots, as a
//! `restJson1` `POST /<OperationName>` with a JSON body signed for the `securityagent`
//! signing name; the harness is `crate::support`. So nothing in this file can pass
//! against a handler that answers a canned body: a value must come back out of the
//! shared store through a read before a case counts.
//!
//! The rules this suite owns:
//!
//! * a create mints an opaque identity of the code review CLASS, stamps the record with
//!   the instant it was written, and answers from the record it persisted — so the batch
//!   read of that id reports the very same members;
//! * a code review is reachable only through the agent space that holds it, and an id
//!   the store does not hold is reported in `notFound` rather than as a failure;
//! * a create the service cannot accept is refused with the service's own error and
//!   stores nothing: an `agentSpaceId` that resolves to no agent space, a required
//!   member whose value names nothing, a budget cap that is not a positive number, and a
//!   space already holding its quota of code reviews.
//!
//! Each case asserts its rule over a TABLE of inputs: the rows differ along which
//! optional members the create carries, the shape and size of the assets, the enum value
//! chosen, the form of the budget cap, how many ids a read asks for, how many resolve and
//! how often each is repeated.

use std::collections::{BTreeMap, BTreeSet};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_securityagent_securityagent::state::{
    CODE_REVIEW_ID_PREFIX, MAX_CODE_REVIEWS_PER_AGENT_SPACE, MAX_CODE_REVIEW_ASSET_ENTRIES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{assert_uuid_body, call, emulator, member, ok};

/// The sentence a refusal of an agent space the account does not hold states: wholly
/// FIXED text, naming the resource class under the name the service's own records use for
/// an agent space and carrying nothing the request sent.
const AGENT_SPACE_NOT_FOUND: &str = "The specified agent instance does not exist.";

/// Every `CodeRemediationStrategy` the model admits
/// (`smithy_query(query="shape",shape="CodeRemediationStrategy",
/// model_path="services/service-com-amazonaws-securityagent-securityagent/smithy-model/com-amazonaws-securityagent-securityagent.json")`),
/// so a row exists for each and a value added to the enum makes this list go stale.
const CODE_REMEDIATION_STRATEGIES: [&str; 2] = ["AUTOMATIC", "DISABLED"];

/// Every `ValidationMode` the model admits (`smithy_query(query="shape",
/// shape="ValidationMode",model_path="services/service-com-amazonaws-securityagent-securityagent/smithy-model/com-amazonaws-securityagent-securityagent.json")`).
const VALIDATION_MODES: [&str; 2] = ["DISABLED", "SIMULATED"];

/// `CreateAgentSpace`, as the id of the record it minted — the space the code reviews of
/// a case are created in.
async fn create_agent_space(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let created = ok(router, "CreateAgentSpace", &json!({"name": name}), case).await;
    member(&created, "agentSpaceId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateAgentSpace returned no agentSpaceId"))
}

/// The whole 200 payload of a `BatchGetCodeReviews` over `ids` in `agent_space_id`.
async fn batch_get(
    router: &MultiServiceRouter,
    agent_space_id: &str,
    ids: &[String],
    case: &str,
) -> Value {
    ok(
        router,
        "BatchGetCodeReviews",
        &json!({"agentSpaceId": agent_space_id, "codeReviewIds": ids}),
        case,
    )
    .await
}

/// A member that must be present AND a JSON array.
fn array(payload: &Value, name: &str, case: &str) -> Vec<Value> {
    payload
        .get(name)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} member, got {payload}"))
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected {name} to be an array, got {payload}"))
        .clone()
}

/// A list of strings that must be present.
fn strings(payload: &Value, name: &str, case: &str) -> Vec<String> {
    array(payload, name, case)
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a {name} entry is not a string: {entry}"))
                .to_string()
        })
        .collect()
}

/// RULE: a `@timestampFormat("date-time")` member is an RFC 3339 instant STRING (a JSON
/// epoch number is the restJson1 default for a timestamp carrying no format), and it is
/// the instant the record was written rather than a placeholder.
fn date_time(payload: &Value, name: &str, case: &str) -> String {
    let value = payload
        .get(name)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} member, got {payload}"));
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected {name} as an RFC 3339 date-time string, got {value}")
    });
    let instant = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {name} {text:?} is not an RFC 3339 date-time: {error}")
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the test clock is after the epoch")
        .as_secs_f64();
    let seconds = instant.as_secs_f64();
    assert!(
        seconds > now - 3600.0 && seconds <= now + 1.0,
        "case {case:?}: expected {name} to be the instant the record was written (now \
         {now}), got {text:?}"
    );
    text.to_string()
}

/// RULE: a create answers from the record it persisted — the id it minted is of the code
/// review CLASS and carries nothing of the request, the timestamps are the instant it was
/// written, every supplied member is reported as stored, and the batch read of that id
/// answers the very same members.
#[tokio::test]
async fn a_created_code_review_reads_back_member_for_member() {
    let router = emulator();
    let agent_space_id = create_agent_space(&router, "code-review-round-trip", "seed").await;

    // The rows differ along which optional members the create carries, which asset kinds
    // the configuration names, the enum value chosen, the form of the budget cap and the
    // script the title is written in.
    let rows: Vec<(&str, Value)> = vec![
        ("the required members alone", json!({"assets": {}})),
        (
            "one endpoint",
            json!({"assets": {"endpoints": [{"uri": "https://alpha.example.com/api"}]}}),
        ),
        (
            "documents and source code",
            json!({
                "assets": {
                    "documents": [
                        {"s3Location": "s3://sec-agent/design.pdf"},
                        {"integratedDocument": {"integrationId": "int-1", "resourceId": "page-7"}},
                    ],
                    "sourceCode": [{"s3Location": "s3://sec-agent/service.zip"}],
                },
            }),
        ),
        (
            "an integrated repository and a trusted CA certificate",
            json!({
                "assets": {
                    "integratedRepositories": [
                        {"integrationId": "int-2", "providerResourceId": "repo-42", "branch": "main"},
                    ],
                    "trustedCaCertificates": [{"source": {"s3Location": "s3://sec-agent/ca.pem"}}],
                },
            }),
        ),
        (
            "actors with an authentication provider",
            json!({
                "assets": {
                    "actors": [{
                        "identifier": "service-user",
                        "uris": ["https://alpha.example.com/login"],
                        "authentication": {
                            "providerType": "SECRETS_MANAGER",
                            "value": "arn:aws:secretsmanager:us-east-1:000000000000:secret:actor",
                        },
                        "description": "the actor the review signs in as",
                        "enableEmailMfa": true,
                        "mfaForwardingAddress": "mfa@example.com",
                    }],
                },
            }),
        ),
        (
            "every optional member at once",
            json!({
                "assets": {
                    "endpoints": [
                        {"uri": "https://beta.example.com/api"},
                        {"uri": "https://beta.example.com/admin"},
                    ],
                    "sourceCode": [{"s3Location": "s3://sec-agent/beta.zip"}],
                },
                "serviceRole": "arn:aws:iam::000000000000:role/SecurityAgentCodeReview",
                "logConfig": {"logGroup": "/aws/securityagent/reviews", "logStream": "beta"},
                "codeRemediationStrategy": CODE_REMEDIATION_STRATEGIES[0],
                "validationMode": VALIDATION_MODES[1],
                "maxTaskHours": 12.5,
                "reportDestination": {
                    "integrationId": "int-3",
                    "containerId": "space-9",
                    "parentId": "parent-4",
                    "documentId": "doc-5",
                },
                "reportFilters": {
                    "riskLevels": ["HIGH", "CRITICAL"],
                    "confidenceLevels": ["MEDIUM"],
                    "statuses": ["ACTIVE"],
                    "riskTypes": ["SQL_INJECTION", "PATH_TRAVERSAL"],
                    "findingTypes": ["code-review"],
                    "taskStatuses": ["COMPLETED"],
                    "annotationNotes": true,
                    "complianceReport": false,
                },
            }),
        ),
        (
            "the other value of each enum, and the smallest budget cap of this table",
            json!({
                "assets": {"endpoints": [{"uri": "https://gamma.example.com"}]},
                "codeRemediationStrategy": CODE_REMEDIATION_STRATEGIES[1],
                "validationMode": VALIDATION_MODES[0],
                "maxTaskHours": 0.25,
                "reportFilters": {"annotationNotes": false, "complianceReport": true},
            }),
        ),
        (
            "a large budget cap and no assets at all",
            json!({"assets": {}, "maxTaskHours": 100_000.0}),
        ),
    ];

    let titles = [
        "nightly review",
        "обзор кода",
        "コードレビュー",
        "review (2) — rerun",
    ];
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for (index, (case, extras)) in rows.into_iter().enumerate() {
        let title = titles[index % titles.len()];
        let mut request = extras.clone();
        request["agentSpaceId"] = json!(agent_space_id);
        request["title"] = json!(title);

        let created = ok(&router, "CreateCodeReview", &request, case).await;

        // The id is the service's to choose: the code review class's prefix over an
        // opaque uuid body, carrying nothing of the request.
        let code_review_id = member(&created, "codeReviewId")
            .unwrap_or_else(|| panic!("case {case:?}: the create returned no codeReviewId"));
        let body = code_review_id
            .strip_prefix(CODE_REVIEW_ID_PREFIX)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a codeReviewId of the code review class \
                     ({CODE_REVIEW_ID_PREFIX}<uuid>), got {code_review_id:?}"
                )
            });
        assert_uuid_body(body, case, "codeReviewId");
        assert!(
            !body.contains(title) && !body.contains(&agent_space_id),
            "case {case:?}: expected an opaque id body, got {code_review_id:?}"
        );
        assert!(
            minted.insert(code_review_id.clone()),
            "case {case:?}: expected a fresh id per create, got {code_review_id:?} again"
        );

        // The identity and the timestamps the service stamped on the record: a record
        // nothing has updated yet carries one instant under both names.
        assert_eq!(
            member(&created, "agentSpaceId").as_deref(),
            Some(agent_space_id.as_str()),
            "case {case:?}: expected the create to report the owning agent space, got {created}"
        );
        assert_eq!(
            member(&created, "title").as_deref(),
            Some(title),
            "case {case:?}: expected the create to report the stored title, got {created}"
        );
        let created_at = date_time(&created, "createdAt", case);
        let updated_at = date_time(&created, "updatedAt", case);
        assert_eq!(
            created_at, updated_at,
            "case {case:?}: expected one instant on a record nothing has updated, got \
             createdAt {created_at:?} and updatedAt {updated_at:?}"
        );

        // Every member the create carried is reported as stored, verbatim.
        for (name, supplied) in extras.as_object().expect("the extras are an object") {
            assert_eq!(
                created.get(name),
                Some(supplied),
                "case {case:?}: expected the create to report the supplied {name} \
                 {supplied}, got {created}"
            );
        }

        // And the read of that id answers the SAME record: the create echo and the batch
        // read are one projection of one stored record, so they agree member for member.
        let payload = batch_get(
            &router,
            &agent_space_id,
            std::slice::from_ref(&code_review_id),
            case,
        )
        .await;
        let records = array(&payload, "codeReviews", case);
        let record = records
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the code review must read back: {payload}"));
        assert_eq!(
            record, &created,
            "case {case:?}: expected the read to report what the create answered \
             {created}, got {record}"
        );
        assert_eq!(
            strings(&payload, "notFound", case),
            Vec::<String>::new(),
            "case {case:?}: expected no unresolved id, got {payload}"
        );
    }
}

/// RULE: a code review is reachable only through the agent space that holds it, so a read
/// answers every requested id in exactly one place — an id that space holds in
/// `codeReviews`, every other id (one no create minted, and one that names a code review
/// of ANOTHER space) in `notFound` — once per DISTINCT id, in the order it was asked for,
/// with both lists present on every answer.
#[tokio::test]
async fn a_read_resolves_only_the_code_reviews_of_the_agent_space_it_names() {
    let router = emulator();
    let here = create_agent_space(&router, "reads-here", "seed").await;
    let elsewhere = create_agent_space(&router, "reads-elsewhere", "seed").await;

    // Two code reviews in the space under read, one in the neighbouring space.
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    for title in ["first review", "second review"] {
        let created = ok(
            &router,
            "CreateCodeReview",
            &json!({"agentSpaceId": here, "title": title, "assets": {}}),
            title,
        )
        .await;
        let id = member(&created, "codeReviewId").expect("the create returns an id");
        titles.insert(id, title.to_string());
    }
    let mine: Vec<String> = titles.keys().cloned().collect();
    let neighbours = ok(
        &router,
        "CreateCodeReview",
        &json!({"agentSpaceId": elsewhere, "title": "neighbour review", "assets": {}}),
        "seed",
    )
    .await;
    let neighbour = member(&neighbours, "codeReviewId").expect("the create returns an id");

    // Ids no create minted: an opaque value of no resource class's form, a readable
    // label, a name in another script, a stored id with a suffix, and the empty id.
    let unknown: Vec<String> = vec![
        "f".repeat(32),
        "no-such-code-review".to_string(),
        "неизвестно".to_string(),
        format!("{}-suffix", mine[0]),
        String::new(),
        neighbour.clone(),
    ];

    // The rows differ along how many ids are asked for, how many resolve, their order and
    // how often each is repeated.
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("no ids at all", vec![]),
        ("one id of this space", vec![mine[0].clone()]),
        ("every id of this space", mine.clone()),
        (
            "every id of this space, reversed",
            mine.iter().rev().cloned().collect(),
        ),
        ("one id no create minted", vec![unknown[1].clone()]),
        ("every id no create minted", unknown.clone()),
        (
            "an id of the neighbouring space",
            vec![neighbour.clone()],
        ),
        (
            "known and unknown interleaved",
            vec![
                unknown[0].clone(),
                mine[1].clone(),
                neighbour.clone(),
                mine[0].clone(),
                unknown[4].clone(),
            ],
        ),
        (
            "one id of this space asked for three times",
            vec![mine[1].clone(), mine[1].clone(), mine[1].clone()],
        ),
        (
            "an unknown id asked for twice",
            vec![unknown[2].clone(), unknown[2].clone()],
        ),
    ];

    for (case, requested) in rows {
        let payload = batch_get(&router, &here, &requested, case).await;

        // Both lists are present, whatever fell on which side.
        for name in ["codeReviews", "notFound"] {
            assert!(
                payload.get(name).is_some_and(Value::is_array),
                "case {case:?}: expected {name} as an array on every answer, got {payload}"
            );
        }

        let mut expected_found: Vec<String> = Vec::new();
        let mut expected_missing: Vec<String> = Vec::new();
        let mut seen: BTreeSet<&String> = BTreeSet::new();
        for id in &requested {
            if !seen.insert(id) {
                continue;
            }
            if titles.contains_key(id) {
                expected_found.push(id.clone());
            } else {
                expected_missing.push(id.clone());
            }
        }

        let resolved = array(&payload, "codeReviews", case);
        let resolved_ids: Vec<String> = resolved
            .iter()
            .map(|record| {
                member(record, "codeReviewId")
                    .unwrap_or_else(|| panic!("case {case:?}: a record carries no id: {record}"))
            })
            .collect();
        assert_eq!(
            resolved_ids, expected_found,
            "case {case:?}: expected codeReviews {expected_found:?}, got {payload}"
        );
        assert_eq!(
            strings(&payload, "notFound", case),
            expected_missing,
            "case {case:?}: expected notFound {expected_missing:?}, got {payload}"
        );

        // Each record answered is the one stored under that id, not another and not a
        // placeholder — and it names the space the read was scoped to.
        for record in &resolved {
            let id = member(record, "codeReviewId").expect("an id, asserted above");
            assert_eq!(
                member(record, "title").as_deref(),
                titles.get(&id).map(String::as_str),
                "case {case:?}: expected the stored title for {id:?}, got {record}"
            );
            assert_eq!(
                member(record, "agentSpaceId").as_deref(),
                Some(here.as_str()),
                "case {case:?}: expected the record to name the space read, got {record}"
            );
        }
    }

    // And the neighbouring space still resolves its own code review: the scoping hides a
    // record from the other space's reads, it does not remove it.
    let payload = batch_get(
        &router,
        &elsewhere,
        std::slice::from_ref(&neighbour),
        "the neighbouring space reads its own",
    )
    .await;
    assert_eq!(
        array(&payload, "codeReviews", "the neighbouring space reads its own").len(),
        1,
        "expected the neighbouring space to resolve its own code review, got {payload}"
    );
}

/// RULE: a create the service cannot accept is refused with the service's own error
/// naming what failed — HTTP 404 for an `agentSpaceId` that resolves to no agent space,
/// HTTP 400 for a required member whose value names nothing and for a budget cap that is
/// not a positive number — and stores NOTHING, while every value the rule admits is
/// stored.
#[tokio::test]
async fn a_refused_create_stores_nothing() {
    let router = emulator();
    let agent_space_id = create_agent_space(&router, "refusals", "seed").await;

    // The rows differ along which member is at fault and the form of its value.
    let refusals: Vec<(&str, Value, u16, &str, &str)> = vec![
        (
            "an agentSpaceId no create minted",
            json!({"agentSpaceId": "as-00000000-0000-4000-8000-000000000000", "title": "t", "assets": {}}),
            404,
            "ResourceNotFoundException",
            "",
        ),
        (
            "an agentSpaceId that is a readable label",
            json!({"agentSpaceId": "no-such-agent-space", "title": "t", "assets": {}}),
            404,
            "ResourceNotFoundException",
            "",
        ),
        (
            "the empty agentSpaceId",
            json!({"agentSpaceId": "", "title": "t", "assets": {}}),
            400,
            "ValidationException",
            "agentSpaceId",
        ),
        (
            "the empty title",
            json!({"agentSpaceId": agent_space_id, "title": "", "assets": {}}),
            400,
            "ValidationException",
            "title",
        ),
        (
            "a title of spaces alone",
            json!({"agentSpaceId": agent_space_id, "title": "   ", "assets": {}}),
            400,
            "ValidationException",
            "title",
        ),
        (
            "a title of a tab and a newline",
            json!({"agentSpaceId": agent_space_id, "title": "\t\n", "assets": {}}),
            400,
            "ValidationException",
            "title",
        ),
        (
            "a budget cap of zero",
            json!({"agentSpaceId": agent_space_id, "title": "t", "assets": {}, "maxTaskHours": 0.0}),
            400,
            "ValidationException",
            "maxTaskHours",
        ),
        (
            "a negative budget cap",
            json!({"agentSpaceId": agent_space_id, "title": "t", "assets": {}, "maxTaskHours": -1.0}),
            400,
            "ValidationException",
            "maxTaskHours",
        ),
        (
            "a budget cap just below zero",
            json!({"agentSpaceId": agent_space_id, "title": "t", "assets": {}, "maxTaskHours": -0.000_1}),
            400,
            "ValidationException",
            "maxTaskHours",
        ),
    ];

    for (case, request, status, code, path) in refusals {
        let answer = call(&router, "CreateCodeReview", &request).await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected a {status} refusal, got {} ({}) with {}",
            answer.status, answer.error_code, answer.body
        );
        assert_eq!(
            answer.error_code, code,
            "case {case:?}: expected the {code} error code, got {:?} with {}",
            answer.error_code, answer.body
        );
        let message = member(&answer.body, "message")
            .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no message: {}", answer.body));
        if path.is_empty() {
            // A space the account does not hold: the fixed sentence the agent space API
            // states, carrying nothing the request sent.
            assert_eq!(
                message, AGENT_SPACE_NOT_FOUND,
                "case {case:?}: expected the service's fixed not-found sentence, got {message:?}"
            );
        } else {
            // A member the service cannot accept: the sentence names the member and the
            // constraint it failed, and the `fieldList` a client reads carries the same.
            assert!(
                message.starts_with(&format!(
                    "1 validation error detected: value for '{path}' failed to satisfy constraint: "
                )),
                "case {case:?}: expected the refusal to name {path:?} and its constraint, \
                 got {message:?}"
            );
            let fields = array(&answer.body, "fieldList", case);
            assert_eq!(
                fields.len(),
                1,
                "case {case:?}: expected one field entry, got {}",
                answer.body
            );
            assert_eq!(
                member(&fields[0], "path").as_deref(),
                Some(path),
                "case {case:?}: expected the field entry to name {path:?}, got {}",
                answer.body
            );
        }
    }

    // Nothing the refusals sent reached the store: the space holds no code review at all,
    // and the ids the rule ADMITS are stored — the boundary of the budget cap rule, just
    // above zero, and a whole number.
    let accepted: Vec<(&str, f64)> = vec![
        ("the smallest cap above zero", 0.000_1),
        ("a whole number of hours", 1.0),
        ("a fractional number of hours", 7.75),
    ];
    let mut stored: Vec<String> = Vec::new();
    for (case, hours) in accepted {
        let created = ok(
            &router,
            "CreateCodeReview",
            &json!({
                "agentSpaceId": agent_space_id,
                "title": format!("accepted {case}"),
                "assets": {},
                "maxTaskHours": hours,
            }),
            case,
        )
        .await;
        assert_eq!(
            created.get("maxTaskHours").and_then(Value::as_f64),
            Some(hours),
            "case {case:?}: expected the stored cap {hours}, got {created}"
        );
        stored.push(member(&created, "codeReviewId").expect("the create returns an id"));
    }

    let payload = batch_get(&router, &agent_space_id, &stored, "after the refusals").await;
    let resolved = array(&payload, "codeReviews", "after the refusals");
    assert_eq!(
        resolved.len(),
        stored.len(),
        "expected only the accepted creates to be stored, got {payload}"
    );
}

/// RULE: a caller-supplied collection the model leaves unbounded is bounded by the
/// service: an asset list of at most the ceiling many entries is stored and read back
/// whole, and one entry past it is refused naming the member — so neither the store nor a
/// later response can be grown without end by one request.
#[tokio::test]
async fn an_asset_list_past_the_ceiling_is_refused() {
    let router = emulator();
    let agent_space_id = create_agent_space(&router, "asset-ceiling", "seed").await;

    // One row per asset list the model declares (`smithy_query(query="shape",
    // shape="Assets",model_path="services/service-com-amazonaws-securityagent-securityagent/smithy-model/com-amazonaws-securityagent-securityagent.json")`),
    // each built at a size: the ceiling, which is stored, and one past it, which is not.
    let entry_of = |list_name: &str, index: usize| -> Value {
        match list_name {
            "endpoints" => json!({"uri": format!("https://host-{index}.example.com")}),
            "actors" => json!({"identifier": format!("actor-{index}")}),
            "documents" => json!({"s3Location": format!("s3://sec-agent/doc-{index}.pdf")}),
            "sourceCode" => json!({"s3Location": format!("s3://sec-agent/src-{index}.zip")}),
            "integratedRepositories" => json!({
                "integrationId": format!("int-{index}"),
                "providerResourceId": format!("repo-{index}"),
            }),
            _ => json!({"source": {"s3Location": format!("s3://sec-agent/ca-{index}.pem")}}),
        }
    };
    let members = [
        "endpoints",
        "actors",
        "documents",
        "sourceCode",
        "integratedRepositories",
        "trustedCaCertificates",
    ];

    for member_name in members {
        for (case, entries, accepted) in [
            (
                format!("{member_name} at the ceiling"),
                MAX_CODE_REVIEW_ASSET_ENTRIES,
                true,
            ),
            (
                format!("{member_name} one past the ceiling"),
                MAX_CODE_REVIEW_ASSET_ENTRIES + 1,
                false,
            ),
        ] {
            let list: Vec<Value> = (0..entries).map(|index| entry_of(member_name, index)).collect();
            let request = json!({
                "agentSpaceId": agent_space_id,
                "title": format!("assets {case}"),
                "assets": {member_name: list},
            });
            let answer = call(&router, "CreateCodeReview", &request).await;
            if accepted {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the ceiling to be storable, got {} ({}) with {}",
                    answer.status, answer.error_code, answer.body
                );
                // Stored whole: the read reports every entry the create carried.
                let id = member(&answer.body, "codeReviewId")
                    .unwrap_or_else(|| panic!("case {case:?}: the create returned no id"));
                let payload =
                    batch_get(&router, &agent_space_id, std::slice::from_ref(&id), &case).await;
                let records = array(&payload, "codeReviews", &case);
                let stored = records
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: the review must read back"));
                assert_eq!(
                    stored
                        .get("assets")
                        .and_then(|assets| assets.get(member_name))
                        .and_then(Value::as_array)
                        .map(Vec::len),
                    Some(entries),
                    "case {case:?}: expected the stored list to hold {entries} entries, \
                     got {stored}"
                );
            } else {
                assert_eq!(
                    answer.status, 400,
                    "case {case:?}: expected a 400 past the ceiling, got {} ({}) with {}",
                    answer.status, answer.error_code, answer.body
                );
                assert_eq!(
                    answer.error_code, "ValidationException",
                    "case {case:?}: expected the ValidationException error code, got {:?} \
                     with {}",
                    answer.error_code, answer.body
                );
                let path = format!("assets.{member_name}");
                assert!(
                    member(&answer.body, "message").is_some_and(|message| {
                        message.contains(&path)
                            && message.contains(&MAX_CODE_REVIEW_ASSET_ENTRIES.to_string())
                    }),
                    "case {case:?}: expected the refusal to name {path:?} and the ceiling \
                     {MAX_CODE_REVIEW_ASSET_ENTRIES}, got {}",
                    answer.body
                );
            }
        }
    }
}

/// RULE: every code review is a stored record, so how many ONE agent space holds is
/// bounded: at the quota a further create is refused with the service's
/// `ServiceQuotaExceededException` (HTTP 402) naming the limit, the store is left as it
/// was, and a NEIGHBOURING space is unaffected because the quota is per agent space.
#[tokio::test]
async fn a_create_past_the_agent_space_quota_is_refused() {
    let router = emulator();
    let full = create_agent_space(&router, "at-the-quota", "seed").await;
    let spare = create_agent_space(&router, "below-the-quota", "seed").await;

    let mut stored: Vec<String> = Vec::new();
    for index in 0..MAX_CODE_REVIEWS_PER_AGENT_SPACE {
        let created = ok(
            &router,
            "CreateCodeReview",
            &json!({
                "agentSpaceId": full,
                "title": format!("review-{index}"),
                "assets": {},
            }),
            "filling the quota",
        )
        .await;
        stored.push(member(&created, "codeReviewId").expect("the create returns an id"));
    }

    let answer = call(
        &router,
        "CreateCodeReview",
        &json!({"agentSpaceId": full, "title": "one too many", "assets": {}}),
    )
    .await;
    assert_eq!(
        answer.status, 402,
        "expected a 402 past the quota, got {} ({}) with {}",
        answer.status, answer.error_code, answer.body
    );
    assert_eq!(
        answer.error_code, "ServiceQuotaExceededException",
        "expected the ServiceQuotaExceededException error code, got {:?} with {}",
        answer.error_code, answer.body
    );
    assert!(
        member(&answer.body, "message")
            .is_some_and(|message| message.contains(&MAX_CODE_REVIEWS_PER_AGENT_SPACE.to_string())),
        "expected the refusal to name the limit {MAX_CODE_REVIEWS_PER_AGENT_SPACE}, got {}",
        answer.body
    );

    // The refused create stored nothing: the space still resolves exactly the records it
    // held, and no more.
    let payload = batch_get(&router, &full, &stored, "after the quota refusal").await;
    assert_eq!(
        array(&payload, "codeReviews", "after the quota refusal").len(),
        stored.len(),
        "expected the store to be left as it was, got {} records",
        array(&payload, "codeReviews", "after the quota refusal").len()
    );

    // The quota is per agent space, so the neighbouring space still accepts a create.
    let created = ok(
        &router,
        "CreateCodeReview",
        &json!({"agentSpaceId": spare, "title": "room to spare", "assets": {}}),
        "the neighbouring space",
    )
    .await;
    assert_eq!(
        member(&created, "agentSpaceId").as_deref(),
        Some(spare.as_str()),
        "expected the create to land in the neighbouring space, got {created}"
    );
}
