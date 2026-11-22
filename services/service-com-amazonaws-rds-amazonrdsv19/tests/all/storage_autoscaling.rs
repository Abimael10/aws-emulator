// SPDX-License-Identifier: CC0-1.0

//! STORAGE AUTOSCALING of a DB instance: what makes Amazon RDS raise `AllocatedStorage` by
//! itself.
//!
//! THE RULE: `MaxAllocatedStorage` is "the upper limit in gibibytes (GiB) to which Amazon RDS can
//! automatically scale the storage of the DB instance", and setting it is the whole of what a
//! client does to ask for autoscaling: RDS raises the storage itself, with no
//! `ModifyDBInstance(AllocatedStorage=…)` call, once the instance's free space has been under a
//! tenth of its allocated storage for the published window — and the raise is bounded by the
//! member. What decides it is the storage the instance's ENGINE is using, which is why this
//! suite drives the condition through the engine's own endpoint rather than through any API
//! member, and why an instance with room to spare is never raised however much is read or polled.
//!
//! The three parts of the rule are pinned in two places, each where it can be observed:
//!
//! * the CONDITION and the SIZE of the raise are the state module's own table-driven tests
//!   (`state::storage_autoscaling`), over the classes they generalise over — how full the storage
//!   is, how big the instance is, and how much room `MaxAllocatedStorage` leaves;
//! * what a CLIENT sees is here: the member is persisted and reported, the storage the engine is
//!   using is the instance's own, and a reporting call never moves `AllocatedStorage` for an
//!   instance that has room — a read that raised storage would be a read that changed what a
//!   later caller sees.

use crate::db_instance_engine_endpoint::{wire_login, Wire, DATABASE, MASTER_PASSWORD, MASTER_USER};
use crate::db_instances::{call, emulator_with_engine, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The storage the instance of this suite is created with, in GiB.
const ALLOCATED_STORAGE: &str = "20";

/// The upper limit it may be scaled to, in GiB.
const MAX_ALLOCATED_STORAGE: &str = "100";

/// Create the instance and wait until it reports `available`, answering its endpoint and port.
async fn available_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> (String, u16) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.t4g.micro"),
                ("AllocatedStorage", ALLOCATED_STORAGE),
                ("MaxAllocatedStorage", MAX_ALLOCATED_STORAGE),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DBName", DATABASE),
            ],
        )
        .await,
    );
    let read = [("DBInstanceIdentifier", identifier)];
    let (body, _polls) = poll_until(
        case,
        "the instance",
        || call(router, "DescribeDBInstances", &read),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
    let port: u16 = text(&body, "Port")
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected the port the engine listens on"));
    (text(&body, "Address"), port)
}

/// The `AllocatedStorage` and `MaxAllocatedStorage` the instance reports now.
async fn reported_storage(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> (String, String) {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await,
    );
    (
        text(&body, "AllocatedStorage"),
        text(&body, "MaxAllocatedStorage"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_with_room_keeps_the_storage_it_was_created_with() {
    let (router, engine) = emulator_with_engine();
    let case = "an instance with autoscaling on and room to spare";
    let identifier = "emu-autoscale-room";
    let (address, port) = available_instance(&router, case, identifier).await;

    // Rows the client really wrote, so the engine behind the endpoint is holding data: the
    // storage this suite's rule is read off is that engine's own.
    let mut session = wire_login(case, Wire::Postgres, &address, port, DATABASE).await;
    session
        .run(case, "CREATE TABLE ledger (id integer PRIMARY KEY, note varchar(64))")
        .await;
    for row in 1..=16 {
        session
            .run(
                case,
                &format!("INSERT INTO ledger (id, note) VALUES ({row}, 'a row of the ledger')"),
            )
            .await;
    }
    let rows = session.value(case, "SELECT count(*) FROM ledger").await;
    assert_eq!(
        rows, "16",
        "case {case:?}: the engine must be holding the rows the session wrote, got {rows}"
    );

    // The storage that data takes, as the engine itself accounts for it — which is what the
    // autoscaling condition is read from, and is far inside the tenth of the instance's
    // storage that must be free.
    let used = {
        let state = st::service_state(&*engine).expect("this service's store");
        let store = state.lock();
        let endpoint = store
            .engine_endpoint(&(REGION.to_string(), identifier.to_string()))
            .unwrap_or_else(|| panic!("case {case:?}: the instance must have an engine endpoint"));
        endpoint.database().stored_bytes()
    };
    assert!(
        used > 0,
        "case {case:?}: the engine must account for the storage its databases take, got {used} \
         bytes"
    );
    let allocated: i32 = ALLOCATED_STORAGE.parse().expect("a number of GiB");
    assert!(
        !st::storage_autoscaling::is_low_on_storage(allocated, used),
        "case {case:?}: an instance using {used} bytes of {ALLOCATED_STORAGE} GiB is not low \
         on storage"
    );

    // So no reporting call raises its storage, however many of them are made: the member is
    // reported as it was set, and the storage stays what the create asked for.
    for poll in 0..3 {
        let (allocated, max) = reported_storage(&router, case, identifier).await;
        assert_eq!(
            (allocated.as_str(), max.as_str()),
            (ALLOCATED_STORAGE, MAX_ALLOCATED_STORAGE),
            "case {case:?}: read {poll} must report the storage the create asked for and the \
             limit it may grow to"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_that_stays_low_on_storage_has_it_raised_without_any_modify_call() {
    let (router, engine) = emulator_with_engine();
    let case = "an instance low on storage for the whole window";
    let identifier = "emu-autoscale-raised";
    available_instance(&router, case, identifier).await;
    let allocated: i32 = ALLOCATED_STORAGE.parse().expect("a number of GiB");
    let max: i32 = MAX_ALLOCATED_STORAGE.parse().expect("a number of GiB");

    // The condition, as the service's own pass observes it: the instance is using all but a
    // twentieth of its storage. The observation is handed in at an instant of this test's
    // choosing, because the low-storage condition has to LAST the published five-minute window
    // before a raise is owed — which is a span no test may wait out.
    let observed = vec![st::storage_autoscaling::ObservedStorage {
        key: (REGION.to_string(), identifier.to_string()),
        resource_id: {
            let state = st::service_state(&*engine).expect("this service's store");
            let store = state.lock();
            store
                .db_instances
                .get(&(REGION.to_string(), identifier.to_string()))
                .map(|instance| instance.dbi_resource_id.clone())
                .unwrap_or_else(|| panic!("case {case:?}: the instance must be stored"))
        },
        allocated,
        max,
        used_bytes: i64::from(allocated) * 1024 * 1024 * 1024 * 19 / 20,
    }];
    let started = ::std::time::Instant::now();
    // The two passes run back to back, with no call in between: a reporting call of its own
    // would observe the instance's REAL storage — far inside the threshold — and end the
    // condition, which is exactly what the suite's first test pins.
    let (before, after) = {
        let state = st::service_state(&*engine).expect("this service's store");
        let mut store = state.lock();
        let stored = |store: &st::ComAmazonawsRdsAmazonrdsv19State| {
            store
                .db_instances
                .get(&(REGION.to_string(), identifier.to_string()))
                .and_then(|instance| instance.allocated_storage)
                .unwrap_or_default()
        };
        // The first pass only starts the window: nothing is owed yet.
        store.apply_storage_autoscaling(REGION, &observed, started);
        let before = stored(&store);
        // Once the condition has LASTED the window, the raise is owed.
        store.apply_storage_autoscaling(
            REGION,
            &observed,
            started + st::storage_autoscaling::LOW_STORAGE_WINDOW,
        );
        (before, stored(&store))
    };
    assert_eq!(
        before, allocated,
        "case {case:?}: the window has not passed, so the storage is still what the create \
         asked for"
    );
    assert!(
        after > allocated && after <= max,
        "case {case:?}: expected the storage raised past {allocated} GiB and inside the \
         {max} GiB limit, got {after}"
    );

    // And a client that made no ModifyDBInstance call at all reads the new storage back.
    let (raised, limit) = reported_storage(&router, case, identifier).await;
    assert_eq!(
        raised,
        after.to_string(),
        "case {case:?}: expected the raised storage to be reported, got {raised}"
    );
    assert_eq!(
        limit, MAX_ALLOCATED_STORAGE,
        "case {case:?}: the limit itself is unchanged by a raise"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_limit_an_instance_may_grow_to_is_persisted_and_can_be_changed() {
    let (router, _engine) = emulator_with_engine();
    let case = "the limit a modification moves";
    let identifier = "emu-autoscale-limit";
    available_instance(&router, case, identifier).await;
    // A client that raises the limit is reported the new one, and the storage itself is
    // untouched by the change: `MaxAllocatedStorage` is a bound on what RDS may grow the
    // instance to, not a target it grows to at once.
    let raised = "200";
    expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("MaxAllocatedStorage", raised),
                ("ApplyImmediately", "true"),
            ],
        )
        .await,
    );
    let (allocated, max) = reported_storage(&router, case, identifier).await;
    assert_eq!(
        (allocated.as_str(), max.as_str()),
        (ALLOCATED_STORAGE, raised),
        "case {case:?}: expected the raised limit beside the storage the instance still has"
    );
    // And the raise a storage modification of this instance would make is bounded by it.
    let allocated: i32 = ALLOCATED_STORAGE.parse().expect("a number of GiB");
    let limit: i32 = raised.parse().expect("a number of GiB");
    let target = st::storage_autoscaling::autoscaled_allocated_storage(allocated, limit);
    assert!(
        target.is_some_and(|target| target > allocated && target <= limit),
        "case {case:?}: expected a raise inside the limit, got {target:?}"
    );
}
