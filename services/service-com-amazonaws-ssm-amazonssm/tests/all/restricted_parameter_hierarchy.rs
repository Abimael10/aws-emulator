// SPDX-License-Identifier: CC0-1.0

//! What a call that would CHANGE a parameter in the hierarchy AWS reserves for its own
//! published parameters is answered with, end to end over the front door.
//!
//! RULE under test: **the `/aws` hierarchy holds AWS's own published parameters and no
//! parameter of this account, so a call that would change a name in it is refused as
//! DENIED — HTTP 400 `AccessDeniedException` — and never as the name merely being
//! absent.** The two refusals say different things to a client: "absent" invites a retry
//! or a recreate under the same name, which can never succeed here, while "denied" tells
//! the caller to stop. WHICH sentence the denial carries is decided by where in that
//! hierarchy the name lies, not by which call asked: a name in a hierarchy AWS publishes in
//! addresses AWS's own published data and carries the fixed sentence "Cannot access/update
//! data for the restricted parameter.", while a name under a publisher level AWS publishes
//! nothing under addresses no data at all and is refused as the reserved NAME it is, which
//! the sentence quotes.
//!
//! A READ of that hierarchy is not a change, so it is not denied for being one: a name AWS
//! publishes resolves against the published catalog (`all/public_parameter_catalog.rs`) and a
//! name it publishes nothing under INSIDE a namespace this endpoint serves is still the
//! modelled miss (`all/parameter_not_found_payload.rs`). What a read IS denied is a name the
//! reserved hierarchy serves nothing under at all — the namespace is closed rather than empty
//! (`all/reserved_parameter_name_reads.rs`). A HIERARCHY read of it does carry its own
//! restriction, checked here too: the reserved hierarchy is the namespaces AWS publishes in
//! (plus the reserved reference namespace this service serves another service's secrets
//! through), and a read reaches a namespace from either side — the levels above it, which
//! hold it, and the levels inside it, which it holds and may hold nothing at — so a path in
//! NO namespace of it is refused rather than answered with an empty page a caller would read
//! as a namespace that exists and holds nothing: denied ("No access to \"/aws/\" namespace:
//! &lt;path&gt; is not a valid namespace") when it leads toward a namespace, and by the name
//! grammar when its publisher level holds no namespace at all.
//!
//! The table's rows differ along what the rule generalises over: the OPERATION that would
//! make the change (the write, the delete, and both label calls), the NAME form inside the
//! reserved hierarchy (its root, a publisher level AWS publishes nothing under, a namespace
//! level, a leaf AWS really publishes, a leaf AWS publishes nothing under, the reserved
//! reference namespace of another service, a name addressed without its leading separator)
//! and its depth. Each operation also makes the same change to a parameter of the ACCOUNT's
//! own in the same pass, so no row passes against a handler that denies everything.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message};
use crate::public_parameter_catalog::{emulator, AMAZON_LINUX_NAMESPACE};

/// The sentence the denial of a call addressed to a name in a hierarchy AWS PUBLISHES in
/// carries. Wholly fixed: it names neither the parameter nor the operation, because what it
/// says is that AWS's published data is not the account's to change.
const RESTRICTED: &str = "Cannot access/update data for the restricted parameter.";

/// The sentence the denial of a call addressed to a reserved name AWS publishes nothing
/// under carries: there is no published data at that name, so the refusal is about the NAME,
/// which it quotes canonically.
fn reserved_name_denial(name: &str) -> String {
    format!(
        "No access to reserved parameter name: {}.",
        name.trim_start_matches('/')
    )
}

/// The modelled class the denial is reported as
/// (`smithy_query(query="shape", shape="AccessDeniedException")`: "The requester doesn't
/// have permissions to perform the requested operation", httpError 400).
const DENIED: &str = "AccessDeniedException";

/// One change a caller can ask for, as the request body that asks for it.
fn change(operation: &str, name: &str) -> Value {
    match operation {
        "PutParameter" => json!({ "Name": name, "Type": "String", "Value": "written" }),
        "LabelParameterVersion" => json!({ "Name": name, "Labels": ["release"] }),
        "UnlabelParameterVersion" => {
            json!({ "Name": name, "ParameterVersion": 1, "Labels": ["release"] })
        }
        _ => json!({ "Name": name }),
    }
}

/// Every call that would change a parameter addressed by name and that this slice serves.
/// Source: each operation's modelled input (`smithy_query(query="operation",
/// operation="PutParameter,DeleteParameter,LabelParameterVersion,UnlabelParameterVersion")`),
/// all four of which take the parameter's `Name`.
const CHANGES: &[&str] = &[
    "PutParameter",
    "DeleteParameter",
    "LabelParameterVersion",
    "UnlabelParameterVersion",
];

/// A name in the reserved hierarchy, whatever its form, is denied to every call that would
/// change it — with the sentence its PLACE in that hierarchy decides, the same for every one
/// of those calls — and the same call changes the account's own parameter.
#[tokio::test]
async fn a_change_to_the_reserved_hierarchy_is_denied() {
    let router = emulator();
    let published = a_published_name(&router).await;

    // (case, the name the change addresses, whether the name lies in a hierarchy AWS
    // publishes in — which is what decides the sentence)
    let mut rows: Vec<(String, String, bool)> = vec![
        (
            "the reserved root itself".to_string(),
            "/aws".to_string(),
            false,
        ),
        (
            "a publisher level of the reserved root AWS publishes nothing under".to_string(),
            "/aws/no-such-publisher".to_string(),
            false,
        ),
        (
            "a deep name under such a publisher level".to_string(),
            "/aws/a/b/c/d/e/f".to_string(),
            false,
        ),
        (
            "a namespace level of the reserved hierarchy".to_string(),
            AMAZON_LINUX_NAMESPACE.to_string(),
            true,
        ),
        (
            "a leaf AWS publishes nothing under".to_string(),
            format!("{AMAZON_LINUX_NAMESPACE}/no-such-published-alias"),
            true,
        ),
        (
            "the reserved reference namespace of another service".to_string(),
            "/aws/reference/secretsmanager/probe-secret".to_string(),
            true,
        ),
        (
            "a reserved name addressed without its leading separator".to_string(),
            "aws/service/pcs/sample".to_string(),
            true,
        ),
    ];
    // A leaf AWS really publishes, discovered through the catalog read itself rather than
    // written here, so the table covers a reserved name that DOES resolve for a reader.
    rows.push((
        "a leaf AWS really publishes".to_string(),
        published.clone(),
        true,
    ));

    for (case, name, published_data) in &rows {
        for operation in CHANGES {
            let (status, body) = call(&router, operation, &change(operation, name)).await;
            // A name in a hierarchy AWS publishes in addresses AWS's own published data,
            // which carries the fixed sentence naming no parameter; a name under a publisher
            // level AWS publishes nothing under addresses no data at all, and is refused as
            // the reserved name it is.
            let expected = if *published_data {
                RESTRICTED.to_string()
            } else {
                reserved_name_denial(name)
            };
            assert_eq!(
                (status, error_code(&body), error_message(&body)),
                (400, DENIED.to_string(), expected.clone()),
                "case {case:?} ({name}): expected {operation} to be denied with {expected:?}, \
                 got {status} {body}"
            );
            if *published_data {
                assert!(
                    !error_message(&body).contains(name.trim_start_matches('/')),
                    "case {case:?}: the denial names no parameter, got {}",
                    error_message(&body)
                );
            }
        }
    }

    // The account's own parameter takes every one of those changes, so the denial is about
    // the hierarchy and not about the operations.
    let own = "/own/changeable";
    put(&router, own, "value").await;
    let (status, labelled) = call(
        &router,
        "LabelParameterVersion",
        &change("LabelParameterVersion", own),
    )
    .await;
    assert_eq!(
        (status, labelled["ParameterVersion"].as_i64()),
        (200, Some(1)),
        "expected the account's own parameter to be labelled, got {status} {labelled}"
    );
    let (status, unlabelled) = call(
        &router,
        "UnlabelParameterVersion",
        &change("UnlabelParameterVersion", own),
    )
    .await;
    assert_eq!(
        (
            status,
            unlabelled["RemovedLabels"].as_array().map(Vec::len)
        ),
        (200, Some(1)),
        "expected the label to be removed from the account's own parameter, got {status} \
         {unlabelled}"
    );
    let (status, deleted) = call(&router, "DeleteParameter", &change("DeleteParameter", own)).await;
    assert_eq!(
        status, 200,
        "expected the account's own parameter to be deleted, got {status} {deleted}"
    );
}

/// The denial is about CHANGING the hierarchy, so a read inside a namespace this endpoint
/// SERVES is unchanged: a name AWS publishes still resolves, and a name it publishes nothing
/// under there is still the miss. (A read addressed to a name the reserved hierarchy serves
/// nothing under at all is denied — `all/reserved_parameter_name_reads.rs`.)
#[tokio::test]
async fn a_read_of_a_served_reserved_namespace_is_not_denied() {
    let router = emulator();
    let published = a_published_name(&router).await;

    let (status, read) = call(&router, "GetParameter", &json!({ "Name": published })).await;
    assert_eq!(
        (status, read.pointer("/Parameter/Name").and_then(Value::as_str)),
        (200, Some(published.as_str())),
        "expected the published parameter to read back, got {status} {read}"
    );

    // (case, the Name read) — each inside a namespace this endpoint serves names in, where a
    // name nothing is published under is absent rather than closed.
    let misses = [
        (
            "a leaf AWS publishes nothing under",
            format!("{AMAZON_LINUX_NAMESPACE}/no-such-published-alias"),
        ),
        (
            "a namespace AWS publishes nothing in, under a publisher level that bears one",
            "/aws/service/no/such/space".to_string(),
        ),
    ];
    for (case, name) in misses {
        for operation in ["GetParameter", "GetParameterHistory"] {
            let (status, body) = call(&router, operation, &json!({ "Name": name })).await;
            assert_eq!(
                (status, error_code(&body)),
                (400, "ParameterNotFound".to_string()),
                "case {case:?}: expected {operation} to report a miss rather than a denial, \
                 got {status} {body}"
            );
        }
    }
}

/// A hierarchy read inside the reserved hierarchy reaches the NAMESPACES of it this
/// endpoint serves names in, from either side: a level above a namespace holds it, and a
/// level inside one is held by it, so both are read. A path in NO namespace of the reserved
/// hierarchy is denied, naming the path and the hierarchy, rather than answered with an
/// empty page a caller would read as "this namespace exists and is empty".
#[tokio::test]
async fn a_hierarchy_read_outside_the_reserved_namespaces_is_denied() {
    let router = emulator();
    let published = a_published_name(&router).await;

    // (case, the path read)
    let denied: Vec<(&str, String)> = vec![
        (
            "a service namespace AWS publishes nothing in",
            "/aws/service/no-such-published-namespace".to_string(),
        ),
        (
            "a reference namespace of no service",
            "/aws/reference/no-such-source".to_string(),
        ),
    ];
    for (case, path) in &denied {
        let (status, refused) = call(
            &router,
            "GetParametersByPath",
            &json!({ "Path": path, "Recursive": true }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused), error_message(&refused)),
            (
                400,
                DENIED.to_string(),
                format!(
                    "No access to \"/aws/\" namespace: {} is not a valid namespace",
                    path.trim_start_matches('/')
                )
            ),
            "case {case:?} ({path}): expected the namespace refusal, got {status} {refused}"
        );
    }

    // A publisher level of the reserved root that holds no namespace: nothing of the path
    // names a hierarchy of the reserved tree, so what is left is a name carrying the
    // reserved root, which the name grammar refuses — the paragraph any other path that
    // names no hierarchy is refused with (`all/parameter_path_requirements.rs`).
    for (case, path) in [
        (
            "a level of the reserved hierarchy AWS has none",
            "/aws/no-such-level",
        ),
        (
            "another such level, read below itself",
            "/aws/no-such-level/deeper",
        ),
    ] {
        let (status, refused) = call(
            &router,
            "GetParametersByPath",
            &json!({ "Path": path, "Recursive": true }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationException".to_string()),
            "case {case:?} ({path}): expected the name-grammar refusal, got {status} {refused}"
        );
        assert!(
            !error_message(&refused).contains(path),
            "case {case:?} ({path}): the paragraph quotes no path back, got {}",
            error_message(&refused)
        );
    }

    // Every namespace this endpoint DOES serve names at or under is read, so the refusals
    // above are about the paths and not about the hierarchy. A level INSIDE a namespace is
    // read too, and reports the nothing that namespace holds at it.
    for (case, path, holds_names) in [
        ("the reserved root", "/aws".to_string(), true),
        ("the service level", "/aws/service".to_string(), true),
        (
            "a namespace AWS publishes in",
            AMAZON_LINUX_NAMESPACE.to_string(),
            true,
        ),
        // The reserved hierarchy through which this service serves another's secrets: a
        // namespace it serves names in, though it publishes no parameter there.
        (
            "the reserved reference namespace",
            "/aws/reference/secretsmanager".to_string(),
            false,
        ),
        ("the level above it", "/aws/reference".to_string(), false),
        ("a published name itself", published.clone(), false),
        // A level inside a namespace that exists: the namespace holds nothing there, which
        // is an empty page and not a denial.
        (
            "a leaf of a namespace AWS publishes nothing under",
            format!("{AMAZON_LINUX_NAMESPACE}/no-such-published-alias"),
            false,
        ),
        (
            "a published name read as if it were a hierarchy below itself",
            format!("{published}/deeper"),
            false,
        ),
    ] {
        let (status, served) = call(
            &router,
            "GetParametersByPath",
            &json!({ "Path": path, "Recursive": true, "MaxResults": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} ({path}): a namespace of the reserved hierarchy is read, got \
             {status} {served}"
        );
        let reported = served["Parameters"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            reported > 0,
            holds_names,
            "case {case:?} ({path}): expected the read to report {}, got {served}",
            if holds_names {
                "the names published under it"
            } else {
                "nothing"
            }
        );
    }
}

/// One parameter written through the front door.
async fn put(router: &MultiServiceRouter, name: &str, value: &str) {
    let (status, written) = call(
        router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": value }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected {name} to be written, got {status} {written}"
    );
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
