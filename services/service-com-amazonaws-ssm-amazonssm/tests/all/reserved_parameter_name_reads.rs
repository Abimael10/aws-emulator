// SPDX-License-Identifier: CC0-1.0

//! What a READ addressed to a name in the hierarchy AWS reserves for its own published
//! parameters is answered with, end to end over the front door.
//!
//! RULE under test: **a call addressed to a name in the reserved `/aws` hierarchy that this
//! endpoint serves no name under is refused as DENIED — HTTP 400 `AccessDeniedException`
//! carrying "No access to reserved parameter name: &lt;name without its leading slash&gt;." —
//! whether the call reads or writes, so a read of that namespace discloses exactly what a
//! write of it does.** The namespace is CLOSED, not empty: "absent" invites a client to
//! retry or to create under the same name, which can never succeed there, while "denied"
//! tells it to stop.
//!
//! The gate is on the names the hierarchy serves nothing under. Inside a namespace this
//! endpoint DOES serve names in — a hierarchy AWS publishes in, or the reserved hierarchy
//! through which this service serves another service's secrets — a read is answered as any
//! other: the published name resolves (`all/public_parameter_catalog.rs`) and a name nothing
//! is published under is the ordinary miss (`all/parameter_not_found_payload.rs`). A call that
//! would CHANGE anything in the reserved hierarchy is denied wherever in it the name lies,
//! which is `all/restricted_parameter_hierarchy.rs`.
//!
//! The table's rows differ along what the rule generalises over: the OPERATION addressed
//! (each read of this slice that takes a parameter's name, and the write beside them), the
//! NAME form inside the reserved hierarchy (its root, a publisher level the endpoint bears no
//! namespace of, a deep name under one, the same name written without its leading separator,
//! a name inside a served namespace that nothing publishes, a leaf AWS really publishes) and
//! its depth. Each pass also reads a parameter of the ACCOUNT's own, so no row passes against
//! a handler that denies everything.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message};
use crate::public_parameter_catalog::{emulator, AMAZON_LINUX_NAMESPACE};

/// The modelled class a denial is reported as (`smithy_query(query="shape",
/// shape="AccessDeniedException")`: "The requester doesn't have permissions to perform the
/// requested operation", httpError 400).
const DENIED: &str = "AccessDeniedException";

/// The modelled class a name nothing holds is reported as (`smithy_query(query="shape",
/// shape="ParameterNotFound")`, httpError 400).
const MISS: &str = "ParameterNotFound";

/// Every read of this slice that is addressed by a parameter's `Name`
/// (`smithy_query(query="operation", operation="GetParameter,GetParameterHistory")`, both of
/// which take `Name` and resolve one parameter by it).
const READS: &[&str] = &["GetParameter", "GetParameterHistory"];

/// What a call addressed to a reserved name this endpoint serves nothing under is refused
/// with: the fixed sentence, then the name as the parameter's own ARN spells it — without the
/// leading separator — and a period.
fn reserved_name_denial(name: &str) -> String {
    format!(
        "No access to reserved parameter name: {}.",
        name.trim_start_matches('/')
    )
}

/// What a row expects of the reads addressed to its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The reserved hierarchy serves nothing under the name, so every call addressed to it is
    /// denied.
    Denied,
    /// The name lies where this endpoint serves names and nothing is held under it, so a read
    /// of it is the modelled miss.
    Miss,
    /// AWS publishes the name, so a read of it is served.
    Served,
}

/// A read addressed to a reserved name the hierarchy serves nothing under is DENIED, with the
/// same diagnostic the write of that name carries, while a name the hierarchy does serve is
/// read as any other.
#[tokio::test]
async fn a_read_of_a_reserved_name_nothing_is_served_under_is_denied() {
    let router = emulator();
    let published = a_published_name(&router).await;

    // (case, the name addressed, what the reads of it answer)
    let mut rows: Vec<(String, String, Expect)> = vec![
        (
            "the reserved root itself".to_string(),
            "/aws".to_string(),
            Expect::Denied,
        ),
        (
            "a publisher level the endpoint bears no namespace of".to_string(),
            "/aws/no-such-publisher".to_string(),
            Expect::Denied,
        ),
        (
            "a deep name under such a publisher level".to_string(),
            "/aws/a/b/c/d/e/f".to_string(),
            Expect::Denied,
        ),
        (
            "the same name written without its leading separator".to_string(),
            "aws/no-such-publisher".to_string(),
            Expect::Denied,
        ),
        (
            "a namespace nothing publishes, under a publisher level that bears one".to_string(),
            "/aws/service/no-such-published-namespace/leaf".to_string(),
            Expect::Miss,
        ),
        (
            "a leaf of a served namespace nothing publishes".to_string(),
            format!("{AMAZON_LINUX_NAMESPACE}/no-such-published-alias"),
            Expect::Miss,
        ),
        (
            "a name of the account's own hierarchy nothing wrote".to_string(),
            "/mine/never-written".to_string(),
            Expect::Miss,
        ),
    ];
    // A leaf AWS really publishes, discovered through the catalog read itself rather than
    // written here, so the table covers a reserved name that DOES resolve for a reader.
    rows.push((
        "a leaf AWS really publishes".to_string(),
        published.clone(),
        Expect::Served,
    ));

    for (case, name, expect) in &rows {
        for operation in READS {
            let (status, body) = call(&router, operation, &json!({ "Name": name })).await;
            match expect {
                Expect::Denied => {
                    let expected = reserved_name_denial(name);
                    assert_eq!(
                        (status, error_code(&body), error_message(&body)),
                        (400, DENIED.to_string(), expected.clone()),
                        "case {case:?} ({name}): expected {operation} to be denied with \
                         {expected:?}, got {status} {body}"
                    );
                }
                Expect::Miss => assert_eq!(
                    (status, error_code(&body)),
                    (400, MISS.to_string()),
                    "case {case:?} ({name}): expected {operation} to report a miss rather than \
                     a denial, got {status} {body}"
                ),
                Expect::Served => assert_eq!(
                    status, 200,
                    "case {case:?} ({name}): expected {operation} to serve the published name, \
                     got {status} {body}"
                ),
            }
        }

        // The reserved gate is the WRITE's gate: for a name the hierarchy serves nothing
        // under, the read and the write say exactly the same thing, so the read discloses no
        // more than the write does.
        if *expect == Expect::Denied {
            let (status, written) = call(
                &router,
                "PutParameter",
                &json!({ "Name": name, "Type": "String", "Value": "never" }),
            )
            .await;
            let (read_status, read) = call(&router, "GetParameter", &json!({ "Name": name })).await;
            assert_eq!(
                (status, error_code(&written), error_message(&written)),
                (
                    read_status,
                    error_code(&read),
                    error_message(&read)
                ),
                "case {case:?} ({name}): the write and the read must answer identically, got \
                 {status} {written} and {read_status} {read}"
            );
        }
    }

    // The account's own parameters are read as they were: the denial is about the reserved
    // hierarchy and not about the reads.
    let own = "/mine/readable";
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": own, "Type": "String", "Value": "value" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected {own} to be written, got {status} {written}"
    );
    for operation in READS {
        let (status, body) = call(&router, operation, &json!({ "Name": own })).await;
        assert_eq!(
            status, 200,
            "expected {operation} of the account's own parameter to be served, got {status} \
             {body}"
        );
    }
}

/// The reserved hierarchy through which this service serves ANOTHER service's secrets is a
/// namespace this endpoint serves names in, so a read addressed there is resolved against that
/// service rather than denied as a reserved name.
#[tokio::test]
async fn a_read_of_the_reserved_reference_namespace_is_not_denied_as_a_reserved_name() {
    let router = emulator();
    // No Secrets Manager is mounted on this emulator, so what the resolution answers is
    // whatever that dependency reports; what it is NOT is the reserved-name denial, because
    // the namespace is one this endpoint serves names in.
    for name in [
        "/aws/reference/secretsmanager/probe-secret",
        "/aws/reference/secretsmanager/team/probe-secret",
    ] {
        let (status, body) = call(&router, "GetParameter", &json!({ "Name": name })).await;
        assert_ne!(
            (error_code(&body), error_message(&body)),
            (DENIED.to_string(), reserved_name_denial(name)),
            "case {name:?}: a read of the reference namespace is not a reserved-name denial, \
             got {status} {body}"
        );
    }
}

/// A name AWS publishes at this endpoint, discovered through the hierarchy read rather than
/// written here.
async fn a_published_name(router: &MultiServiceRouter) -> String {
    let (status, page) = call(
        router,
        "GetParametersByPath",
        &json!({ "Path": AMAZON_LINUX_NAMESPACE, "MaxResults": 1 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the published namespace to be readable, got {status} {page}"
    );
    page["Parameters"][0]["Name"]
        .as_str()
        .unwrap_or_else(|| panic!("the published namespace reports a parameter, got {page}"))
        .to_string()
}
