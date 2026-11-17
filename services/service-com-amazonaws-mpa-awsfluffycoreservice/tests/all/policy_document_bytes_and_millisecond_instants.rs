// SPDX-License-Identifier: CC0-1.0

//! Two rules of this service's READ PROJECTIONS, both of them the whole service's rather
//! than one operation's.
//!
//! 1. A policy document a reader reports is the text the catalog holds for that version,
//!    BYTE FOR BYTE: no reflow, no re-indentation, no added or dropped trailing newline.
//!    `PolicyVersion$Document` and `GetResourcePolicy$PolicyDocument` both target
//!    `PolicyDocument` (`length(max: 400000)`, `@sensitive`), a plain string the model
//!    gives no structure, and no operation of this model mints or edits one — so a reader
//!    hands the stored text on unchanged, whatever shape it is in. The table below spans
//!    the FORM of the document (compact one-line, pretty-printed with a trailing newline,
//!    tab-indented, carrying a `Principal` element and an IAM policy variable, and padded
//!    with blank lines), because that is what "byte for byte" generalises over.
//!
//! 2. A timestamp member whose model declares `@timestampFormat("date-time")` is
//!    serialized at MILLISECOND precision: `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`, a
//!    fractional-second field of exactly three digits, present whatever the instant is —
//!    a whole second renders `...T00:00:00.000Z`, never `...T00:00:00Z`. This model
//!    declares exactly one timestamp shape, `IsoTimestamp`, and it carries that trait, so
//!    the rule holds for every timestamp member of every shape; the table spans the
//!    instant's own sub-second value (none, a half second, values needing one and two
//!    zeros of padding, the largest millisecond, and one finer than a millisecond) and is
//!    checked on a stored catalog instant and on an instant the live clock minted.
//!
//!    Rule 2 is a WHOLE TEMPLATE, not just a precision: the rendering is the fixed text
//!    `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` — the `-`, `T`, `:` and `.` separators, the trailing
//!    `Z`, and the presence and width of every field — with only the digits coming from
//!    the instant. The two tables at the end of this file read that template strictly:
//!    one spans the instant's place in the CALENDAR as well as its sub-second value (the
//!    epoch, a leap day, the last second of a year, a far-future year) and checks the
//!    exact text each one is written as, and the other sweeps EVERY reader of this
//!    service, asserting the template on every `date-time` member it reports, however
//!    deeply nested — the width a client parsing with that strict pattern depends on
//!    cannot vary from one reader, member or instant to another.
//!
//! Every request goes over the wire the real client uses: a `restJson1` HTTP request
//! dispatched through the `MultiServiceRouter` the harness in `approval_team_lifecycle.rs`
//! mounts, so the route, the URI bindings, the JSON envelope and the HTTP status are all
//! exercised.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::Value;
use service_com_amazonaws_mpa_awsfluffycoreservice::state;

use crate::approval_team_lifecycle::{encode_label, expected_arn, text, Harness};
use crate::identity_source_deletion::create_source;
use crate::published_catalog_and_iso_timestamps::date_time_members;
use crate::resource_policies::{create_body, get_resource_policy};

/// A fixed instant the tables below date their recorded versions from, so no assertion
/// depends on when the test ran.
fn base_instant() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// `GET /policy-versions/{PolicyVersionArn}` — `GetPolicyVersion`.
async fn get_policy_version(harness: &Harness, version_arn: &str) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!("/policy-versions/{}", encode_label(version_arn)),
            None,
        )
        .await
}

/// `POST /policies/{PolicyArn}/?List` — `ListPolicyVersions`.
async fn list_policy_versions(harness: &Harness, policy_arn: &str) -> (u16, Value) {
    let uri = format!("/policies/{}/?List&MaxResults=20", encode_label(policy_arn));
    harness.call("POST", &uri, None).await
}

/// The catalog a FRESH store publishes, in ARN order.
fn published_catalog() -> Vec<state::CatalogPolicy> {
    state::ComAmazonawsMpaAwsfluffycoreserviceState::default()
        .policies
        .values()
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// RULE 1: a document is reported byte for byte.
// ---------------------------------------------------------------------------

/// One row of the document-form table: a version id and the exact text recorded for it.
struct DocumentCase {
    case: &'static str,
    version_id: i32,
    document: String,
}

/// The document-form table. Rows differ along the FORM of the stored text — the dimension
/// "byte for byte" generalises over — not along what the policy says.
fn document_cases() -> Vec<DocumentCase> {
    vec![
        DocumentCase {
            case: "compact, one line, no trailing newline",
            version_id: 1,
            document: concat!(
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
                r#""Action":["mpa:StartSession"],"Resource":"*"}]}"#,
            )
            .to_string(),
        },
        DocumentCase {
            case: "pretty-printed, two-space indentation, trailing newline",
            version_id: 2,
            document: r#"{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [ "mpa:StartSession", "mpa:CancelSession" ],
      "Resource": "*"
    }
  ]
}
"#
            .to_string(),
        },
        DocumentCase {
            case: "tab-indented, carrying a Principal element and a policy variable",
            version_id: 3,
            document: "{\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": [{\n\t\t\"Sid\": \
                       \"TabIndented\",\n\t\t\"Effect\": \"Allow\",\n\t\t\"Principal\": { \
                       \"AWS\": \"*\" },\n\t\t\"Action\": \"mpa:StartSession\",\n\t\t\
                       \"Resource\": \"*\",\n\t\t\"Condition\": { \"StringEquals\": { \
                       \"mpa:ProtectedResourceAccount\": \"${aws:PrincipalAccount}\" } }\n\t\
                       }]\n}\n"
                .to_string(),
        },
        DocumentCase {
            case: "padded with blank lines and trailing spaces",
            version_id: 4,
            document: "\n{  \n  \"Version\" : \"2012-10-17\" ,\n\n  \"Statement\" : [ { \
                       \"Effect\" : \"Allow\" , \"Action\" : \"mpa:CancelSession\" , \
                       \"Resource\" : \"*\" } ]\n\n}\n\n"
                .to_string(),
        },
    ]
}

/// Record one catalog policy holding every row of [`document_cases`] as its own version,
/// and answer its ARN. No modelled operation mints a policy — the catalog is AWS's — so a
/// table that needs one writes it directly, the way `policy_versions.rs` does.
fn record_document_cases(harness: &Harness) -> String {
    let mut policy = state::CatalogPolicy {
        arn: state::policy_arn("IdentityCenter", "DocumentFormPolicy"),
        name: "Document Form Policy".to_string(),
        policy_type: state::PolicyType::AwsManaged,
        default_version: 1,
        versions: BTreeMap::new(),
    };
    for case in document_cases() {
        let created =
            base_instant() + Duration::from_secs(u64::from(case.version_id.unsigned_abs()) * 60);
        policy.insert_version(state::CatalogPolicyVersion {
            version_id: case.version_id,
            status: state::PolicyStatus::Attachable,
            creation_time: created,
            last_updated_time: created,
            document: case.document,
        });
    }
    let arn = policy.arn.clone();
    let store = state::service_state(&*harness.engine).expect("the service store");
    store.lock().insert_policy(policy);
    arn
}

/// RULE 1: whatever form the stored document is in, the reader reports those exact bytes.
#[tokio::test]
async fn a_version_document_is_reported_byte_for_byte() {
    let harness = Harness::without_organization();
    let policy_arn = record_document_cases(&harness);

    for expected in document_cases() {
        let version_arn = state::policy_version_arn(&policy_arn, expected.version_id);
        let (status, detail) = get_policy_version(&harness, &version_arn).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetPolicyVersion returned {status}: {detail:?}",
            expected.case
        );
        let reported = text(&detail["PolicyVersion"], "Document");
        assert_eq!(
            reported, expected.document,
            "case {:?}: expected the stored document unchanged ({} bytes), got {} bytes: \
             {reported:?}",
            expected.case,
            expected.document.len(),
            reported.len()
        );
    }
}

/// RULE 1 for the catalog this service PUBLISHES: its documents are published text, so
/// every reader of them reports those bytes, and the text itself is a whole IAM policy
/// document — a `Principal` element and a named protected operation included.
#[tokio::test]
async fn the_published_document_is_published_text_and_reads_back_unchanged() {
    let harness = Harness::new();
    let published = published_catalog();
    assert!(
        !published.is_empty(),
        "a fresh store must seed the published catalog"
    );

    for (index, policy) in published.iter().enumerate() {
        for version in policy.versions.values() {
            let row = format!("{} (version {})", policy.arn, version.version_id);

            // The published text: a complete IAM policy document, laid out as the catalog
            // publishes it.
            let document: Value = serde_json::from_str(&version.document).unwrap_or_else(|error| {
                panic!("case {row:?}: the published document is not JSON: {error}")
            });
            assert!(
                version.document.contains('\n') && version.document.ends_with('\n'),
                "case {row:?}: the published document is pretty-printed text with a trailing \
                 newline, got {:?}",
                version.document
            );
            let statements = document["Statement"].as_array().cloned().unwrap_or_default();
            assert!(
                !statements.is_empty(),
                "case {row:?}: the published document carries no statement: {document:?}"
            );
            for statement in &statements {
                assert!(
                    statement["Principal"].is_object() || statement["Principal"].is_string(),
                    "case {row:?}: the published statement carries no Principal element: \
                     {statement:?}"
                );
                let requested_operation = statement["Condition"]["StringEquals"]
                    [&format!("{}:RequestedOperation", state::SERVICE_ACTION_PREFIX)]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    requested_operation.contains(':') && !requested_operation.contains('*'),
                    "case {row:?}: the published statement must gate one named operation, got \
                     {requested_operation:?} in {statement:?}"
                );
            }

            // And every reader reports those bytes.
            let version_arn = state::policy_version_arn(&policy.arn, version.version_id);
            let (status, detail) = get_policy_version(&harness, &version_arn).await;
            assert_eq!(
                status, 200,
                "case {row:?}: GetPolicyVersion returned {status}: {detail:?}"
            );
            assert_eq!(
                text(&detail["PolicyVersion"], "Document"),
                version.document,
                "case {row:?}: GetPolicyVersion must report the published text unchanged: \
                 {detail:?}"
            );
        }

        // The resource-policy read resolves the same catalog entry, through an attachment
        // of the policy's default version, and reports the same bytes.
        let default_version = policy
            .versions
            .get(&policy.default_version)
            .expect("the published default version");
        let team_name = format!("doc-bytes-{index}");
        let attachment = format!("{}/{}", policy.arn, state::DEFAULT_POLICY_VERSION);
        let (status, created) = harness
            .create(&create_body(&team_name, std::slice::from_ref(&attachment)))
            .await;
        assert_eq!(
            status, 200,
            "case {attachment:?}: CreateApprovalTeam returned {status}: {created:?}"
        );
        let team_arn = text(&created, "Arn");
        assert_eq!(
            team_arn,
            expected_arn(&team_name),
            "case {attachment:?}: CreateApprovalTeam minted {team_arn:?}"
        );
        let (status, attached) = get_resource_policy(
            &harness,
            &team_arn,
            &policy.name,
            policy.policy_type.as_wire(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {attachment:?}: GetResourcePolicy returned {status}: {attached:?}"
        );
        assert_eq!(
            text(&attached, "PolicyDocument"),
            default_version.document,
            "case {attachment:?}: GetResourcePolicy must report the published text unchanged: \
             {attached:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: a date-time is reported at millisecond precision.
// ---------------------------------------------------------------------------

/// The millisecond field of a `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` rendering, or `None` when
/// `rendered` is not in that form: the fractional-second field must be present, exactly
/// three digits wide, and what precedes it must still be an RFC 3339 UTC date-time.
fn millisecond_field(rendered: &str) -> Option<u32> {
    let instant = rendered.strip_suffix('Z')?;
    let (seconds, fraction) = instant.split_once('.')?;
    if fraction.len() != 3 || !fraction.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    DateTime::from_str(&format!("{seconds}Z"), Format::DateTime).ok()?;
    fraction.parse().ok()
}

/// RULE 2 over one member: it is a millisecond-precision date-time string, and its
/// millisecond field is the stored instant's.
fn assert_millisecond_instant(case: &str, body: &Value, member: &str, expected: SystemTime) {
    let rendered = body
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {member} is missing or not a string: {body:?}"))
        .to_string();
    let actual = millisecond_field(&rendered).unwrap_or_else(|| {
        panic!(
            "case {case:?}: {member} must be a date-time with a three-digit fractional-second \
             field, got {rendered:?}"
        )
    });
    let expected_millis = expected
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("an instant after the epoch")
        .subsec_millis();
    assert_eq!(
        actual, expected_millis,
        "case {case:?}: expected {member} to report {expected_millis} ms, got {rendered:?}"
    );
}

/// RULE 2, for a member whose instant the live clock minted: the SHAPE is still fixed,
/// whatever sub-second value the clock happened to hand out.
fn assert_millisecond_shape(case: &str, body: &Value, member: &str) {
    let rendered = body
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {member} is missing or not a string: {body:?}"))
        .to_string();
    assert!(
        millisecond_field(&rendered).is_some(),
        "case {case:?}: {member} must be a date-time with a three-digit fractional-second \
         field, got {rendered:?}"
    );
}

/// The sub-second offsets the instant table spans: none, a half second, a value needing
/// one zero of padding, one needing two, the largest millisecond, and a value finer than a
/// millisecond (reported at this service's precision).
const SUBSECOND_NANOS: &[(&str, u32)] = &[
    ("a whole second", 0),
    ("half a second", 500_000_000),
    ("tens of milliseconds", 50_000_000),
    ("a single millisecond", 7_000_000),
    ("the largest millisecond", 999_000_000),
    ("finer than a millisecond", 123_456_789),
];

/// Record one policy whose versions carry [`SUBSECOND_NANOS`] as their instants, and
/// answer `(policy arn, (version id, creation, last update) per row)`.
fn record_instant_cases(harness: &Harness) -> (String, Vec<(i32, SystemTime, SystemTime)>) {
    let mut policy = state::CatalogPolicy {
        arn: state::policy_arn("IdentityCenter", "InstantPrecisionPolicy"),
        name: "Instant Precision Policy".to_string(),
        policy_type: state::PolicyType::AwsRam,
        default_version: 1,
        versions: BTreeMap::new(),
    };
    let mut rows = Vec::new();
    for (index, (_, nanos)) in SUBSECOND_NANOS.iter().enumerate() {
        let version_id = i32::try_from(index + 1).expect("a small version id");
        let created = base_instant()
            + Duration::from_secs(u64::from(version_id.unsigned_abs()) * 60)
            + Duration::from_nanos(u64::from(*nanos));
        // The two members carry different sub-second values, so neither can pass by
        // reporting the other's.
        let updated = created + Duration::from_millis(1);
        policy.insert_version(state::CatalogPolicyVersion {
            version_id,
            status: state::PolicyStatus::Attachable,
            creation_time: created,
            last_updated_time: updated,
            document: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["mpa:StartSession"],"Resource":"*","Condition":{{"StringEquals":{{"mpa:RequestedOperation":"v{version_id}"}}}}}}]}}"#
            ),
        });
        rows.push((version_id, created, updated));
    }
    let arn = policy.arn.clone();
    let store = state::service_state(&*harness.engine).expect("the service store");
    store.lock().insert_policy(policy);
    (arn, rows)
}

/// RULE 2: every `date-time` member is reported with a three-digit fractional-second
/// field, through both the single-version reader and the paged one.
#[tokio::test]
async fn a_date_time_is_reported_at_millisecond_precision() {
    let harness = Harness::new();
    let (policy_arn, rows) = record_instant_cases(&harness);

    let (status, listed) = list_policy_versions(&harness, &policy_arn).await;
    assert_eq!(
        status, 200,
        "ListPolicyVersions returned {status}: {listed:?}"
    );
    let summaries = listed["PolicyVersions"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        summaries.len(),
        rows.len(),
        "ListPolicyVersions reports {} of {} versions: {listed:?}",
        summaries.len(),
        rows.len()
    );

    for ((label, _), (version_id, created, updated)) in SUBSECOND_NANOS.iter().zip(&rows) {
        let version_arn = state::policy_version_arn(&policy_arn, *version_id);
        let summary = summaries
            .iter()
            .find(|entry| text(entry, "Arn") == version_arn)
            .unwrap_or_else(|| panic!("case {label:?}: no summary for {version_arn}: {listed:?}"));
        let case = format!("ListPolicyVersions, {label}");
        assert_millisecond_instant(&case, summary, "CreationTime", *created);
        assert_millisecond_instant(&case, summary, "LastUpdatedTime", *updated);

        let (status, detail) = get_policy_version(&harness, &version_arn).await;
        assert_eq!(
            status, 200,
            "case {label:?}: GetPolicyVersion returned {status}: {detail:?}"
        );
        let reported = detail["PolicyVersion"].clone();
        let case = format!("GetPolicyVersion, {label}");
        assert_millisecond_instant(&case, &reported, "CreationTime", *created);
        assert_millisecond_instant(&case, &reported, "LastUpdatedTime", *updated);
    }

    // The seeded catalog's own instants, which the published entry records on a whole
    // second: they too carry the field.
    for policy in &published_catalog() {
        for version in policy.versions.values() {
            let version_arn = state::policy_version_arn(&policy.arn, version.version_id);
            let (status, detail) = get_policy_version(&harness, &version_arn).await;
            assert_eq!(
                status, 200,
                "case {version_arn:?}: GetPolicyVersion returned {status}: {detail:?}"
            );
            let case = format!("the published catalog, {version_arn}");
            assert_millisecond_instant(
                &case,
                &detail["PolicyVersion"],
                "CreationTime",
                version.creation_time,
            );
            assert_millisecond_instant(
                &case,
                &detail["PolicyVersion"],
                "LastUpdatedTime",
                version.last_updated_time,
            );
        }
    }

    // And an instant the live clock minted, read back through a different reader: the
    // shape is the same.
    let team_name = "millisecond-instants";
    let attachment = format!("{policy_arn}/{}", state::DEFAULT_POLICY_VERSION);
    let (status, created) = harness
        .create(&create_body(team_name, &[attachment]))
        .await;
    assert_eq!(
        status, 200,
        "CreateApprovalTeam returned {status}: {created:?}"
    );
    assert_millisecond_shape("CreateApprovalTeam", &created, "CreationTime");
    let (status, team) = harness.get(&text(&created, "Arn")).await;
    assert_eq!(status, 200, "GetApprovalTeam returned {status}: {team:?}");
    assert_millisecond_shape("GetApprovalTeam", &team, "CreationTime");
}

// ---------------------------------------------------------------------------
// RULE 2 as a TEMPLATE: the fixed text and the field widths themselves.
// ---------------------------------------------------------------------------

/// The template a `date-time` of this service is written in, with `0` standing for an
/// ASCII digit and every other character standing for itself — the fixed text of
/// `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'`, separators, field widths and trailing `Z` included.
const MILLISECOND_TEMPLATE: &str = "0000-00-00T00:00:00.000Z";

/// `rendered` read STRICTLY as [`MILLISECOND_TEMPLATE`]: the instant it names, in
/// milliseconds since the epoch, or `None` when a single character of the template is
/// missing, widened or narrowed. This is the `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` parser a
/// fixed-millisecond producer invites, so a rendering one field short fails it outright.
fn template_millis(rendered: &str) -> Option<i64> {
    if rendered.len() != MILLISECOND_TEMPLATE.len() {
        return None;
    }
    for (shape, actual) in MILLISECOND_TEMPLATE.bytes().zip(rendered.bytes()) {
        let matches = if shape == b'0' {
            actual.is_ascii_digit()
        } else {
            shape == actual
        };
        if !matches {
            return None;
        }
    }
    let instant = DateTime::from_str(rendered, Format::DateTime).ok()?;
    Some(instant.secs() * 1_000 + i64::from(instant.subsec_nanos() / 1_000_000))
}

/// A stored instant in milliseconds since the epoch, truncated to this service's
/// precision the way a rendering of it is.
fn stored_millis(instant: SystemTime) -> i64 {
    i64::try_from(
        instant
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("an instant at or after the epoch")
            .as_millis(),
    )
    .expect("an instant inside the range a date-time expresses")
}

/// One member whose stored instant is known: it is written in [`MILLISECOND_TEMPLATE`],
/// as exactly `expected_text`, and the instant it names is the stored one.
fn assert_template_text(
    case: &str,
    body: &Value,
    member: &str,
    expected_text: &str,
    expected: SystemTime,
) {
    let rendered = body
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: {member} is missing or not a string: {body:?}"))
        .to_string();
    let millis = template_millis(&rendered).unwrap_or_else(|| {
        panic!(
            "case {case:?}: {member} must be written as yyyy-MM-ddTHH:mm:ss.SSSZ, got {rendered:?}"
        )
    });
    assert_eq!(
        rendered, expected_text,
        "case {case:?}: expected {member} {expected_text:?}, got {rendered:?}"
    );
    assert_eq!(
        millis,
        stored_millis(expected),
        "case {case:?}: expected {member} to name the stored instant ({} ms), got {rendered:?}",
        stored_millis(expected)
    );
}

/// Every `date-time` member one response carries, however deeply nested, written in
/// [`MILLISECOND_TEMPLATE`] — and the members `expected` names present, so a reader that
/// reported no instant at all cannot pass vacuously. Answers how many were checked.
fn assert_template_everywhere(case: &str, body: &Value, expected: &[&str]) -> usize {
    let mut found: Vec<(String, Value)> = Vec::new();
    date_time_members("$", body, &mut found);
    for (path, value) in &found {
        let rendered = value
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: {path} must be a date-time string, got {value:?}")
            })
            .to_string();
        assert!(
            template_millis(&rendered).is_some(),
            "case {case:?}: {path} must be written as yyyy-MM-ddTHH:mm:ss.SSSZ, got {rendered:?}"
        );
    }
    for member in expected {
        assert!(
            found
                .iter()
                .any(|(path, _)| path.ends_with(&format!(".{member}"))),
            "case {case:?}: no {member} in {body:?}"
        );
    }
    found.len()
}

/// One row of the template table: a version's two instants, each with the exact text the
/// service writes it as.
struct TemplateCase {
    case: &'static str,
    creation_secs: i64,
    creation_nanos: u32,
    creation_text: &'static str,
    update_secs: i64,
    update_nanos: u32,
    update_text: &'static str,
}

impl TemplateCase {
    fn creation(&self) -> SystemTime {
        instant_of(self.creation_secs, self.creation_nanos)
    }

    fn last_update(&self) -> SystemTime {
        instant_of(self.update_secs, self.update_nanos)
    }
}

/// The instant `secs` seconds and `nanos` nanoseconds after the epoch.
fn instant_of(secs: i64, nanos: u32) -> SystemTime {
    SystemTime::UNIX_EPOCH
        + Duration::new(
            u64::try_from(secs).expect("an instant at or after the epoch"),
            nanos,
        )
}

/// The template table. Rows span BOTH dimensions the rendering generalises over: the
/// instant's sub-second value (none, one, two and three significant digits, the largest
/// millisecond, and values finer than a millisecond, which are reported at this service's
/// precision rather than rounded up into the next second) and the instant's place in the
/// CALENDAR (the epoch itself, an ordinary instant, a leap day, the last second of a year
/// and the first of the next, a far-future year) — every field of the template is
/// exercised, and the two members of a row never share an instant, so neither can pass by
/// reporting the other's.
const TEMPLATE_CASES: &[TemplateCase] = &[
    TemplateCase {
        case: "the epoch itself, and half a second past it",
        creation_secs: 0,
        creation_nanos: 0,
        creation_text: "1970-01-01T00:00:00.000Z",
        update_secs: 0,
        update_nanos: 500_000_000,
        update_text: "1970-01-01T00:00:00.500Z",
    },
    TemplateCase {
        case: "a whole second, and one millisecond past it",
        creation_secs: 1_700_000_000,
        creation_nanos: 0,
        creation_text: "2023-11-14T22:13:20.000Z",
        update_secs: 1_700_000_000,
        update_nanos: 1_000_000,
        update_text: "2023-11-14T22:13:20.001Z",
    },
    TemplateCase {
        case: "one and two significant digits of milliseconds",
        creation_secs: 1_700_000_000,
        creation_nanos: 7_000_000,
        creation_text: "2023-11-14T22:13:20.007Z",
        update_secs: 1_700_000_000,
        update_nanos: 50_000_000,
        update_text: "2023-11-14T22:13:20.050Z",
    },
    TemplateCase {
        case: "three significant digits, and the largest millisecond",
        creation_secs: 1_700_000_000,
        creation_nanos: 123_000_000,
        creation_text: "2023-11-14T22:13:20.123Z",
        update_secs: 1_700_000_000,
        update_nanos: 999_000_000,
        update_text: "2023-11-14T22:13:20.999Z",
    },
    TemplateCase {
        case: "finer than a millisecond, reported at this service's precision",
        creation_secs: 1_700_000_000,
        creation_nanos: 123_456_789,
        creation_text: "2023-11-14T22:13:20.123Z",
        update_secs: 1_700_000_000,
        update_nanos: 999_999_999,
        update_text: "2023-11-14T22:13:20.999Z",
    },
    TemplateCase {
        case: "a leap day",
        creation_secs: 1_709_208_000,
        creation_nanos: 250_000_000,
        creation_text: "2024-02-29T12:00:00.250Z",
        update_secs: 1_709_208_001,
        update_nanos: 0,
        update_text: "2024-02-29T12:00:01.000Z",
    },
    TemplateCase {
        case: "the last second of a year, and the first of the next",
        creation_secs: 1_735_689_599,
        creation_nanos: 999_000_000,
        creation_text: "2024-12-31T23:59:59.999Z",
        update_secs: 1_735_689_600,
        update_nanos: 0,
        update_text: "2025-01-01T00:00:00.000Z",
    },
    TemplateCase {
        case: "a far-future instant",
        creation_secs: 4_102_444_800,
        creation_nanos: 0,
        creation_text: "2100-01-01T00:00:00.000Z",
        update_secs: 4_102_444_800,
        update_nanos: 60_000_000,
        update_text: "2100-01-01T00:00:00.060Z",
    },
];

/// Record one policy holding every row of [`TEMPLATE_CASES`] as a version of its own, and
/// answer its ARN. No modelled operation mints a policy — the catalog is AWS's — so a
/// table that needs one writes it directly, the way `policy_versions.rs` does.
fn record_template_cases(harness: &Harness) -> String {
    let mut policy = state::CatalogPolicy {
        arn: state::policy_arn("IdentityCenter", "DateTimeTemplatePolicy"),
        name: "Date-time Template Policy".to_string(),
        policy_type: state::PolicyType::AwsManaged,
        default_version: 1,
        versions: BTreeMap::new(),
    };
    for (index, case) in TEMPLATE_CASES.iter().enumerate() {
        let version_id = i32::try_from(index + 1).expect("a small version id");
        policy.insert_version(state::CatalogPolicyVersion {
            version_id,
            status: state::PolicyStatus::Attachable,
            creation_time: case.creation(),
            last_updated_time: case.last_update(),
            document: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["mpa:StartSession"],"Resource":"*","Condition":{{"StringEquals":{{"mpa:RequestedOperation":"template-v{version_id}"}}}}}}]}}"#
            ),
        });
    }
    let arn = policy.arn.clone();
    let store = state::service_state(&*harness.engine).expect("the service store");
    store.lock().insert_policy(policy);
    arn
}

/// RULE 2 as a template: whatever the instant is, the two readers of a policy version
/// write it as `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` — that exact text, and nothing of the
/// template elided.
#[tokio::test]
async fn a_date_time_is_written_as_the_fixed_millisecond_template() {
    let harness = Harness::new();
    let policy_arn = record_template_cases(&harness);

    let (status, listed) = list_policy_versions(&harness, &policy_arn).await;
    assert_eq!(status, 200, "ListPolicyVersions returned {status}: {listed:?}");
    let summaries = listed["PolicyVersions"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        summaries.len(),
        TEMPLATE_CASES.len(),
        "ListPolicyVersions reports {} of {} versions: {listed:?}",
        summaries.len(),
        TEMPLATE_CASES.len()
    );

    for (index, case) in TEMPLATE_CASES.iter().enumerate() {
        let version_id = i32::try_from(index + 1).expect("a small version id");
        let version_arn = state::policy_version_arn(&policy_arn, version_id);
        let summary = summaries
            .iter()
            .find(|entry| text(entry, "Arn") == version_arn)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: no summary for {version_arn}: {listed:?}",
                    case.case
                )
            });
        let row = format!("ListPolicyVersions, {}", case.case);
        assert_template_text(
            &row,
            summary,
            "CreationTime",
            case.creation_text,
            case.creation(),
        );
        assert_template_text(
            &row,
            summary,
            "LastUpdatedTime",
            case.update_text,
            case.last_update(),
        );

        let (status, detail) = get_policy_version(&harness, &version_arn).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetPolicyVersion returned {status}: {detail:?}",
            case.case
        );
        let reported = detail["PolicyVersion"].clone();
        let row = format!("GetPolicyVersion, {}", case.case);
        assert_template_text(
            &row,
            &reported,
            "CreationTime",
            case.creation_text,
            case.creation(),
        );
        assert_template_text(
            &row,
            &reported,
            "LastUpdatedTime",
            case.update_text,
            case.last_update(),
        );
    }
}

/// `POST /policies/?List` — `ListPolicies`.
async fn list_policies(harness: &Harness) -> (u16, Value) {
    harness.call("POST", "/policies/?List", None).await
}

/// `GET /identity-sources/{IdentitySourceArn}` — `GetIdentitySource`.
async fn get_identity_source(harness: &Harness, source_arn: &str) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!("/identity-sources/{}", encode_label(source_arn)),
            None,
        )
        .await
}

/// `POST /identity-sources/?List` — `ListIdentitySources`.
async fn list_identity_sources(harness: &Harness) -> (u16, Value) {
    harness.call("POST", "/identity-sources/?List", None).await
}

/// RULE 2 is the SERVICE'S, not one projection's: every reader that reports an instant
/// writes every `date-time` member it carries in the template, so a client's strict
/// parser works the same against each of them. The rows are the readers themselves, each
/// driven end to end over the wire.
#[tokio::test]
async fn every_reader_writes_its_date_time_members_in_the_template() {
    let harness = Harness::new();
    let mut checked = 0usize;

    // The catalog readers, over every entry a fresh store publishes: the paged reader and
    // the single-version one. These are the instants the published catalog records on a
    // whole second, which is where an elided fractional-second field shows.
    let published = published_catalog();
    assert!(
        !published.is_empty(),
        "a fresh store must seed the published catalog"
    );
    let (status, policies) = list_policies(&harness).await;
    assert_eq!(status, 200, "ListPolicies returned {status}: {policies:?}");
    checked += assert_template_everywhere("ListPolicies", &policies, &[]);
    for policy in &published {
        let (status, versions) = list_policy_versions(&harness, &policy.arn).await;
        assert_eq!(
            status, 200,
            "case {:?}: ListPolicyVersions returned {status}: {versions:?}",
            policy.arn
        );
        checked += assert_template_everywhere(
            &format!("ListPolicyVersions, {}", policy.arn),
            &versions,
            &["CreationTime", "LastUpdatedTime"],
        );
        for version in policy.versions.values() {
            let version_arn = state::policy_version_arn(&policy.arn, version.version_id);
            let (status, detail) = get_policy_version(&harness, &version_arn).await;
            assert_eq!(
                status, 200,
                "case {version_arn:?}: GetPolicyVersion returned {status}: {detail:?}"
            );
            checked += assert_template_everywhere(
                &format!("GetPolicyVersion, {version_arn}"),
                &detail,
                &["CreationTime", "LastUpdatedTime"],
            );
        }
    }

    // A team: the write's own projection, and both readers of it.
    let policy = published.first().expect("a published policy");
    let attachment = format!("{}/{}", policy.arn, state::DEFAULT_POLICY_VERSION);
    let team_name = "date-time-template";
    let (status, created) = harness.create(&create_body(team_name, &[attachment])).await;
    assert_eq!(
        status, 200,
        "CreateApprovalTeam returned {status}: {created:?}"
    );
    checked += assert_template_everywhere("CreateApprovalTeam", &created, &["CreationTime"]);
    let team_arn = text(&created, "Arn");
    assert_eq!(
        team_arn,
        expected_arn(team_name),
        "CreateApprovalTeam minted {team_arn:?}"
    );
    let (status, team) = harness.get(&team_arn).await;
    assert_eq!(status, 200, "GetApprovalTeam returned {status}: {team:?}");
    checked += assert_template_everywhere("GetApprovalTeam", &team, &["CreationTime"]);
    let (status, teams) = harness.list("MaxResults=20").await;
    assert_eq!(status, 200, "ListApprovalTeams returned {status}: {teams:?}");
    checked += assert_template_everywhere("ListApprovalTeams", &teams, &["CreationTime"]);

    // An identity source, read singly and in a page.
    let source_arn = create_source(&harness, 1, 0).await;
    let (status, source) = get_identity_source(&harness, &source_arn).await;
    assert_eq!(status, 200, "GetIdentitySource returned {status}: {source:?}");
    checked += assert_template_everywhere("GetIdentitySource", &source, &["CreationTime"]);
    let (status, sources) = list_identity_sources(&harness).await;
    assert_eq!(
        status, 200,
        "ListIdentitySources returned {status}: {sources:?}"
    );
    checked += assert_template_everywhere("ListIdentitySources", &sources, &["CreationTime"]);

    assert!(
        checked >= 9,
        "the readers above must between them report at least 9 date-time members, got {checked}"
    );
}
