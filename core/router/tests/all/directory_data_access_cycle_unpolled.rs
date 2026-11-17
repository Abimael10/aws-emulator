// SPDX-License-Identifier: CC0-1.0

//! The DS Data feature switch as a client that does NOT watch it sees it: what the FIRST
//! call after a clock-owed transition's due instant reports.
//!
//! `ds:CreateMicrosoftAD`, `ds:EnableDirectoryDataAccess` and
//! `ds:DisableDirectoryDataAccess` all answer BEFORE the state they ask for is reached: each
//! records a transient value (`Requested`/`Creating`, `Enabling`, `Disabling`) and stores the
//! instant the settled one falls due. The rule this suite pins is what that storing is for:
//! the transition is owed by the CLOCK, so the first request that touches the directory after
//! that instant already sees the settled state — however many requests came before it, and
//! even when NONE did — and no request advances it. A client therefore spends one call per
//! transition to observe a settled status, whatever cadence it calls at, and a whole
//! create -> enable -> disable -> enable cycle costs it the spans the service publishes
//! rather than one polling interval per step.
//!
//! Both halves of that rule need BOTH services: the control plane owns the status and the
//! data plane is gated on the same record, so a directory whose cycle was driven without the
//! control plane ever being read must still be served and refused at exactly the instants
//! the switch moved.
//!
//! What makes these cases a test of the STORED due instant rather than of polling: the wait
//! is a poll loop against a SECOND emulator — its own engine, its own store — carrying a
//! directory driven through the same leg a moment LATER than the one under test. While that
//! clock witness is polled, the store under test receives no request at all, so the single
//! call that follows the window has to answer from the instants the earlier calls recorded.
//! A status that only moved when something looked at it (one step per call, say) would still
//! satisfy a client that polls, and is exactly what costs an unwatched client an extra
//! interval per leg; here it answers the transient value and the case fails.
//!
//! The witness is what lets that window be waited out without waiting a FIXED time for a
//! transition nothing is allowed to observe: the wait ends when a store that took the same leg
//! reports it settled, so a loaded or slow host cannot make the window too short, and a change
//! to the spans the control plane publishes needs no edit here.
//!
//! No case here reads the directory under test between the call that starts a leg and the end
//! of that leg's window, so no outcome depends on how fast the host runs: waiting longer than
//! a due instant can only make the settled value MORE due. The other half of the rule — that
//! the span is not zero, so the transient value is observable straight after the call — is
//! pinned by the sibling suites `directory_data_access_cycle` (both planes) and the control
//! plane's own `directory_data_access` / `directory_data_access_disable`, together with the
//! content of the cycle: the identical refusal a read, a write and a listing get, and the
//! objects that survive it. What is new here is that nothing polls.

use std::time::Duration;

use serde_json::{json, Value};

use super::directory_data_plane_authorization::{create_microsoft_ad, Answer, Emulator};

/// The two stages a create's chain passes THROUGH, and the one it ends on
/// (`smithy_query(query="shape", shape="DirectoryStage")` carries `Requested`, `Creating`
/// and `Active`). The first two are named by the failure messages below: they are what a read
/// reports when a step of the chain was not materialised from the instant the create stored.
const STAGE_REQUESTED: &str = "Requested";
const STAGE_CREATING: &str = "Creating";
const STAGE_ACTIVE: &str = "Active";

/// The four values of the modelled `DataAccessStatus` the two toggles move a directory
/// between (`smithy_query(query="shape", shape="DataAccessStatus")`: `Disabled`,
/// `Disabling`, `Enabled`, `Enabling`, `Failed`).
const DISABLED: &str = "Disabled";
const DISABLING: &str = "Disabling";
const ENABLED: &str = "Enabled";
const ENABLING: &str = "Enabling";

/// The error a data-plane call gets for a directory whose feature is out of service, and the
/// modelled `AccessDeniedReason` it carries (`smithy_query(query="shape",
/// shape="AccessDeniedException")`: `@httpError` 403, members `Message` / `Reason`;
/// `AccessDeniedReason` is `IAM_AUTH`, `DIRECTORY_AUTH`, `DATA_DISABLED`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const DATA_DISABLED: &str = "DATA_DISABLED";

/// The whole sentence that refusal states: fixed text naming the control-plane call that
/// turns the feature on, with nothing of the request in it.
const DATA_DISABLED_MESSAGE: &str = "DS Data feature is not enabled on this directory. \
     This feature must first be enabled by calling the EnableDirectoryDataAccess API.";

/// How long a poll loop waits between looks at the clock witness. Short enough that the
/// window a case waits out is the service's own span rather than this cadence.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How many of those looks a witness gets before a case gives up. The product is forty
/// seconds, many times over the longest chain the control plane publishes — a transition span
/// followed by the directory build span — so it bounds a WEDGED status rather than timing a
/// transition.
const POLL_ATTEMPTS: usize = 400;

/// The objects the cycle is driven around: the user and group created while the feature is in
/// service, and the name whose create is attempted while it is shut.
const USER: &str = "unpolledSurvivor";
const REFUSED_USER: &str = "unpolledNeverCreated";

/// The `Stage` the control plane reports for `directory`.
async fn stage(emulator: &Emulator, directory: &str) -> String {
    let described = emulator
        .control_plane("DescribeDirectories", json!({"DirectoryIds": [directory]}))
        .await;
    described.body["DirectoryDescriptions"][0]["Stage"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `DataAccessStatus` the control plane reports for `directory`.
async fn data_access_status(emulator: &Emulator, directory: &str) -> String {
    let described = emulator
        .control_plane(
            "DescribeDirectoryDataAccess",
            json!({"DirectoryId": directory}),
        )
        .await;
    described.text("DataAccessStatus")
}

/// `CreateMicrosoftAD` for `name`, returning the id it minted.
async fn create(emulator: &Emulator, name: &str, case: &str) -> String {
    let created = emulator
        .control_plane("CreateMicrosoftAD", create_microsoft_ad(name))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected CreateMicrosoftAD to be accepted, got {} with {:?}",
        created.status, created.body
    );
    let directory = created.text("DirectoryId");
    assert!(
        !directory.is_empty(),
        "case {case:?}: expected a DirectoryId, got {:?}",
        created.body
    );
    directory
}

/// One accepted toggle of the switch.
async fn toggle(emulator: &Emulator, operation: &str, directory: &str, case: &str) {
    let answer = emulator
        .control_plane(operation, json!({"DirectoryId": directory}))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected {operation} to be accepted, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Wait out a transition by polling the clock WITNESS — never the directory under test —
/// until its own stage has reached `wanted`.
async fn witness_reaches_stage(emulator: &Emulator, directory: &str, wanted: &str, case: &str) {
    for _ in 0..POLL_ATTEMPTS {
        if stage(emulator, directory).await == wanted {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let last = stage(emulator, directory).await;
    panic!(
        "case {case:?}: expected the clock witness to reach Stage {wanted:?}, got {last:?}"
    );
}

/// The same, for a leg of the switch: polled until the witness's own `DataAccessStatus` has
/// settled on `wanted`.
async fn witness_settles_on(emulator: &Emulator, directory: &str, wanted: &str, case: &str) {
    for _ in 0..POLL_ATTEMPTS {
        if data_access_status(emulator, directory).await == wanted {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let last = data_access_status(emulator, directory).await;
    panic!(
        "case {case:?}: expected the clock witness to settle on DataAccessStatus \
         {wanted:?}, got {last:?}"
    );
}

/// Assert `answer` is the refusal a directory out of service gets: 403
/// `AccessDeniedException`, `Reason` `DATA_DISABLED` and the fixed sentence.
fn assert_data_disabled(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        ACCESS_DENIED_EXCEPTION,
        "case {case:?}: expected {ACCESS_DENIED_EXCEPTION}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body.get("Reason").and_then(Value::as_str),
        Some(DATA_DISABLED),
        "case {case:?}: expected Reason {DATA_DISABLED}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body.get("Message").and_then(Value::as_str),
        Some(DATA_DISABLED_MESSAGE),
        "case {case:?}: expected Message {DATA_DISABLED_MESSAGE:?}, got {:?}",
        answer.body
    );
}

/// RULE: every leg of the cycle is settled by the instant the call that started it stored, so
/// the FIRST read after that instant reports the settled value — including the create's leg,
/// which is a CHAIN of two steps and is materialised whole — and reading again does not move
/// it on.
///
/// The rows are the four legs the switch's cycle is made of — the create's stage chain, then
/// the enable, the disable and the enable that opens it again — each driven on a directory
/// that is READ for the first time only once that leg's window has passed, so what the read
/// reports can come from nothing but the instant the earlier call stored.
#[tokio::test]
async fn every_leg_of_the_cycle_settles_for_the_first_read_after_its_due_instant() {
    let subject = Emulator::new();
    let witness = Emulator::new();

    // The directory under test is created BEFORE the witness's, so every due instant its
    // calls store is at or before the matching one in the witness's store: a witness that
    // has settled is proof this directory's transition had fallen due too.
    let case = "the create's stage chain";
    let watched = create(&subject, "unpolled.example.com", case).await;
    let pacer = create(&witness, "pacer.example.com", case).await;
    witness_reaches_stage(&witness, &pacer, STAGE_ACTIVE, case).await;

    // The FIRST read this directory has ever had, and the create owed TWO steps:
    // `Requested` -> `Creating` -> `Active`. Both fell due while nothing was looking, so this
    // read reports the end of the chain rather than its middle.
    let settled = stage(&subject, &watched).await;
    assert_eq!(
        settled, STAGE_ACTIVE,
        "case {case:?}: expected the first read after the chain's due instants to report \
         {STAGE_ACTIVE} rather than a step the chain passed through ({STAGE_REQUESTED} or \
         {STAGE_CREATING}), got {settled:?}"
    );
    let repeated = stage(&subject, &watched).await;
    assert_eq!(
        repeated, STAGE_ACTIVE,
        "case {case:?}: expected a repeated read to report {STAGE_ACTIVE} as well, got \
         {repeated:?}"
    );

    // The three legs of the switch the scenario drives, in that order: the enable, the
    // disable that recloses it, and the enable that opens it again.
    for (operation, transient, wanted) in [
        ("EnableDirectoryDataAccess", ENABLING, ENABLED),
        ("DisableDirectoryDataAccess", DISABLING, DISABLED),
        ("EnableDirectoryDataAccess", ENABLING, ENABLED),
    ] {
        let case = format!("{operation} settling on {wanted}");
        toggle(&subject, operation, &watched, &case).await;

        // The witness takes the same leg a moment later and is the only thing polled: the
        // directory under test gets no request at all while its transition falls due.
        toggle(&witness, operation, &pacer, &case).await;
        witness_settles_on(&witness, &pacer, wanted, &case).await;

        let reported = data_access_status(&subject, &watched).await;
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected the first read after the due instant to report \
             {wanted} rather than the transient {transient} the toggle recorded, got \
             {reported:?}"
        );
        let again = data_access_status(&subject, &watched).await;
        assert_eq!(
            again, wanted,
            "case {case:?}: expected a repeated read to report {wanted} as well, got \
             {again:?}"
        );

        // The switch moves the data plane, not the directory: the stage stays where the
        // create's chain left it through every leg.
        let stage_now = stage(&subject, &watched).await;
        assert_eq!(
            stage_now, STAGE_ACTIVE,
            "case {case:?}: expected the directory to stay {STAGE_ACTIVE} while its data \
             plane moves, got {stage_now:?}"
        );
    }
}

/// RULE: the data plane is gated on the same clock, so for a directory whose cycle is driven
/// without the control plane ever being READ, the first data-plane call after each leg's due
/// instant already sees that leg — served once the enable is settled, refused once the
/// disable is, and serving the same objects after the re-enable.
#[tokio::test]
async fn the_data_plane_follows_the_clock_for_a_directory_nothing_describes() {
    let subject = Emulator::new();
    let witness = Emulator::new();

    let case = "a directory nothing describes";
    let watched = create(&subject, "unread.example.com", case).await;
    let pacer = create(&witness, "pacer.example.com", case).await;
    witness_reaches_stage(&witness, &pacer, STAGE_ACTIVE, case).await;

    // The enable is the first call to reach this directory since its create, and it needs a
    // directory in service: it is admitted only because the stage chain was materialised
    // from the instants the create stored, with nothing having polled it.
    let case = "the enable of a directory nothing describes";
    toggle(&subject, "EnableDirectoryDataAccess", &watched, case).await;
    toggle(&witness, "EnableDirectoryDataAccess", &pacer, case).await;
    witness_settles_on(&witness, &pacer, ENABLED, case).await;

    // The first data-plane request this directory has ever had, taken after the enable's due
    // instant: the feature is in service, so it is served.
    let listed = subject
        .data_plane("/Users/ListUsers", &watched, json!({}))
        .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected the first listing after the enable settled to be served, \
         got {} with {:?}",
        listed.status, listed.body
    );
    let created_user = subject
        .data_plane(
            "/Users/CreateUser",
            &watched,
            json!({"SAMAccountName": USER, "GivenName": "Un", "Surname": "Polled"}),
        )
        .await;
    assert_eq!(
        created_user.status, 200,
        "case {case:?}: expected CreateUser to be served, got {} with {:?}",
        created_user.status, created_user.body
    );
    let sid_before = created_user.text("SID");
    assert!(
        !sid_before.is_empty(),
        "case {case:?}: expected the created user to carry a SID, got {:?}",
        created_user.body
    );

    // The disable, whose own due instant is what shuts the plane: nothing touches this
    // directory again until the witness says that instant has passed.
    let case = "the disable of a directory nothing describes";
    toggle(&subject, "DisableDirectoryDataAccess", &watched, case).await;
    toggle(&witness, "DisableDirectoryDataAccess", &pacer, case).await;
    witness_settles_on(&witness, &pacer, DISABLED, case).await;

    // The first calls after the disable's due instant: one identical refusal for a read, a
    // write and an inventory listing, because what is out of service is the directory.
    let read = subject
        .data_plane(
            "/Users/DescribeUser",
            &watched,
            json!({"SAMAccountName": USER}),
        )
        .await;
    let write = subject
        .data_plane(
            "/Users/CreateUser",
            &watched,
            json!({"SAMAccountName": REFUSED_USER, "GivenName": "Never", "Surname": "Made"}),
        )
        .await;
    let listing = subject
        .data_plane("/Users/ListUsers", &watched, json!({}))
        .await;
    for (which, answer) in [
        ("a read", &read),
        ("a write", &write),
        ("a listing", &listing),
    ] {
        assert_data_disabled(&format!("{case}: {which} once the disable settled"), answer);
    }
    assert_eq!(
        read.body, write.body,
        "case {case:?}: expected a read and a write to carry the identical refusal, got \
         {:?} and {:?}",
        read.body, write.body
    );
    assert_eq!(
        read.body, listing.body,
        "case {case:?}: expected a read and a listing to carry the identical refusal, got \
         {:?} and {:?}",
        read.body, listing.body
    );

    // Only now is the control plane read, for the first time since the create: it reports
    // the leg the data plane has been following, and a directory still in service.
    let status = data_access_status(&subject, &watched).await;
    assert_eq!(
        status, DISABLED,
        "case {case:?}: expected the control plane's first read of the cycle to report \
         {DISABLED}, got {status:?}"
    );
    let stage_now = stage(&subject, &watched).await;
    assert_eq!(
        stage_now, STAGE_ACTIVE,
        "case {case:?}: expected the directory to stay {STAGE_ACTIVE} while its data plane \
         is shut, got {stage_now:?}"
    );

    // And the re-enable: the first call after its due instant is served, with the object the
    // cycle was supposed to keep and without the one the refused write never made.
    let case = "the re-enable of a directory nothing describes";
    toggle(&subject, "EnableDirectoryDataAccess", &watched, case).await;
    toggle(&witness, "EnableDirectoryDataAccess", &pacer, case).await;
    witness_settles_on(&witness, &pacer, ENABLED, case).await;

    let described = subject
        .data_plane(
            "/Users/DescribeUser",
            &watched,
            json!({"SAMAccountName": USER}),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected the first read after the re-enable settled to be served, \
         got {} with {:?}",
        described.status, described.body
    );
    assert_eq!(
        described.text("SID"),
        sid_before,
        "case {case:?}: expected the same AD object across the cycle, got {:?}",
        described.body
    );
    let inventory = subject
        .data_plane("/Users/ListUsers", &watched, json!({}))
        .await;
    let names: Vec<String> = inventory.body["Users"]
        .as_array()
        .map(|users| {
            users
                .iter()
                .filter_map(|user| user["SAMAccountName"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.iter().any(|name| name == USER) && !names.iter().any(|name| name == REFUSED_USER),
        "case {case:?}: expected {USER:?} and not {REFUSED_USER:?} in the inventory, got \
         {names:?}"
    );
}
