// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SOURCE INSTANCE
//! `StopDBInstanceAutomatedBackupsReplication` resolves before it looks for anything else.
//!
//! The rule under test: an operation whose request names its source DB instance by an ARN
//! resolves that instance in the Region the ARN itself carries, so a `SourceDBInstanceArn`
//! naming an instance that Region does not hold addresses a resource that does not exist —
//! the modelled `DBInstanceNotFoundFault`, awsQuery code `DBInstanceNotFound`, HTTP 404,
//! worded `Requested DB Instance <identifier> not found in source region <region>`, which is
//! the one sentence this service words every cross-Region source it cannot find with. Only a
//! source that IS resolved reaches the question this operation then asks — is that instance
//! replicating its automated backups into the Region of the call? — whose one refusal is the
//! modelled `InvalidDBInstanceStateFault`, HTTP 400.
//!
//! The rows span what the rule generalises over: the Region the ARN points at against the
//! Region of the call, an identifier that exists in ANOTHER Region than the ARN names (so
//! existence is per-Region and not global), an ARN with no Region segment at all, the casing
//! of both halves of what the ARN names, and the identifier's own shape. The two controls on
//! the other side of the rule keep it honest: a source that exists and replicates nothing here
//! is refused the state fault instead, and one that is replicating here is SERVED.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, and the assertions read the
//! XML the emulator answers with.
//!
//! Nothing here waits out a span: the one state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_automated_backup_replication::{
    available_instance, describe_backups_of, start_replication, stop_replication, the_backup,
};
use crate::db_instances::{emulator, expect_error, expect_ok, optional_text, text};
use crate::lifecycle_poll_loops::poll_until;

/// The account id every generated emulator presents, which the ARN forms below are built with.
const ACCOUNT: &str = "000000000000";

/// The `awsQueryError` code of `DBInstanceNotFoundFault`.
const NOT_FOUND: &str = "DBInstanceNotFound";

/// The `awsQueryError` code of `InvalidDBInstanceStateFault`.
const INVALID_STATE: &str = "InvalidDBInstanceState";

/// The status a replicated automated backup reports once it has its first snapshot.
const ACTIVE: &str = "active";

/// The status it reports "after backup replication is stopped".
const RETAINED: &str = "retained";

/// A DB instance ARN of this service's own form, as `SourceDBInstanceArn` documents one.
fn db_instance_arn(region: &str, identifier: &str) -> String {
    format!("arn:aws:rds:{region}:{ACCOUNT}:db:{identifier}")
}

/// The sentence a source instance that cannot be resolved is refused with: fixed text around
/// the identifier asked for and the Region it was looked for in.
fn not_found_in(identifier: &str, source_region: &str) -> String {
    format!("Requested DB Instance {identifier} not found in source region {source_region}")
}

/// One row: the `SourceDBInstanceArn` a stop carries, and the identifier and Region its
/// refusal must name.
struct Unresolvable {
    case: &'static str,
    /// The Region the stop is made to — the destination of the replication.
    request_region: &'static str,
    source_arn: String,
    identifier: &'static str,
    source_region: &'static str,
}

/// RULE: a stop whose `SourceDBInstanceArn` names an instance the ARN's own Region does not
/// hold is refused HTTP 404 `DBInstanceNotFound`, naming that identifier and that Region.
#[tokio::test]
async fn a_stop_whose_source_instance_cannot_be_resolved_is_refused_not_found() {
    let router = emulator();
    // One instance that DOES exist, in us-west-2, so a row can address an identifier that
    // exists by an ARN naming another Region.
    let case = "the instance that exists";
    let (_, exists_west) =
        available_instance(&router, case, "us-west-2", "emu-stop-src-exists", "postgres", 1).await;
    assert_eq!(
        exists_west,
        db_instance_arn("us-west-2", "emu-stop-src-exists"),
        "case {case:?}: the instance must report the ARN form the rows are built with, got \
         {exists_west}"
    );

    let table: &[Unresolvable] = &[
        Unresolvable {
            case: "a source west of the call that was never created",
            request_region: "us-east-1",
            source_arn: db_instance_arn("us-west-2", "emu-stop-src-ghost"),
            identifier: "emu-stop-src-ghost",
            source_region: "us-west-2",
        },
        Unresolvable {
            case: "the same identifier, in another source Region",
            request_region: "us-east-1",
            source_arn: db_instance_arn("eu-central-1", "emu-stop-src-ghost"),
            identifier: "emu-stop-src-ghost",
            source_region: "eu-central-1",
        },
        Unresolvable {
            // The identifier DOES exist — in us-west-2, created above — so an instance is
            // resolved in the Region its ARN names and nowhere else.
            case: "an existing identifier addressed by an ARN naming another Region",
            request_region: "us-east-1",
            source_arn: db_instance_arn("ap-southeast-2", "emu-stop-src-exists"),
            identifier: "emu-stop-src-exists",
            source_region: "ap-southeast-2",
        },
        Unresolvable {
            // An ARN with an EMPTY Region segment names no Region other than the one the
            // request was signed for, so that is where its source is looked for.
            case: "a source ARN carrying no Region segment",
            request_region: "ca-central-1",
            source_arn: format!("arn:aws:rds::{ACCOUNT}:db:emu-stop-src-region-less"),
            identifier: "emu-stop-src-region-less",
            source_region: "ca-central-1",
        },
        Unresolvable {
            // Both halves of what the ARN names are folded the way this store keys them, so
            // the sentence names the Region in the published form however the ARN spelled it.
            case: "a source ARN whose Region segment is upper-cased",
            request_region: "us-east-1",
            source_arn: db_instance_arn("EU-WEST-1", "emu-stop-src-cased"),
            identifier: "emu-stop-src-cased",
            source_region: "eu-west-1",
        },
        Unresolvable {
            case: "a one-label identifier",
            request_region: "sa-east-1",
            source_arn: db_instance_arn("us-east-2", "db"),
            identifier: "db",
            source_region: "us-east-2",
        },
    ];

    for row in table {
        let expected = not_found_in(row.identifier, row.source_region);
        let refused = expect_error(
            row.case,
            stop_replication(&router, row.request_region, &row.source_arn).await,
            404,
            NOT_FOUND,
        );
        assert_eq!(
            text(&refused, "Message"),
            expected,
            "case {:?}: expected {expected:?}, got {refused}",
            row.case
        );
    }
}

/// RULE (the other side of it): a stop whose source instance IS resolved is answered for the
/// REPLICATION — the state fault where the Region of the call holds none, and a 200 where it
/// holds one — so the 404 above is about resolving the source and nothing else.
#[tokio::test]
async fn a_stop_whose_source_resolves_is_answered_for_the_replication() {
    let router = emulator();
    let destination = "us-east-1";

    // A source that exists and replicates nothing anywhere: the question the stop asks has an
    // answer, and it is "nothing is replicating here".
    let case = "a source that exists and replicates nothing";
    let (_, idle) =
        available_instance(&router, case, "us-west-2", "emu-stop-src-idle", "mysql", 2).await;
    let refused = expect_error(
        case,
        stop_replication(&router, destination, &idle).await,
        400,
        INVALID_STATE,
    );
    assert_eq!(
        text(&refused, "Message"),
        format!("DB Instance {idle} is not replicating to the current region."),
        "case {case:?}: the state refusal names the ARN the request carried, got {refused}"
    );

    // And a source that IS replicating into the Region of the call is served: the backups it
    // had taken are retained from then on.
    let case = "a source replicating into the Region of the call";
    let (resource_id, replicating) = available_instance(
        &router,
        case,
        "eu-west-1",
        "emu-stop-src-replicating",
        "postgres",
        3,
    )
    .await;
    expect_ok(
        case,
        start_replication(&router, destination, &replicating, &[]).await,
    );
    reach_active(&router, case, destination, &resource_id).await;
    let stopped = expect_ok(
        case,
        stop_replication(&router, destination, &replicating).await,
    );
    assert_eq!(
        text(&the_backup(case, &stopped), "Status"),
        RETAINED,
        "case {case:?}: the stop retains the backups it had taken, got {stopped}"
    );
}

/// Poll the replicated backups of `resource_id` in `destination` until they report `active`,
/// so the stop under test acts on a replication fully in force.
async fn reach_active(
    router: &MultiServiceRouter,
    case: &str,
    destination: &str,
    resource_id: &str,
) {
    poll_until(
        case,
        "the replicated backups reaching active",
        || describe_backups_of(router, destination, resource_id),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some(ACTIVE),
    )
    .await;
}
