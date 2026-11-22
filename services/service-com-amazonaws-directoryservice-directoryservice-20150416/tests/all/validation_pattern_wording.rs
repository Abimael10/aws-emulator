// SPDX-License-Identifier: CC0-1.0

//! How this service QUOTES a `@pattern` in the refusal of a value outside it, driven end to
//! end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router.
//!
//! The rule under test: a value a member's `@pattern` does not admit is refused HTTP 400
//! `ValidationException` with the boundary's envelope — the rejected value, the member as
//! the wire names it, and the requirement — and the requirement QUOTES the expression THIS
//! SERVICE PUBLISHES for the member's shape, which for every shape below is the model's own
//! text, including the `^`/`$` pair the model wraps a whole-value pattern in (the shapes
//! this service publishes with an anchor dropped are `tests/all/published_pattern_text.rs`'s
//! table). It is asserted over a TABLE of this service's `@pattern`-constrained members
//! rather than the one recorded call: ids of four different id spaces, a tag resource id and
//! a domain name, which between them span a short character class, a class with an escaped
//! metacharacter, a bracketed single letter, a counted hex run and a grouped repetition.
//!
//! The requirement belongs to the MEMBER's shape and not to the operation that carried it,
//! which the second table asserts: one shape (`DirectoryId`) through every operation class of
//! this service that carries it — reporting reads, paginated listings, a read whose member is
//! optional, a create and a toggle — crossed with each way a value can fall outside that
//! expression, including values an UNANCHORED reading of it would admit.
//!
//! The CHECK is the same expression: every rejected value below CONTAINS a substring the
//! expression matches (a run of admitted characters), so a value is refused exactly when the
//! model's anchored expression refuses it. The last case is the positive control: a value the
//! expression admits whole is not refused this way at all and reaches the handler.

use serde_json::json;

use crate::harness::{call, service};

/// A `DirectoryId` that satisfies the modelled `^d-[0-9a-f]{10}$` and is created in no case
/// here, so a request whose other members are well formed reaches the handler.
const ABSENT_DIRECTORY_ID: &str = "d-0123456789";

/// One row: an operation, a body with exactly ONE member outside its `@pattern`, the member
/// as the wire names it, the value that was rejected, and the expression this service is
/// expected to quote.
struct PatternCase {
    case: &'static str,
    operation: &'static str,
    member: &'static str,
    value: &'static str,
    /// The expression as the refusal quotes it: the expression this service publishes for
    /// the shape, which for every row of this table is the shape's `@pattern` itself, anchor
    /// pair included. Each row's modelled pattern is named in the comment beside it, from
    /// `smithy_query(query="shape",shape="<Shape>")`.
    stated: &'static str,
    body: fn(&str) -> serde_json::Value,
}

/// The `@pattern`-constrained members this rule is asserted over, one per expression form
/// this service declares.
fn pattern_rows() -> Vec<PatternCase> {
    vec![
        // `DirectoryId`: `^d-[0-9a-f]{10}$` — a literal prefix and a counted hex run.
        PatternCase {
            case: "a directory id of the wrong form",
            operation: "DescribeLDAPSSettings",
            member: "directoryId",
            value: "not-a-directory-id",
            stated: r"^d-[0-9a-f]{10}$",
            body: |value| json!({ "DirectoryId": value }),
        },
        // `SnapshotId`: `^s-[0-9a-f]{10}$` — another id space, so the clause is not one
        // member's.
        PatternCase {
            case: "a snapshot id of the wrong form",
            operation: "DeleteSnapshot",
            member: "snapshotId",
            value: "snap-0123456789",
            stated: r"^s-[0-9a-f]{10}$",
            body: |value| json!({ "SnapshotId": value }),
        },
        // `AssessmentId`: `^da-[0-9a-f]{18}$` — a wider run and a two-letter prefix.
        PatternCase {
            case: "an assessment id too short for its id space",
            operation: "DescribeADAssessment",
            member: "assessmentId",
            value: "da-0123456789",
            stated: r"^da-[0-9a-f]{18}$",
            body: |value| json!({ "AssessmentId": value }),
        },
        // `ResourceId`: `^[d]-[0-9a-f]{10}$` — a bracketed single letter, which the drop
        // must leave alone.
        PatternCase {
            case: "a tag resource id of the wrong form",
            operation: "AddTagsToResource",
            member: "resourceId",
            value: "r-0123456789",
            stated: r"^[d]-[0-9a-f]{10}$",
            body: |value| {
                json!({
                    "ResourceId": value,
                    "Tags": [{ "Key": "owner", "Value": "platform" }]
                })
            },
        },
        // `CertificateId`: `^c-[0-9a-f]{10}$`, refused beside a well-formed `DirectoryId`,
        // so the clause names the member that broke its pattern and not the first one.
        PatternCase {
            case: "a certificate id of the wrong form",
            operation: "DescribeCertificate",
            member: "certificateId",
            value: "cert-0123456789",
            stated: r"^c-[0-9a-f]{10}$",
            body: |value| {
                json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "CertificateId": value })
            },
        },
        // `TrustId`: `^t-[0-9a-f]{10}$`.
        PatternCase {
            case: "a trust id of the wrong form",
            operation: "DeleteTrust",
            member: "trustId",
            value: "trust-0123456789",
            stated: r"^t-[0-9a-f]{10}$",
            body: |value| json!({ "TrustId": value }),
        },
        // `RemoteDomainName`: `^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+[.]?$` — a grouped
        // repetition whose character class carries an escaped metacharacter, which the drop
        // must carry through untouched.
        PatternCase {
            case: "a remote domain name with a space in it",
            operation: "CreateConditionalForwarder",
            member: "remoteDomainName",
            value: "not a domain!",
            stated: r"^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+[.]?$",
            body: |value| {
                json!({
                    "DirectoryId": ABSENT_DIRECTORY_ID,
                    "RemoteDomainName": value,
                    "DnsIpAddrs": ["10.0.0.10"]
                })
            },
        },
    ]
}

/// The whole clause the boundary states for one rejected value.
fn pattern_clause(value: &str, member: &str, stated: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {stated}"
    )
}

#[tokio::test]
async fn a_pattern_refusal_quotes_the_expression_the_service_publishes_for_the_shape() {
    let router = service();

    for row in pattern_rows() {
        let (status, parsed) = call(&router, row.operation, (row.body)(row.value)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the value to be refused, got {status} with {parsed}",
            row.case
        );
        assert_eq!(
            parsed["__type"],
            json!("ValidationException"),
            "case {:?}: expected the boundary's validation envelope, got {parsed}",
            row.case
        );
        let stated = parsed["message"].as_str().unwrap_or_default();
        assert_eq!(
            stated,
            pattern_clause(row.value, row.member, row.stated),
            "case {:?}: expected the expression quoted as the model declares it, got \
             {stated:?}",
            row.case
        );
    }
}

/// The expression the model declares on the `DirectoryId` shape
/// (`smithy_query(query="shape",shape="DirectoryId")`), which is the text a clause about a
/// value outside it quotes: the `^` that opens it and the `$` that closes it are part of the
/// expression, so a clause that dropped either would publish a DIFFERENT, unanchored
/// expression from the one the model states.
const DIRECTORY_ID_PATTERN: &str = r"^d-[0-9a-f]{10}$";

/// A `LogGroupName` inside its own `@pattern` and length bounds
/// (`smithy_query(query="shape",shape="LogGroupName")`), so a `CreateLogSubscription` row
/// turns on its `DirectoryId` and on nothing else.
const GOOD_LOG_GROUP_NAME: &str = "/aws/directoryservice/sim2real";

/// The only wire value the `LDAPSType` enum declares
/// (`smithy_query(query="shape",shape="LDAPSType")`), so an LDAPS row states one clause.
const LDAPS_TYPE: &str = "Client";

/// One operation that carries a `DirectoryId`: its wire name, and a body built around the id
/// whose every OTHER member is well formed, so the refusal states exactly one clause.
struct DirectoryIdCase {
    operation: &'static str,
    body: fn(&str) -> serde_json::Value,
}

/// A row for the operation `operation`, whose body is `body`.
fn directory_id_case(
    operation: &'static str,
    body: fn(&str) -> serde_json::Value,
) -> DirectoryIdCase {
    DirectoryIdCase { operation, body }
}

/// A body carrying nothing but the directory id.
fn directory_id_only(id: &str) -> serde_json::Value {
    json!({ "DirectoryId": id })
}

/// The operations whose input carries a `DirectoryId` member.
///
/// The member travels with the SHAPE rather than with an entry point, so the rule is
/// asserted over a span of operation classes that carry it: reporting reads, paginated
/// listings, a read whose member is OPTIONAL (`ListADAssessments`), a create and a toggle
/// that mutates. Every signature is from
/// `smithy_query(query="operation",operation="<Operation>")`.
fn directory_id_operations() -> Vec<DirectoryIdCase> {
    vec![
        directory_id_case("DescribeCAEnrollmentPolicy", directory_id_only),
        directory_id_case("DescribeDirectoryDataAccess", directory_id_only),
        directory_id_case("DescribeSettings", directory_id_only),
        directory_id_case("DescribeLDAPSSettings", directory_id_only),
        directory_id_case("DescribeDomainControllers", directory_id_only),
        directory_id_case("GetSnapshotLimits", directory_id_only),
        directory_id_case("ListIpRoutes", directory_id_only),
        directory_id_case("ListCertificates", directory_id_only),
        directory_id_case("ListSchemaExtensions", directory_id_only),
        // `ListADAssessments$DirectoryId` is OPTIONAL, so a value is still checked against
        // its expression when the caller sends one.
        directory_id_case("ListADAssessments", directory_id_only),
        directory_id_case("CreateLogSubscription", |id| {
            json!({ "DirectoryId": id, "LogGroupName": GOOD_LOG_GROUP_NAME })
        }),
        directory_id_case("EnableLDAPS", |id| {
            json!({ "DirectoryId": id, "Type": LDAPS_TYPE })
        }),
    ]
}

/// The ways a value can fall outside `^d-[0-9a-f]{10}$`, one row per form rather than one
/// recorded request value.
///
/// The last three rows are the ones an UNANCHORED reading of that expression would admit:
/// each CONTAINS a well-formed id, so a check or a published expression that had dropped the
/// `^`/`$` pair would match them. They are refused here, which is what ties the expression a
/// clause quotes to the one the walk applies.
fn malformed_directory_ids() -> [(&'static str, &'static str); 9] {
    [
        ("no id space at all", "not-a-directory-id"),
        ("the id space prefix with a body that is not hex", "d-bad"),
        ("an upper-case prefix", "D-0123456789"),
        ("one hex digit short of the counted run", "d-012345678"),
        ("a character outside the hex class", "d-012345678g"),
        ("empty", ""),
        ("one hex digit past the counted run", "d-0123456789a"),
        ("a well-formed id with text before it", "directory-d-0123456789"),
        ("a well-formed id with a trailing space", "d-0123456789 "),
    ]
}

/// RULE: the clause a `@pattern` refusal states is the member's, not the operation's — the
/// same rejected value draws the same envelope, the same member path and the same quoted
/// expression (the model's own text, anchors included) from EVERY operation that carries the
/// member, whatever the operation does with it.
#[tokio::test]
async fn a_directory_id_clause_is_the_same_in_every_operation_that_carries_it() {
    let router = service();

    for row in directory_id_operations() {
        for (form, value) in malformed_directory_ids() {
            let case = format!("{}: {form}", row.operation);
            let (status, parsed) = call(&router, row.operation, (row.body)(value)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the value to be refused, got {status} with {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("ValidationException"),
                "case {case:?}: expected the boundary's validation envelope, got {parsed}"
            );
            let stated = parsed["message"].as_str().unwrap_or_default();
            assert_eq!(
                stated,
                pattern_clause(value, "directoryId", DIRECTORY_ID_PATTERN),
                "case {case:?}: expected the expression quoted as the model declares it, got \
                 {stated:?}"
            );
        }
    }
}

/// RULE (the positive control for the same rule): a `DirectoryId` the expression admits
/// WHOLE is not refused by it in any of those operations — the quoted text changed no check,
/// so each call gets past the constraint walk and is answered by its own handler.
#[tokio::test]
async fn a_well_formed_directory_id_is_not_refused_by_its_pattern() {
    let router = service();

    // Well-formed ids no directory is created under, varied along the counted hex run so no
    // row can pass by matching one value.
    for admitted in ["d-0123456789", "d-abcdef0123", "d-0000000000", "d-ffffffffff"] {
        for row in directory_id_operations() {
            let case = format!("{}: the id {admitted:?}", row.operation);
            let (status, parsed) = call(&router, row.operation, (row.body)(admitted)).await;
            let wire_type = parsed["__type"].as_str().unwrap_or_default();
            assert!(
                !wire_type.contains("ValidationException"),
                "case {case:?}: expected no constraint refusal, got {status} with {parsed}"
            );
            let stated = parsed["message"].as_str().unwrap_or_default();
            assert!(
                !stated.contains("regular expression pattern"),
                "case {case:?}: expected no pattern clause, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_value_its_pattern_admits_is_not_refused_by_it() {
    let router = service();

    // The positive control for the whole table: a topic name the expression admits WHOLE
    // clears the constraint walk and reaches the handler, which resolves the directory the
    // request named and refuses THAT instead — so the clause quotes the expression a value
    // had to satisfy and the walk admits exactly the values it matches.
    for topic_name in ["sim2real-behav-topic", "a", "A_1-b", "0123456789"] {
        let case = format!("topic name {topic_name:?}");
        let (status, parsed) = call(
            &router,
            "RegisterEventTopic",
            json!({ "DirectoryId": ABSENT_DIRECTORY_ID, "TopicName": topic_name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the absent directory to be refused, got {status} with \
             {parsed}"
        );
        let wire_type = parsed["__type"].as_str().unwrap_or_default();
        assert!(
            wire_type.contains("EntityDoesNotExistException"),
            "case {case:?}: expected the resolution failure rather than a constraint refusal, \
             got {parsed}"
        );
    }
}
