// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the already-exists refusal of
//! `CreateDBClusterEndpoint`.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a create whose `DBClusterEndpointIdentifier` is already taken is
//! refused the modelled `DBClusterEndpointAlreadyExistsFault`, whose message names BOTH the
//! identifier that collided and the endpoint already holding it —
//! `An endpoint with the identifier <identifier> already exists: <that endpoint's Endpoint>`
//! — so a client creating several endpoints in one flow can tell from the response WHICH
//! identifier was taken and WHAT answers at it. The identifier is the one the REQUEST sent,
//! in the spelling it sent it in, and the address is read out of this service's own state: it
//! is the very address `DescribeDBClusterEndpoints` reports for the holder, whichever form
//! that cluster's endpoints are published in.
//!
//! The table spans what the rule generalises over: an identifier of one character, a plain
//! lower-case name, a mixed-case spelling (which the store folds, so the refusal has to carry
//! the caller's own spelling while the address comes from the stored record), one carrying
//! digits and hyphens, one at the longest length this service admits, a holder of each
//! published `CustomEndpointType`, a second create that asks for another type than the holder
//! has, and both forms a cluster's endpoints are published in — the address of a listener
//! this emulator opened for the endpoint, and the DNS name a cluster it serves no wire for
//! mints.
//!
//! Nothing here waits out a span of wall-clock: the one wait is the shared poll loop
//! ([`crate::lifecycle_poll_loops::poll_until`]).

use service_com_amazonaws_rds_amazonrdsv19::state::MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The cluster whose endpoints are published at listeners this emulator opened: its engine
/// speaks a wire this emulator serves.
const SERVED_CLUSTER: &str = "emu-cecw-served";

/// A cluster this emulator serves no engine wire for, whose endpoints are published as the
/// DNS names it mints — the other published form.
const NAMED_CLUSTER: &str = "emu-cecw-named";

/// `CreateDBCluster` for one of the two clusters above.
///
/// An Aurora cluster is created with a master user, as a client that means to connect to it
/// does; a graph cluster takes neither member — Amazon Neptune assigns the master user
/// itself — so the create names only what that engine accepts.
async fn a_cluster(router: &MultiServiceRouter, identifier: &str, engine: &str) {
    let mut params: Vec<(&str, &str)> = vec![
        ("DBClusterIdentifier", identifier),
        ("Engine", engine),
    ];
    if engine.starts_with("aurora") {
        params.extend([
            ("MasterUsername", "emuadmin"),
            ("MasterUserPassword", "emu-Secret-99"),
        ]);
    }
    expect_ok(identifier, call(router, "CreateDBCluster", &params).await);
}

/// `CreateDBClusterEndpoint` with its three required members.
async fn create_endpoint(
    router: &MultiServiceRouter,
    identifier: &str,
    cluster: &str,
    endpoint_type: &str,
) -> (u16, String) {
    call(
        router,
        "CreateDBClusterEndpoint",
        &[
            ("DBClusterEndpointIdentifier", identifier),
            ("DBClusterIdentifier", cluster),
            ("EndpointType", endpoint_type),
        ],
    )
    .await
}

/// The address `DescribeDBClusterEndpoints` reports for the endpoint `identifier`, once the
/// transition the create owed it has settled — the value the refusal has to carry.
async fn published_address(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let params = [("DBClusterEndpointIdentifier", identifier.to_string())];
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        || call(router, "DescribeDBClusterEndpoints", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|endpoint| {
                        optional_text(endpoint, "Status").as_deref() == Some("available")
                    })
        },
    )
    .await;
    let endpoint = subtrees(&body, "DBClusterEndpointList")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry {identifier}"));
    let address = text(&endpoint, "Endpoint");
    assert!(
        !address.is_empty(),
        "case {case:?}: a custom endpoint publishes an address, got {endpoint}"
    );
    address
}

/// One row of the collision table.
struct Row<'a> {
    case: &'static str,
    /// The cluster both creates name.
    cluster: &'static str,
    /// The `DBClusterEndpointIdentifier` the FIRST create sends, which the store folds to
    /// lower case.
    held: &'a str,
    /// The `EndpointType` that first create asks for.
    held_type: &'static str,
    /// The spelling the SECOND create sends — the one the refusal has to name.
    named: &'a str,
    /// The type that second create asks for.
    named_type: &'static str,
}

/// The longest identifier this service admits, built from the published bound so the row
/// cannot drift from it.
fn longest() -> String {
    let mut identifier = String::from("emu-cecw-");
    while identifier.len() < MAX_DB_CLUSTER_ENDPOINT_IDENTIFIER_LEN {
        identifier.push('x');
    }
    identifier
}

#[tokio::test]
async fn the_identifier_a_delete_freed_is_one_the_identical_create_takes_again() {
    let router = emulator();
    a_cluster(&router, SERVED_CLUSTER, "aurora-postgresql").await;
    let identifier = "emu-cecw-freed";

    // The holder, and the refusal a second create of its identifier is answered with.
    expect_ok(
        "the first create",
        create_endpoint(&router, identifier, SERVED_CLUSTER, "READER").await,
    );
    let address = published_address(&router, "the holder", identifier).await;
    let refused = expect_error(
        "a second create",
        create_endpoint(&router, identifier, SERVED_CLUSTER, "ANY").await,
        400,
        "DBClusterEndpointAlreadyExistsFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        format!("An endpoint with the identifier {identifier} already exists: {address}"),
        "expected the sentence naming the identifier and the endpoint holding it, got {refused}"
    );

    // Once the holder is gone — the delete is accepted and the clock takes the record away —
    // the identical create is served again, and the endpoint it stores publishes an address of
    // its own.
    expect_ok(
        "the delete",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", identifier)],
        )
        .await,
    );
    let params = [("DBClusterEndpointIdentifier", identifier.to_string())];
    poll_until(
        "the delete",
        &format!("{identifier} leaving the store"),
        || call(&router, "DescribeDBClusterEndpoints", &params),
        |status, body| status == 200 && subtrees(body, "DBClusterEndpointList").is_empty(),
    )
    .await;
    expect_ok(
        "the create that takes the identifier again",
        create_endpoint(&router, identifier, SERVED_CLUSTER, "ANY").await,
    );
    let again = published_address(&router, "the endpoint that took it again", identifier).await;
    assert!(
        !again.is_empty(),
        "the endpoint created under the freed identifier publishes an address, got {again:?}"
    );
}

#[tokio::test]
async fn a_second_create_of_one_identifier_names_it_and_the_endpoint_already_holding_it() {
    let router = emulator();
    a_cluster(&router, SERVED_CLUSTER, "aurora-postgresql").await;
    a_cluster(&router, NAMED_CLUSTER, "neptune").await;

    let longest = longest();
    let table: Vec<Row<'_>> = vec![
        Row {
            case: "one character",
            cluster: SERVED_CLUSTER,
            held: "e",
            held_type: "ANY",
            named: "e",
            named_type: "ANY",
        },
        Row {
            case: "a plain lower-case name, taken by a reader endpoint",
            cluster: SERVED_CLUSTER,
            held: "emu-cecw-reader",
            held_type: "READER",
            named: "emu-cecw-reader",
            named_type: "WRITER",
        },
        Row {
            case: "a mixed-case spelling of an identifier the store folded",
            cluster: SERVED_CLUSTER,
            held: "emu-cecw-folded",
            held_type: "WRITER",
            named: "EMU-CECW-Folded",
            named_type: "READER",
        },
        Row {
            case: "digits and hyphens",
            cluster: SERVED_CLUSTER,
            held: "emu-cecw-9-7",
            held_type: "ANY",
            named: "emu-cecw-9-7",
            named_type: "READER",
        },
        Row {
            case: "the longest identifier this service admits",
            cluster: SERVED_CLUSTER,
            held: longest.as_str(),
            held_type: "READER",
            named: longest.as_str(),
            named_type: "ANY",
        },
        Row {
            case: "a cluster whose endpoints are published as minted DNS names",
            cluster: NAMED_CLUSTER,
            held: "emu-cecw-named-ep",
            held_type: "READER",
            named: "emu-cecw-named-ep",
            named_type: "ANY",
        },
    ];

    for row in &table {
        let case = row.case;
        expect_ok(
            case,
            create_endpoint(&router, row.held, row.cluster, row.held_type).await,
        );
        let address = published_address(&router, case, row.held).await;

        let refused = expect_error(
            case,
            create_endpoint(&router, row.named, row.cluster, row.named_type).await,
            400,
            "DBClusterEndpointAlreadyExistsFault",
        );
        assert_eq!(
            text(&refused, "Message"),
            format!(
                "An endpoint with the identifier {} already exists: {address}",
                row.named
            ),
            "case {case:?}: expected the sentence naming the identifier the request sent and the \
             address the endpoint holding it publishes, got {refused}"
        );

        // The refused create changed nothing: the holder keeps the type it was created with,
        // and the address the refusal named is still the one it publishes.
        let holder = published_address(&router, case, row.held).await;
        assert_eq!(
            holder, address,
            "case {case:?}: a refused create leaves the holder's address alone, got {holder:?}"
        );
    }
}
