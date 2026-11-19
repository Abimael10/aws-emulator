// SPDX-License-Identifier: CC0-1.0

//! What `ListPermissions` reports for a principal that holds permissions from MORE THAN ONE
//! record — through the assembled router, where Lake Formation reads the Data Catalog out of
//! AWS Glue's own store.
//!
//! The rule under test: **one record's permissions are reported in the order that record was
//! granted them in, and an entry that reports SEVERAL records carries their UNION in the
//! order the `Permission` enum declares its values** — a principal holding `DESCRIBE` in its
//! own name and `ALL` through the IAM-allowed group is reported holding `ALL` then `DESCRIBE`,
//! whichever record the service read first. There is no grant order across records: they were
//! granted independently of each other, so the set they together hold is reported as a set.
//!
//! Where the merge happens is the listing that names a principal AND a resource: "if both
//! Principal and Resource parameters are provided, the response returns effective permissions
//! rather than the explicitly granted permissions" (`ListPermissions`), and what an IAM role
//! effectively holds is its own grant together with the group's.
//!
//! The rows differ along what the rule generalises over: which permissions the principal was
//! granted in its own name, how many, in which ORDER they were granted, whether the set
//! overlaps the group's own, and how many separate grant calls wrote them — with the
//! unfiltered listing of the same records asserted beside each row, which is where one
//! record's own order is observable.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries, which is also the Data Catalog id a
/// catalog call defaults to.
const ACCOUNT: &str = "000000000000";

/// The group a database is created holding `ALL` for, which is the other record every row's
/// principal holds through.
const IAM_ALLOWED_PRINCIPALS: &str = "IAM_ALLOWED_PRINCIPALS";

/// The emulator with the two services this suite exercises, on ONE engine: Lake Formation
/// resolves the database a grant names out of AWS Glue's own store.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lakeformation_awslakeformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one AWS Glue operation over its `awsJson1_1` wire.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Call one Lake Formation operation over its `restJson1` wire.
async fn lakeformation(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("lakeformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lakeformation"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The permissions one listing reports, keyed by the principal they are reported under, in the
/// order the entry carries them.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    filter: &Value,
) -> Vec<(String, Vec<String>)> {
    let (status, body) = lakeformation(router, "ListPermissions", filter).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListPermissions to answer 200, got {status} {body}"
    );
    body.get("PrincipalResourcePermissions")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a PrincipalResourcePermissions list, got {body}")
        })
        .iter()
        .map(|entry| {
            (
                entry
                    .get("Principal")
                    .and_then(|principal| principal.get("DataLakePrincipalIdentifier"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("Permissions")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .map(|value| value.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// One row: what the principal is granted in its own name, and what it is then reported
/// holding.
struct Row {
    case: &'static str,
    /// The database the row's grants are made on, its own so the rows share nothing.
    database: &'static str,
    /// The grant calls made, in order, each naming the permissions of one call.
    granted: &'static [&'static [&'static str]],
    /// What the principal's own record then holds, in the order the listing of the RESOURCE
    /// reports it: the order the record was granted in.
    held: &'static [&'static str],
    /// What the EFFECTIVE listing reports it holding: the union with the group's `ALL`, in the
    /// order the `Permission` enum declares its values.
    effective: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "one permission the group's own does not carry",
        database: "order_one",
        granted: &[&["DESCRIBE"]],
        held: &["DESCRIBE"],
        effective: &["ALL", "DESCRIBE"],
    },
    Row {
        // `CREATE_TABLE` is declared AFTER `DESCRIBE` by the enum and sorts BEFORE it, so
        // this row tells the enum's order from an alphabetical one.
        case: "two permissions granted in the enum's own order",
        database: "order_two",
        granted: &[&["DESCRIBE", "CREATE_TABLE"]],
        held: &["DESCRIBE", "CREATE_TABLE"],
        effective: &["ALL", "DESCRIBE", "CREATE_TABLE"],
    },
    Row {
        case: "the same two granted the other way round",
        database: "order_two_reversed",
        granted: &[&["CREATE_TABLE", "DESCRIBE"]],
        held: &["CREATE_TABLE", "DESCRIBE"],
        effective: &["ALL", "DESCRIBE", "CREATE_TABLE"],
    },
    Row {
        case: "three permissions written by three calls",
        database: "order_three_calls",
        granted: &[&["DROP"], &["SELECT"], &["ALTER"]],
        held: &["DROP", "SELECT", "ALTER"],
        effective: &["ALL", "SELECT", "ALTER", "DROP"],
    },
    Row {
        case: "the permission the group itself holds",
        database: "order_overlapping",
        granted: &[&["ALL"]],
        held: &["ALL"],
        effective: &["ALL"],
    },
    Row {
        case: "a permission the group holds beside one it does not",
        database: "order_overlapping_two",
        granted: &[&["DESCRIBE", "ALL"]],
        held: &["DESCRIBE", "ALL"],
        effective: &["ALL", "DESCRIBE"],
    },
];

/// The order the reporting rule reads the enum in MUST name exactly the values the model
/// declares: the order is hand-written in the service's own state (the generated
/// `PERMISSION_VALUES` carries the same values SORTED, which is a different order), so
/// nothing but a comparison against the generated list keeps the two from drifting as the
/// model gains a value.
#[test]
fn the_reported_order_names_exactly_the_modelled_permission_values() {
    let declared: std::collections::BTreeSet<&str> =
        service_com_amazonaws_lakeformation_awslakeformation::state::PERMISSION_DECLARATION_ORDER
            .iter()
            .copied()
            .collect();
    let modelled: std::collections::BTreeSet<&str> =
        service_com_amazonaws_lakeformation_awslakeformation::ops::types::PERMISSION_VALUES
            .iter()
            .copied()
            .collect();
    assert_eq!(
        declared, modelled,
        "case \"the order against the model\": expected the reported order to name exactly the \
         modelled values, got {declared:?} against {modelled:?}"
    );
    assert_eq!(
        service_com_amazonaws_lakeformation_awslakeformation::state::PERMISSION_DECLARATION_ORDER
            .len(),
        declared.len(),
        "case \"the order against itself\": expected no value named twice, got {:?}",
        service_com_amazonaws_lakeformation_awslakeformation::state::PERMISSION_DECLARATION_ORDER
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listing_reports_a_merged_entry_in_the_enums_own_order() {
    let router = build_router();
    let principal = format!("arn:aws:iam::{ACCOUNT}:role/analyst");

    for row in ROWS {
        let case = row.case;
        let (status, body) = glue(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": row.database } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup — CreateDatabase answered {status} {body}"
        );

        let resource = json!({ "Database": { "Name": row.database } });
        for permissions in row.granted {
            let (status, body) = lakeformation(
                &router,
                "GrantPermissions",
                &json!({
                    "Principal": { "DataLakePrincipalIdentifier": principal },
                    "Resource": resource,
                    "Permissions": permissions,
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the grant of {permissions:?} to be accepted, got \
                 {status} {body}"
            );
        }

        // The listing of the RESOURCE reports each record on its own: the principal's own
        // record carries what it was granted, in the order it was granted it, and the group's
        // record carries the database's default beside it.
        let reported_records = reported(&router, case, &json!({ "Resource": resource })).await;
        let mut expected: Vec<(String, Vec<String>)> = vec![
            (
                principal.clone(),
                row.held.iter().map(|value| value.to_string()).collect(),
            ),
            (
                IAM_ALLOWED_PRINCIPALS.to_string(),
                vec!["ALL".to_string()],
            ),
        ];
        let mut records = reported_records.clone();
        records.sort();
        expected.sort();
        assert_eq!(
            records, expected,
            "case {case:?}: expected each record reported on its own as {expected:?}, got \
             {reported_records:?}"
        );

        // The EFFECTIVE listing reports them as ONE entry under the principal it asked about,
        // carrying their union in the order the enum declares its values.
        let effective = reported(
            &router,
            case,
            &json!({
                "Resource": resource,
                "Principal": { "DataLakePrincipalIdentifier": principal },
            }),
        )
        .await;
        let expected: Vec<(String, Vec<String>)> = vec![(
            principal.clone(),
            row.effective
                .iter()
                .map(|value| value.to_string())
                .collect(),
        )];
        assert_eq!(
            effective, expected,
            "case {case:?}: expected the union {expected:?}, got {effective:?}"
        );
    }
}
