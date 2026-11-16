// SPDX-License-Identifier: CC0-1.0

//! The FORM of every identifier this service mints for itself.
//!
//! RULE: an opaque identifier Entity Resolution generates — a matching job's `jobId` and
//! a resolved group's `matchId` — is served as exactly 32 lowercase hexadecimal
//! characters and nothing else, unless the model fixes a different form for that shape,
//! in which case the modelled form wins (`PolicyToken` is a 36-character hyphenated
//! grouping by its own pattern,
//! `smithy_query(query="shape",shape="PolicyToken")`).
//!
//! A `matchId` reaches a client through three surfaces — the `MatchID` column of a
//! matching job's output table, `GenerateMatchId`'s `MatchGroup$matchId` and
//! `GetMatchId`'s `matchId` — and all three carry the one id the run or the index
//! minted, so the form is asserted on each of them rather than on the generator alone.
//! Beside the form, the identity invariants the id exists for are asserted too: records
//! of one group share their id and two groups never do.
//!
//! The tables span what the rule generalises over: how many records a group holds and
//! how many groups a run resolves (including the single-record run and the group reached
//! transitively through two rules), whether the input source asks for normalization, the
//! whole modelled `ProcessingType` enum plus the absent member on the real-time path,
//! and the one shape whose modelled pattern keeps its hyphens. The recorded case — four
//! records in two groups of two, matched on one email key — is one row of the first
//! table.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::matching_run::{
    glue_table_arn, id_email_output, id_email_phone_fields, rule_based, workflow_request, Emulator,
    OutputTable, INPUT_BUCKET, OUTPUT_BUCKET, OUTPUT_PREFIX,
};

/// How many characters a generated id carries: the width the sibling modelled `JobId`
/// fixes (`^[a-f0-9]{32}$`, `smithy_query(query="shape",shape="JobId")`) and the width
/// this service serves a `matchId` in.
const ID_LENGTH: usize = 32;

/// How long the modelled `PolicyToken` is (`length 36..36`,
/// `smithy_query(query="shape",shape="PolicyToken")`): the one minted value whose own
/// pattern keeps the hyphens.
const POLICY_TOKEN_LENGTH: usize = 36;

/// Assert that `value` — the identifier `member` of `case` — is in the form a generated
/// id is served in: `ID_LENGTH` lowercase hexadecimal characters and nothing else.
fn assert_hex_id(case: &str, member: &str, value: &str) {
    assert_eq!(
        value.len(),
        ID_LENGTH,
        "case {case:?}: {member} must be {ID_LENGTH} characters long, got {value:?} \
         ({} characters)",
        value.len()
    );
    assert!(
        value.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')),
        "case {case:?}: {member} must be lowercase hexadecimal with no separator, got \
         {value:?}"
    );
}

/// What one input record must be reported as: its `id` column and a label for the group
/// it belongs to — records sharing a label must share one `matchId`, and two labels must
/// never share one.
#[derive(Debug, Clone, Copy)]
struct Row {
    id: &'static str,
    group: &'static str,
}

/// One row of the matching-job table.
struct JobCase {
    case: &'static str,
    /// The object staged under this case's input table location.
    object: String,
    /// `ruleBasedProperties.rules`.
    rules: Value,
    /// `InputSource$applyNormalization`.
    normalize: Option<bool>,
    /// Every record the run must report, by its `id` column.
    expected: Vec<Row>,
}

/// The rows of the matching-job table: how many records share a group, how many groups a
/// run resolves, how many rules connect them, and whether the source normalizes.
fn job_cases() -> Vec<JobCase> {
    vec![
        JobCase {
            case: "four records in two groups of two, matched on one email key",
            object: "id,email,phone\n\
                     r1,dup@example.com,555-0100\n\
                     r2,dup@example.com,555-0101\n\
                     r3,other@example.com,555-0102\n\
                     r4,other@example.com,555-0103\n"
                .to_string(),
            rules: json!([{ "ruleName": "emailRule", "matchingKeys": ["email"] }]),
            normalize: None,
            expected: vec![
                Row { id: "r1", group: "a" },
                Row { id: "r2", group: "a" },
                Row { id: "r3", group: "b" },
                Row { id: "r4", group: "b" },
            ],
        },
        JobCase {
            case: "one record, so one group of one",
            object: "id,email,phone\n\
                     r1,solo@example.com,555-0100\n"
                .to_string(),
            rules: json!([{ "ruleName": "emailRule", "matchingKeys": ["email"] }]),
            normalize: None,
            expected: vec![Row { id: "r1", group: "a" }],
        },
        JobCase {
            case: "a group of three beside a singleton",
            object: "id,email,phone\n\
                     r1,trio@example.com,555-0100\n\
                     r2,trio@example.com,555-0101\n\
                     r3,trio@example.com,555-0102\n\
                     r4,lone@example.com,555-0103\n"
                .to_string(),
            rules: json!([{ "ruleName": "emailRule", "matchingKeys": ["email"] }]),
            normalize: None,
            expected: vec![
                Row { id: "r1", group: "a" },
                Row { id: "r2", group: "a" },
                Row { id: "r3", group: "a" },
                Row { id: "r4", group: "b" },
            ],
        },
        JobCase {
            case: "two rules: a group reached transitively still carries ONE id",
            object: "id,email,phone\n\
                     r1,ada@example.com,555-0100\n\
                     r2,ada@example.com,555-0200\n\
                     r3,zoe@example.com,555-0200\n\
                     r4,quinn@example.com,555-0900\n"
                .to_string(),
            rules: json!([
                { "ruleName": "emailRule", "matchingKeys": ["email"] },
                { "ruleName": "phoneRule", "matchingKeys": ["phone"] },
            ]),
            normalize: None,
            expected: vec![
                Row { id: "r1", group: "a" },
                Row { id: "r2", group: "a" },
                Row { id: "r3", group: "a" },
                Row { id: "r4", group: "b" },
            ],
        },
        JobCase {
            case: "a normalizing source groups across case and spacing",
            object: "id,email,phone\n\
                     r1,Ada@Example.com,555-0100\n\
                     r2,\"  ada@example.com \",555-0200\n\
                     r3,bob@example.com,555-0300\n"
                .to_string(),
            rules: json!([{ "ruleName": "emailRule", "matchingKeys": ["email"] }]),
            normalize: Some(true),
            expected: vec![
                Row { id: "r1", group: "a" },
                Row { id: "r2", group: "a" },
                Row { id: "r3", group: "b" },
            ],
        },
    ]
}

#[tokio::test]
async fn a_matching_job_mints_its_job_id_and_every_match_id_as_32_lowercase_hex() {
    let emulator = Emulator::new();
    emulator.given_bucket(INPUT_BUCKET).await;
    emulator.given_bucket(OUTPUT_BUCKET).await;
    emulator.given_database().await;

    for (index, case) in job_cases().into_iter().enumerate() {
        let label = case.case;
        let table = format!("ids{index}");
        let schema = format!("idschema{index}");
        let workflow = format!("idworkflow{index}");
        let prefix = format!("in/{table}/");
        emulator
            .given_object(INPUT_BUCKET, &format!("{prefix}people.csv"), &case.object)
            .await;
        emulator
            .given_table(
                &table,
                &format!("s3://{INPUT_BUCKET}/{prefix}"),
                &["id", "email", "phone"],
                None,
            )
            .await;
        emulator
            .given_schema_mapping(&schema, &id_email_phone_fields())
            .await;
        let created = emulator
            .create_workflow(&workflow_request(
                &workflow,
                &schema,
                &table,
                case.normalize,
                id_email_output(),
                &format!("s3://{OUTPUT_BUCKET}/{OUTPUT_PREFIX}"),
                rule_based(case.rules.clone()),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the workflow must be created, got {created:?}"
        );

        let started = emulator.start_job(&workflow).await;
        assert_eq!(
            started.status, 200,
            "case {label:?}: StartMatchingJob must answer 200, got {started:?}"
        );
        let job_id = started.text("jobId");
        assert_hex_id(label, "StartMatchingJob's jobId", &job_id);

        let settled = emulator.settle(label, &workflow, &job_id).await;
        assert_eq!(
            settled.text("status"),
            "SUCCEEDED",
            "case {label:?}: the run must succeed, got {:?}",
            settled.body
        );

        let keys = emulator
            .keys_under(OUTPUT_BUCKET, &format!("{OUTPUT_PREFIX}/{workflow}/"))
            .await;
        assert_eq!(
            keys.len(),
            1,
            "case {label:?}: the run must write exactly one output object, got {keys:?}"
        );
        let document = emulator
            .object_text(OUTPUT_BUCKET, &keys[0])
            .await
            .unwrap_or_else(|| panic!("case {label:?}: the output object must be readable"));
        let output = OutputTable::parse(label, &document);
        assert_eq!(
            output.rows.len(),
            case.expected.len(),
            "case {label:?}: the output table must hold one row per processed record, got \
             {:?}",
            output.rows
        );

        // Every row's `MatchID` is in the served form, equal within a group and distinct
        // across groups — the join key a client reads the table by.
        let mut id_of_group: BTreeMap<&str, String> = BTreeMap::new();
        for record in &case.expected {
            let row = output.row(record.id).unwrap_or_else(|| {
                panic!(
                    "case {label:?}: the output table must hold the record {:?}, got {:?}",
                    record.id, output.rows
                )
            });
            let match_id = row.get("MatchID").cloned().unwrap_or_default();
            assert_hex_id(
                label,
                &format!("the MatchID column of record {:?}", record.id),
                &match_id,
            );
            match id_of_group.get(record.group) {
                None => {
                    assert!(
                        !id_of_group.values().any(|held| *held == match_id),
                        "case {label:?}: group {:?} must not share the matchId {match_id:?} \
                         with another group",
                        record.group
                    );
                    id_of_group.insert(record.group, match_id);
                }
                Some(held) => assert_eq!(
                    *held, match_id,
                    "case {label:?}: record {:?} must carry its group's matchId, got \
                     {match_id:?}",
                    record.id
                ),
            }
        }
        let groups: BTreeSet<&str> = case.expected.iter().map(|record| record.group).collect();
        assert_eq!(
            id_of_group.len(),
            groups.len(),
            "case {label:?}: the table must carry one distinct matchId per group, got {:?}",
            id_of_group
        );
    }
}

/// The whole modelled `ProcessingType` enum
/// (`smithy_query(query="shape",shape="ProcessingType")` → `CONSISTENT | EVENTUAL |
/// EVENTUAL_NO_LOOKUP`) plus the absent member, which the model documents as
/// `CONSISTENT`.
const PROCESSING_TYPES: &[Option<&str>] = &[
    None,
    Some("CONSISTENT"),
    Some("EVENTUAL"),
    Some("EVENTUAL_NO_LOOKUP"),
];

#[tokio::test]
async fn the_real_time_pair_serves_match_ids_in_the_same_32_lowercase_hex_form() {
    let emulator = Emulator::new();
    let table = "realtime";
    let schema = "realtimeschema";
    emulator
        .given_schema_mapping(schema, &id_email_phone_fields())
        .await;

    for (index, processing_type) in PROCESSING_TYPES.iter().enumerate() {
        let label = processing_type.unwrap_or("processingType absent");
        let workflow = format!("realtime{index}");
        let created = emulator
            .create_workflow(&workflow_request(
                &workflow,
                schema,
                table,
                None,
                id_email_output(),
                &format!("s3://{OUTPUT_BUCKET}/{OUTPUT_PREFIX}"),
                rule_based(json!([{ "ruleName": "emailRule", "matchingKeys": ["email"] }])),
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the workflow must be created, got {created:?}"
        );

        // Two records sharing the matching key, submitted one after the other: the
        // second joins the first's group under every mode that looks up, and opens its
        // own under the mode that does not. Either way both ids are in the served form.
        let mut answered: Vec<String> = Vec::new();
        for unique_id in ["rt1", "rt2"] {
            let mut body = json!({
                "records": [{
                    "inputSourceARN": glue_table_arn(table),
                    "uniqueId": unique_id,
                    "recordAttributeMap": { "email": "rt@example.com" },
                }],
            });
            if let Some(mode) = processing_type {
                body["processingType"] = json!(mode);
            }
            let reply = emulator
                .call(
                    "POST",
                    &format!("/matchingworkflows/{workflow}/generateMatches"),
                    Some(&body),
                )
                .await;
            assert_eq!(
                reply.status, 200,
                "case {label:?}: GenerateMatchId must answer 200, got {reply:?}"
            );
            let groups = reply
                .body
                .get("matchGroups")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                !groups.is_empty(),
                "case {label:?}: GenerateMatchId must report a match group, got {:?}",
                reply.body
            );
            for group in &groups {
                let match_id = group
                    .get("matchId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert_hex_id(label, "MatchGroup$matchId", &match_id);
                answered.push(match_id);
            }
        }

        // `GetMatchId` reads the same index, so it answers one of the ids already served
        // — in the same form.
        let read = emulator
            .get_match_id(&workflow, json!({ "email": "rt@example.com" }))
            .await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: GetMatchId must answer 200, got {read:?}"
        );
        let read_id = read.text("matchId");
        assert_hex_id(label, "GetMatchId's matchId", &read_id);
        assert!(
            answered.contains(&read_id),
            "case {label:?}: GetMatchId must answer an id the writer minted ({answered:?}), \
             got {read_id:?}"
        );
    }
}

#[tokio::test]
async fn a_policy_token_keeps_the_hyphenated_form_its_own_pattern_fixes() {
    let emulator = crate::schema_mapping_workflow_lifecycle::Emulator::new();
    let created = emulator
        .create_schema_mapping(&json!({
            "schemaName": "tokenform",
            "mappedInputFields": [
                { "fieldName": "id", "type": "UNIQUE_ID" },
                { "fieldName": "name", "type": "NAME" },
            ],
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "the schema mapping must be created, got {created:?}"
    );
    let arn = crate::schema_mapping_workflow_lifecycle::string_member(&created.body, "schemaArn");

    let put = crate::resource_policy::put_policy(
        &emulator,
        &arn,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "tokenform",
                "Effect": "Allow",
                "Principal": { "AWS": "*" },
                "Action": "entityresolution:GetSchemaMapping",
                "Resource": arn,
            }],
        })
        .to_string(),
        None,
    )
    .await;
    assert_eq!(put.status, 200, "PutPolicy must answer 200, got {put:?}");
    let token = crate::schema_mapping_workflow_lifecycle::string_member(&put.body, "token");
    // The modelled `PolicyToken` form is read from the suite that owns it, so the one
    // minted value whose pattern KEEPS its separators is checked against one reading of
    // that pattern: stripping them here would be the defect rather than the fix.
    assert!(
        crate::resource_policy::is_modelled_policy_token(&token),
        "the policy token must keep the modelled hyphenated form of \
         {POLICY_TOKEN_LENGTH} characters, got {token:?}"
    );
    assert!(
        token.contains('-'),
        "the policy token must carry the separators its own pattern fixes, got {token:?}"
    );
}
