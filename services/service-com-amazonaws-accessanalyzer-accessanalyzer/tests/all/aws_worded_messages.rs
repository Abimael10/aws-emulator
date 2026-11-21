// SPDX-License-Identifier: CC0-1.0

//! The TEXT this service puts on the wire for four message templates, driven end to end
//! through the real router.
//!
//! Each rule is asserted over a table that spans the class the template generalises over,
//! never over one request, and every expected string is written out here as a literal — so
//! a row cannot be satisfied by reading the implementation's own wording back:
//!
//! 1. `ValidatePolicy` reports a document that is not parseable JSON as one `ERROR`
//!    finding whose `findingDetails` is the service's FIXED syntax-error sentence, with
//!    the position of the parse failure as its only variable part, stated in the modelled
//!    `Position` coordinates (`offset` from 0, `line` from 1, `column` from 0 —
//!    `smithy_query(query="shape",shape="Position")`). No JSON parser's own diagnostic
//!    appears in it.
//! 2. A `jobId` that names no policy generation job is refused — on every operation that
//!    resolves a job — with ONE template naming the submitted id.
//! 3. A value outside a member's modelled `@pattern` is refused with the service's short
//!    `Invalid <member>: <value>` sentence, which never carries the pattern itself.
//! 4. A request asking a job to move into a status its CURRENT status does not allow is
//!    refused with the service's illegal-status-change sentence, naming the identifier
//!    from the request and both statuses — the current one read from the service's own
//!    state. It stays distinct from template 2, which the same operations render for an
//!    identifier that names nothing.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_accessanalyzer_accessanalyzer::state::{
    policy_generation_illegal_status_change, JOB_STATUS_CANCELED, JOB_STATUS_IN_PROGRESS,
};

use crate::fixed_refusal_texts::message;
use crate::harness::{
    member, named_fields, policy_generation_body, role_arn, start_policy_generation_ok, trail_arn,
    user_arn, wire_reason, Emulator, Reply,
};

/// The fixed halves of the syntax-error sentence, either side of the position triple.
const SYNTAX_ERROR_HEAD: &str = "Fix the JSON syntax error at index ";

/// The issue code a document that is not JSON is reported under, from the model's
/// `IssueCode` space as this service's checks name it.
const JSON_SYNTAX_ERROR: &str = "JSON_SYNTAX_ERROR";

/// The `ValidatePolicyFindingType` a non-functional policy is reported as, from the
/// model's own enum (`smithy_query(query="shape",shape="ValidatePolicyFindingType")`).
const ERROR: &str = "ERROR";

/// Vocabulary that belongs to a JSON PARSER rather than to this service: the diagnostics
/// `serde_json` and `jsonc-parser` phrase their failures with, plus the wording of the
/// framework's generic constraint report. None of it may reach a response.
const PARSER_VOCABULARY: &[&str] = &[
    "key must be a string",
    "expected value",
    "EOF while parsing",
    "trailing characters",
    "not valid JSON",
    "validation error detected",
    "regular expression pattern",
];

/// One malformed-document row: the document, and the text it stops being JSON AT.
///
/// `stops_at` is a marker the test resolves to a character index itself, so the expected
/// `index` is stated independently of whichever parser decided the document was invalid.
#[derive(Debug)]
struct MalformedCase {
    case: &'static str,
    document: &'static str,
    /// The first occurrence of this text is where the document stops being JSON, or
    /// `None` for a document whose stop position no reader of the row can state (a
    /// truncation, which stops at the end).
    stops_at: Option<&'static str>,
}

/// The `policyType` values, from the model's `PolicyType` enum
/// (`smithy_query(query="shape",shape="PolicyType")`).
const POLICY_TYPES: &[&str] = &[
    "IDENTITY_POLICY",
    "RESOURCE_POLICY",
    "SERVICE_CONTROL_POLICY",
    "RESOURCE_CONTROL_POLICY",
];

/// `ValidatePolicy` for a document and a policy type.
async fn validate(emulator: &Emulator, document: &str, policy_type: &str) -> Reply {
    emulator
        .send(
            "POST",
            "/policy/validation",
            Some(json!({ "policyDocument": document, "policyType": policy_type })),
        )
        .await
}

/// The line and column of a character offset, recomputed from the document: line from 1,
/// column from 0, as the model documents `Position`.
fn position_at(document: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 0usize;
    for character in document.chars().take(offset) {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// How many characters precede the first occurrence of `marker` in `document`.
fn character_index_of(document: &str, marker: &str) -> usize {
    let byte = document
        .find(marker)
        .unwrap_or_else(|| panic!("{marker:?} must occur in {document:?}"));
    document[..byte].chars().count()
}

/// A required integer member of a reported position.
fn integer(value: &Value, member: &str) -> usize {
    usize::try_from(
        value
            .get(member)
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("{member} is required, but is missing from {value}")),
    )
    .unwrap_or_else(|_| panic!("{member} of {value} must not be negative"))
}

/// RULE 1: a document that is not parseable JSON is answered 200 with one `ERROR` finding
/// whose details are the service's fixed syntax-error sentence, carrying the position of
/// the parse failure and nothing of the parser that found it.
///
/// The rows differ along the FORM of the syntax error (a quoting the grammar refuses, an
/// unquoted key, a comment, a trailing comma, a document that is not JSON at all, a
/// truncation, the empty document) and along WHERE the document stops being JSON — on the
/// first character, inside the first line, and on a later line of a multi-line document —
/// which is what the sentence's three variable parts report. The recorded case is a
/// first-line stop at index 1 (the `single-quoted property name` row).
#[tokio::test]
async fn an_unparseable_document_reports_the_fixed_syntax_error_sentence() {
    let cases = &[
        MalformedCase {
            case: "a single-quoted property name: the document stops at the quote",
            document: r#"{'Version':'2012-10-17'}"#,
            stops_at: Some("'Version'"),
        },
        MalformedCase {
            case: "an unquoted property name",
            document: r#"{Version:"2012-10-17"}"#,
            stops_at: Some("Version"),
        },
        MalformedCase {
            case: "not JSON at all, so it stops at its very first character",
            document: "allow everything",
            stops_at: Some("allow"),
        },
        MalformedCase {
            case: "a comment before the document",
            document: "// the policy\n{\"Version\":\"2012-10-17\"}",
            stops_at: Some("//"),
        },
        MalformedCase {
            case: "a single-quoted key on the THIRD line of a multi-line document",
            document: "{\n  \"Version\": \"2012-10-17\",\n  'Statement': []\n}",
            stops_at: Some("'Statement'"),
        },
        MalformedCase {
            case: "a trailing comma, which stops at the brace that follows it",
            document: r#"{"Version":"2012-10-17","Statement":[],}"#,
            stops_at: Some("}"),
        },
        MalformedCase {
            case: "the empty document",
            document: "",
            stops_at: Some(""),
        },
        MalformedCase {
            case: "a truncated object",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow""#,
            stops_at: None,
        },
    ];

    let emulator = Emulator::new();
    for row in cases {
        for policy_type in POLICY_TYPES {
            let case = format!("{} ({policy_type})", row.case);
            let reply = validate(&emulator, row.document, policy_type).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: a malformed document is reported as a finding, got {reply:?}"
            );
            let findings = reply
                .member("findings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                findings.len(),
                1,
                "case {case:?}: expected exactly one finding, got {findings:?}"
            );
            let finding = &findings[0];
            assert_eq!(
                (
                    finding.get("issueCode").and_then(Value::as_str),
                    finding.get("findingType").and_then(Value::as_str)
                ),
                (Some(JSON_SYNTAX_ERROR), Some(ERROR)),
                "case {case:?}: expected an {ERROR} {JSON_SYNTAX_ERROR} finding, got {finding:?}"
            );

            let details = finding
                .get("findingDetails")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // The position the finding's own location reports is the one the sentence
            // must state, so the two cannot disagree.
            let start = finding
                .get("locations")
                .and_then(Value::as_array)
                .and_then(|locations| locations.first())
                .and_then(|location| location.get("span"))
                .and_then(|span| span.get("start"))
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: expected a span start in {finding:?}"));
            let offset = integer(&start, "offset");
            let line = integer(&start, "line");
            let column = integer(&start, "column");
            let expected = format!("{SYNTAX_ERROR_HEAD}{offset} line {line} column {column}.");
            assert_eq!(
                details, expected,
                "case {case:?}: expected the fixed sentence {expected:?}, got {details:?}"
            );

            // The triple is a real position in the submitted document: its line and
            // column follow from its index, recomputed here without the handler.
            let characters = row.document.chars().count();
            assert!(
                offset <= characters,
                "case {case:?}: index {offset} is past the {characters}-character document"
            );
            assert_eq!(
                (line, column),
                position_at(row.document, offset),
                "case {case:?}: line {line} column {column} does not follow from index {offset}"
            );
            // And it is the position the ROW states the document stops being JSON at.
            if let Some(marker) = row.stops_at {
                let expected_index = character_index_of(row.document, marker);
                assert_eq!(
                    offset, expected_index,
                    "case {case:?}: expected the stop at index {expected_index} (at {marker:?}), \
                     got {offset}"
                );
            }

            // No parser's diagnostic, and none of the framework's constraint prose,
            // reaches the wire.
            for leaked in PARSER_VOCABULARY {
                assert!(
                    !details.contains(leaked),
                    "case {case:?}: the details must carry no parser wording, but {details:?} \
                     contains {leaked:?}"
                );
            }
        }
    }

    // The boundary of the rule: a document that IS parseable reports no syntax error.
    let good = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:GetObject",
            "Resource": "arn:aws:s3:::reports/*",
        }],
    })
    .to_string();
    let reply = validate(&emulator, &good, "IDENTITY_POLICY").await;
    let codes: Vec<&str> = reply
        .member("findings")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter_map(|finding| finding.get("issueCode").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !codes.contains(&JSON_SYNTAX_ERROR),
        "a parseable document must report no {JSON_SYNTAX_ERROR} finding, got {codes:?}"
    );
}

/// The two operations that resolve a `jobId` out of the path, so one table row is driven
/// on each: both must word an unknown job the same way.
#[derive(Debug, Clone, Copy)]
enum JobCall {
    Get,
    Cancel,
}

impl JobCall {
    async fn send(self, emulator: &Emulator, job_id: &str) -> Reply {
        match self {
            JobCall::Get => emulator.get_generated_policy(job_id, "").await,
            JobCall::Cancel => emulator.cancel_policy_generation(job_id).await,
        }
    }
}

/// Start a generation and answer the `jobId` it minted, so the table has a job that DOES
/// exist beside the ones that do not.
async fn start_generation(emulator: &Emulator, principal: &str) -> String {
    start_policy_generation_ok(
        emulator,
        "the fixture start",
        policy_generation_body(
            principal,
            &[json!({ "cloudTrailArn": trail_arn("audit") })],
            RANGE_START,
            None,
            None,
        ),
    )
    .await
}

/// RULE 2: a `jobId` that names no policy generation job is refused with ONE template —
/// `Invalid `, the member, `: `, the submitted id, ` does not exist` — on every operation
/// that resolves a job, whatever form the id takes.
///
/// The rows differ along the FORM of the unknown id (a UUID of the kind a start mints, a
/// UUID that differs from a live job's in one character, an id that is not a UUID at all,
/// a single character, a long id) — the id is the template's only variable part — and the
/// table is driven on both job-addressed operations.
#[tokio::test]
async fn an_unknown_job_id_is_refused_with_one_aws_worded_template() {
    let emulator = Emulator::new();
    // A job that DOES exist, so the table is not answering out of an empty store, and so
    // one row can differ from a live id by a single character.
    let live = start_generation(&emulator, &role_arn("message_template")).await;
    let near_miss: String = {
        let mut characters: Vec<char> = live.chars().collect();
        let last = characters.len() - 1;
        characters[last] = if characters[last] == 'a' { 'b' } else { 'a' };
        characters.into_iter().collect()
    };

    let unknown: Vec<(&str, String)> = vec![
        (
            "a UUID of the form a start mints",
            "acef2041-5fb4-4bdd-a201-aee420aa09d0".to_string(),
        ),
        (
            "a UUID differing from a live job's in its last character",
            near_miss.clone(),
        ),
        ("an id that is not a UUID at all", "no-such-job".to_string()),
        ("a single-character id", "j".to_string()),
        ("a long id", "job-".to_string() + &"9".repeat(80)),
    ];

    for (row, job_id) in &unknown {
        for call in [JobCall::Get, JobCall::Cancel] {
            let case = format!("{call:?} with {row}");
            let reply = call.send(&emulator, job_id).await;
            assert_eq!(
                reply.status, 400,
                "case {case:?}: an unknown job is HTTP 400, got {reply:?}"
            );
            assert_eq!(
                reply.code(),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {reply:?}"
            );
            let expected = format!("Invalid jobId: {job_id} does not exist");
            assert_eq!(
                message(&reply),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                message(&reply)
            );
            assert_eq!(
                reply.member("reason").and_then(Value::as_str),
                Some(wire_reason("fieldValidationFailed")),
                "case {case:?}: the refusal must carry the required reason, got {reply:?}"
            );
            let named = named_fields(&reply);
            assert_eq!(
                named,
                vec!["jobId".to_string()],
                "case {case:?}: expected fieldList to name jobId, got {reply:?}"
            );
        }
    }

    // The boundary of the rule: the job that exists is served, not refused — read back
    // through the operation the table drove over the unknown ids.
    let read = emulator.get_generated_policy(&live, "").await;
    assert_eq!(
        read.status, 200,
        "the job that exists must be served, got {read:?}"
    );
    assert_eq!(
        read.member("jobDetails")
            .and_then(|details| details.get("jobId"))
            .and_then(Value::as_str),
        Some(live.as_str()),
        "the read must report the job that was started, got {read:?}"
    );
    let canceled = emulator.cancel_policy_generation(&live).await;
    assert_eq!(
        canceled.status, 204,
        "the job that exists must be cancelable, got {canceled:?}"
    );
}

/// The member whose modelled `@pattern` rule 3 is asserted over, and the prefix of the
/// sentence that refuses it.
const PRINCIPAL_ARN: &str = "principalArn";

/// How many bytes of a rejected value a message echoes before it is reported by its
/// leading characters instead; past that the response would amplify the request.
const ECHO_LIMIT: usize = 256;

/// RULE 3: a value outside its member's modelled `@pattern` is refused with the service's
/// short sentence — `Invalid `, the member's wire name, `: `, the rejected value — and the
/// pattern itself never reaches the text.
///
/// The rows differ along HOW the value breaks the pattern
/// (`^arn:[^:]*:iam::[^:]*:(role|user)/.{1,576}$`, from
/// `smithy_query(query="shape",shape="PrincipalArn")`): not an ARN at all (the recorded
/// case), another service's ARN, an IAM ARN carrying a region, an entity type the pattern
/// does not list, an empty resource segment (below the published 1-character minimum), one
/// character past the published 576-character maximum, and a value long enough that the
/// echo is capped.
#[tokio::test]
async fn a_value_outside_a_modelled_pattern_is_refused_with_the_services_own_sentence() {
    let emulator = Emulator::new();
    let rejected: Vec<(&str, String)> = vec![
        ("not an ARN at all", "not-an-arn".to_string()),
        (
            "another service's ARN",
            "arn:aws:s3:::my-bucket".to_string(),
        ),
        (
            "an IAM ARN carrying a region, which the pattern forbids",
            "arn:aws:iam:us-east-1:123456789012:role/app".to_string(),
        ),
        (
            "an entity type the pattern does not list",
            "arn:aws:iam::123456789012:group/admins".to_string(),
        ),
        (
            "a role ARN with no resource segment (below the 1-character minimum)",
            "arn:aws:iam::123456789012:role/".to_string(),
        ),
        (
            "a role ARN one character past the 576-character maximum",
            format!("arn:aws:iam::123456789012:role/{}", "p".repeat(577)),
        ),
        (
            "a value long enough for the echo to be capped",
            format!("not-an-arn-{}", "z".repeat(ECHO_LIMIT * 2)),
        ),
    ];

    for (case, value) in &rejected {
        let reply = emulator
            .list_policy_generations(&format!("?principalArn={}", crate::harness::label(value)))
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a value outside the pattern is HTTP 400, got {reply:?}"
        );
        assert_eq!(
            reply.code(),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {reply:?}"
        );
        let text = message(&reply).to_string();
        let prefix = format!("Invalid {PRINCIPAL_ARN}: ");
        let shown = text.strip_prefix(prefix.as_str()).unwrap_or_else(|| {
            panic!("case {case:?}: expected a message starting {prefix:?}, got {text:?}")
        });
        if value.len() <= ECHO_LIMIT {
            assert_eq!(
                shown, value,
                "case {case:?}: the sentence must name the rejected value, got {text:?}"
            );
        } else {
            // A value past the echo cap is named by its leading characters.
            let head = shown.split("...").next().unwrap_or(shown);
            assert!(
                !head.is_empty() && value.starts_with(head),
                "case {case:?}: expected the leading characters of the rejected value, \
                 got {text:?}"
            );
        }
        // Neither the pattern nor the framework's constraint report reaches the text.
        for leaked in PARSER_VOCABULARY.iter().chain(["^arn:", "satisfy"].iter()) {
            assert!(
                !text.contains(leaked),
                "case {case:?}: the message must not carry {leaked:?}, got {text:?}"
            );
        }
        // `fieldList` still names the member a client has to correct.
        let named = named_fields(&reply);
        assert_eq!(
            named,
            vec![PRINCIPAL_ARN.to_string()],
            "case {case:?}: expected fieldList to name {PRINCIPAL_ARN}, got {reply:?}"
        );
    }

    // The boundary of the rule: a value the pattern accepts — at each published bound of
    // its resource segment — is not refused at all.
    for (case, value) in [
        ("a role ARN at the 1-character minimum", role_arn("a")),
        (
            "a user ARN at the 576-character maximum",
            format!(
                "arn:aws:iam::{}:user/{}",
                engine::emulator_account_id(),
                "u".repeat(576)
            ),
        ),
    ] {
        let reply = emulator
            .list_policy_generations(&format!("?principalArn={}", crate::harness::label(&value)))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a value inside the pattern must be served, got {reply:?}"
        );
    }
}

/// The `JobStatus` a generation reports while it is still running: the one status a cancel
/// can move a job out of. Taken from the crate's own constant for the model's enum value
/// (`smithy_query(query="shape",shape="JobStatus")` reports `IN_PROGRESS`, `SUCCEEDED`,
/// `FAILED`, `CANCELED`), so the table names the modelled status rather than a second copy
/// of the string; the SENTENCE the rows expect is still written out here as a literal.
const IN_PROGRESS: &str = JOB_STATUS_IN_PROGRESS;

/// The status a cancel asks a job to move TO, from that same enum.
const CANCELED: &str = JOB_STATUS_CANCELED;

/// The statuses of that enum a cancel is refused from because the job has already stopped
/// at one of them. `SUCCEEDED` is the one this emulator's clock produces on its own;
/// `FAILED` is the one a generation whose analysis could not run reports, and the template
/// has to render it the same way. (`CANCELED` is the fourth terminal value and is NOT in
/// this list: re-cancelling a job that is already `CANCELED` is accepted, because the
/// operation is `@idempotent`.)
const REFUSED_FROM_STATUSES: &[&str] = &["SUCCEEDED", "FAILED"];

/// How long a case keeps looking for the clock to owe a started generation its terminal
/// status before it gives up. Generous beside the generation's own span: the CONDITION is
/// polled until it holds, so a loaded machine only polls more often.
const TERMINAL_STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// 2026-01-01T00:00:00Z in epoch seconds: the start of every analysed range below.
const RANGE_START: f64 = 1_767_225_600.0;

/// One day of an analysed range, in the same unit.
const DAY: f64 = 86_400.0;

/// The `status` `GetGeneratedPolicy` reports for a job.
async fn reported_status(emulator: &Emulator, case: &str, job_id: &str) -> String {
    let reply = emulator.get_generated_policy(job_id, "").await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the job must be readable, got {reply:?}"
    );
    let details = reply
        .member("jobDetails")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected jobDetails, got {reply:?}"));
    member(&details, "status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a status, got {details:?}"))
        .to_string()
}

/// Look again until the job has left `IN_PROGRESS`, and answer the status it settled in.
///
/// The status comes off the wire, so the sentence under test is asserted against the status
/// this service itself reports rather than one the test assumed.
async fn await_terminal_status(emulator: &Emulator, case: &str, job_id: &str) -> String {
    let deadline = Instant::now() + TERMINAL_STATUS_DEADLINE;
    loop {
        let status = reported_status(emulator, case, job_id).await;
        if status != IN_PROGRESS {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the generation was still {IN_PROGRESS} after a generous wait"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `status` `ListPolicyGenerations` reports for `job_id` among the generations of
/// `principal_arn`, so the sentence can be checked against the status EVERY read of the one
/// record reports.
async fn listed_status(
    emulator: &Emulator,
    case: &str,
    principal_arn: &str,
    job_id: &str,
) -> String {
    let reply = emulator
        .list_policy_generations(&format!(
            "?principalArn={}",
            crate::harness::label(principal_arn)
        ))
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the listing must be served, got {reply:?}"
    );
    let entry = reply
        .member("policyGenerations")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("jobId").and_then(Value::as_str) == Some(job_id))
        })
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {job_id} in {reply:?}"));
    member(&entry, "status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a status, got {entry:?}"))
        .to_string()
}

/// RULE 4: a request asking a job to move into a status its CURRENT status does not allow
/// is refused with the service's illegal-status-change sentence — `Invalid jobId: `, the id
/// the request named, `. Illegal status change from `, the status the job is in, ` to `, the
/// status the call asked for — with the current status read from the service's own state.
///
/// The rows differ along what the template generalises over: which job it is (so the id,
/// its only request-borne variable part, differs), and the request form the job was started
/// from — the principal kind, how many trails are named and which optional members each
/// carries, whether the analysed range carries an end, and whether a `clientToken` was
/// carried — none of which may reach the sentence. The recorded case (one trail naming only
/// its ARN, a role principal, no analysed end) is the first row. Every job is started before
/// any is polled, so the whole table waits out ONE generation span.
#[tokio::test]
async fn a_cancel_of_a_job_in_a_terminal_status_reports_the_illegal_status_change() {
    let emulator = Emulator::new();
    let principal = role_arn("illegal_status_change");
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "one trail naming only its ARN, a role principal, no analysed end",
            principal.clone(),
            policy_generation_body(
                &principal,
                &[json!({ "cloudTrailArn": trail_arn("isc-recorded-trail") })],
                RANGE_START,
                None,
                None,
            ),
        ),
        (
            "three trails, a user principal, an explicit analysed end",
            user_arn("illegal_status_change_user"),
            policy_generation_body(
                &user_arn("illegal_status_change_user"),
                &[
                    json!({ "cloudTrailArn": trail_arn("isc-trail-a") }),
                    json!({ "cloudTrailArn": trail_arn("isc-trail-b"), "regions": ["us-west-2"] }),
                    json!({ "cloudTrailArn": trail_arn("isc-trail-c"), "allRegions": true }),
                ],
                RANGE_START + DAY,
                Some(RANGE_START + 2.0 * DAY),
                None,
            ),
        ),
        (
            "a start carrying a clientToken",
            role_arn("illegal_status_change_token"),
            policy_generation_body(
                &role_arn("illegal_status_change_token"),
                &[json!({ "cloudTrailArn": trail_arn("isc-token-trail") })],
                RANGE_START + 3.0 * DAY,
                None,
                Some("c2a2d6f4-5f0c-4a2e-9d0b-2f0a6d4c8e11"),
            ),
        ),
        (
            "a second job of the SAME principal, differing only in its id",
            principal.clone(),
            policy_generation_body(
                &principal,
                &[json!({ "cloudTrailArn": trail_arn("isc-recorded-trail") })],
                RANGE_START + 4.0 * DAY,
                None,
                None,
            ),
        ),
    ];

    let mut started: Vec<(&str, String, String)> = Vec::new();
    for (case, principal_arn, body) in rows {
        let job_id = start_policy_generation_ok(&emulator, case, body).await;
        started.push((case, principal_arn, job_id));
    }

    for (case, principal_arn, job_id) in &started {
        let status = await_terminal_status(&emulator, case, job_id).await;
        assert_eq!(
            listed_status(&emulator, case, principal_arn, job_id).await,
            status,
            "case {case:?}: the listing must report the status the read reports"
        );

        let refused = emulator.cancel_policy_generation(job_id).await;
        assert_eq!(
            (refused.status, refused.code()),
            (400, "ValidationException"),
            "case {case:?}: expected the modelled 400 ValidationException, got {refused:?}"
        );
        let expected =
            format!("Invalid jobId: {job_id}. Illegal status change from {status} to {CANCELED}");
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            message(&refused)
        );
        // The neighbouring template of the same operation must not be what answered.
        assert!(
            !message(&refused).contains("does not exist"),
            "case {case:?}: an existing job must not be refused as a missing one, got {refused:?}"
        );
        assert_eq!(
            refused.member("reason").and_then(Value::as_str),
            Some(wire_reason("fieldValidationFailed")),
            "case {case:?}: the refusal must carry the required reason, got {refused:?}"
        );
        assert_eq!(
            named_fields(&refused),
            vec!["jobId".to_string()],
            "case {case:?}: expected fieldList to name jobId, got {refused:?}"
        );

        // A refused cancel changed nothing, so the same call is refused with the same
        // sentence — the status it reports is the job's, not a step of its own.
        let again = emulator.cancel_policy_generation(job_id).await;
        assert_eq!(
            (again.status, again.code(), message(&again)),
            (400, "ValidationException", expected.as_str()),
            "case {case:?}: expected the refusal to be stable, got {again:?}"
        );
        assert_eq!(
            reported_status(&emulator, case, job_id).await,
            status,
            "case {case:?}: a refused cancel must leave the job in the status it was in"
        );
    }
}

/// The boundary of RULE 4: the status that DOES allow the change is not refused, and the
/// identifier that names no job at all keeps the other template.
///
/// Three cases, one per outcome the operation has beside the refusal above: a running job is
/// cancelled, a job already `CANCELED` is cancelled again (`@idempotent`, so the same
/// success and no message at all), and an id no start minted is refused as missing rather
/// than as an illegal status change.
#[tokio::test]
async fn the_status_that_allows_a_cancel_and_an_unknown_job_are_not_illegal_status_changes() {
    let emulator = Emulator::new();
    let principal = role_arn("illegal_status_change_boundary");
    let case = "a job cancelled while it is still running";
    let job_id = start_policy_generation_ok(
        &emulator,
        case,
        policy_generation_body(
            &principal,
            &[json!({ "cloudTrailArn": trail_arn("isc-boundary-trail") })],
            RANGE_START,
            None,
            None,
        ),
    )
    .await;
    assert_eq!(
        reported_status(&emulator, case, &job_id).await,
        IN_PROGRESS,
        "case {case:?}: a generation starts {IN_PROGRESS}"
    );
    let stopped = emulator.cancel_policy_generation(&job_id).await;
    assert_eq!(
        (stopped.status, stopped.code()),
        (204, ""),
        "case {case:?}: a running generation is cancelled, got {stopped:?}"
    );
    assert!(
        stopped.carries_no_content(),
        "case {case:?}: the success reports no content, got {stopped:?}"
    );
    assert_eq!(
        reported_status(&emulator, case, &job_id).await,
        CANCELED,
        "case {case:?}: the cancelled generation reports {CANCELED}"
    );

    let repeated_case = "a job that is already CANCELED, cancelled again";
    let repeated = emulator.cancel_policy_generation(&job_id).await;
    assert_eq!(
        (repeated.status, repeated.code()),
        (204, ""),
        "case {repeated_case:?}: the repeated cancel answers the same success, got {repeated:?}"
    );
    assert_eq!(
        message(&repeated),
        "",
        "case {repeated_case:?}: an accepted cancel carries no message, got {repeated:?}"
    );

    let unknown_case = "an id no start minted";
    let unknown = "11111111-2222-4333-8444-555555555555";
    let missing = emulator.cancel_policy_generation(unknown).await;
    assert_eq!(
        (missing.status, missing.code()),
        (400, "ValidationException"),
        "case {unknown_case:?}: expected the modelled 400 ValidationException, got {missing:?}"
    );
    let expected = format!("Invalid jobId: {unknown} does not exist");
    assert_eq!(
        message(&missing),
        expected,
        "case {unknown_case:?}: expected {expected:?}, got {:?}",
        message(&missing)
    );
    assert!(
        !message(&missing).contains("Illegal status change"),
        "case {unknown_case:?}: a missing job is not an illegal status change, got {missing:?}"
    );
}

/// RULE 4 over the whole status space it generalises over: the sentence renders EVERY
/// terminal `JobStatus` the refusal applies to, including the `FAILED` one this emulator's
/// own clock never produces, and whatever form the submitted id takes.
///
/// The constructor is driven directly because no wire call can put a job in `FAILED`; the
/// expected text is written out here as a literal, so the row cannot be satisfied by reading
/// the implementation's wording back.
#[test]
fn the_illegal_status_change_sentence_renders_every_status_it_refuses_from() {
    // The forms a submitted id takes: the UUID a start mints, an id that is no UUID, and a
    // single character.
    let ids = ["acef2041-5fb4-4bdd-a201-aee420aa09d0", "no-such-job", "j"];
    for from in REFUSED_FROM_STATUSES {
        for job_id in ids {
            let case = format!("{from} -> {CANCELED} for {job_id:?}");
            let refusal = policy_generation_illegal_status_change(job_id, from, CANCELED);
            let expected =
                format!("Invalid jobId: {job_id}. Illegal status change from {from} to {CANCELED}");
            assert_eq!(
                refusal.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                refusal.message
            );
            let named: Vec<&str> = refusal
                .field_list
                .iter()
                .flatten()
                .map(|field| field.name.as_str())
                .collect();
            assert_eq!(
                named,
                vec!["jobId"],
                "case {case:?}: expected the refusal to blame jobId, got {named:?}"
            );
        }
    }
}
