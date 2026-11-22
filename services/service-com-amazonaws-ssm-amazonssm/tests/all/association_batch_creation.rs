// SPDX-License-Identifier: CC0-1.0

//! Creating State Manager associations in a batch, end to end: `CreateAssociationBatch`
//! persisting one association per entry and reporting what became of every entry.
//!
//! RULE: a batch creates each of its entries exactly as a single create creates one — the
//! document resolved against the store, the version it names selected from that document,
//! the parameters held to the ones that version declares — reporting every association it
//! persisted in `Successful` and every entry it could not create in `Failed` with that
//! entry, a description of the failure and its source, while a fault of the REQUEST as a
//! whole (a managed node named by more than one entry, a batch that would take the Region
//! past the documented association limit) refuses the call and persists nothing.
//!
//! Every case goes through the front door a client uses, over the dispatch
//! [`crate::state_manager_associations`] publishes, so this suite shares one calling
//! convention and one reading of the store with the create, update and listing suites.
//!
//! The suites are tables. Each row is one input along the axis the rule generalises over —
//! how many entries one batch carries, which targeting, schedule, parameter and document
//! form each entry takes, and which class of refusal an entry runs into — rather than the
//! single batch the recorded units happened to send.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ssm_amazonssm::state::{association_create, associations};

use crate::state_manager_associations::{
    call, call_in_region, created, described, error_code, service, stored_associations,
    without_overview, AGENT, NODE, OTHER_NODE, OTHER_REGION, REGION, SHELL,
};

/// A role of the form the modelled `AssociationDispatchAssumeRoleArn` pattern admits,
/// named by the REQUEST rather than by an entry, so every association a batch creates
/// dispatches under it.
const DISPATCH_ROLE: &str = "arn:aws:iam::123456789012:role/StateManagerDispatch";

/// `CreateAssociationBatch` with `body`, signed for [`REGION`].
async fn batch(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "CreateAssociationBatch", body).await
}

/// The response of a batch that must be served.
async fn batched(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = batch(router, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateAssociationBatch, got {status} with \
         {parsed:?}"
    );
    parsed
}

/// A list member of a response, which this operation reports even when it is empty.
fn reported(response: &Value, member: &str) -> Vec<Value> {
    assert!(
        response[member].is_array(),
        "expected {member:?} to be reported as a list, got {response:?}"
    );
    response[member].as_array().cloned().unwrap_or_default()
}

/// A string member of a response body.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// One batch: the entries it carries and the document name each association it creates
/// reports, in entry order.
struct BatchRow {
    case: &'static str,
    entries: Vec<Value>,
    names: Vec<&'static str>,
}

/// The table of batches: one row per form a batch can take — how many entries it carries
/// and which targeting form, schedule form, parameter set, document and document version
/// each of those entries takes.
fn batch_rows() -> Vec<BatchRow> {
    vec![
        BatchRow {
            case: "one entry, a command document with no targets",
            entries: vec![json!({"Name": AGENT})],
            names: vec![AGENT],
        },
        BatchRow {
            case: "two entries, node-id targets with a cron schedule and tag targets with a \
                   rate schedule",
            entries: vec![
                json!({
                    "Name": AGENT,
                    "Targets": [{"Key": "InstanceIds", "Values": [NODE, OTHER_NODE]}],
                    "ScheduleExpression": "cron(0 2 ? * SUN *)",
                    "AssociationName": "nightly-agent-update",
                    "ApplyOnlyAtCronInterval": true,
                    "MaxConcurrency": "10%",
                    "MaxErrors": "2",
                }),
                json!({
                    "Name": SHELL,
                    "Targets": [{"Key": "tag:Env", "Values": ["prod"]}],
                    "ScheduleExpression": "rate(12 hours)",
                    "Parameters": {"commands": ["echo one", "echo two"]},
                    "ComplianceSeverity": "MEDIUM",
                    "SyncCompliance": "MANUAL",
                }),
            ],
            names: vec![AGENT, SHELL],
        },
        BatchRow {
            case: "three entries spanning the targeting forms, including the deprecated \
                   InstanceId member and a target map",
            entries: vec![
                json!({"Name": AGENT, "InstanceId": NODE}),
                json!({
                    "Name": SHELL,
                    "Parameters": {"commands": ["echo three"]},
                    "CalendarNames": ["AWS-Calendar"],
                    "Duration": 3,
                }),
                json!({
                    "Name": AGENT,
                    "TargetMaps": [{"BucketName": ["logs"]}],
                    "DocumentVersion": "$DEFAULT",
                    "ScheduleOffset": 2,
                    "ScheduleExpression": "cron(0 3 ? * MON *)",
                }),
            ],
            names: vec![AGENT, SHELL, AGENT],
        },
        BatchRow {
            case: "two entries of the same document, one of them naming the document's \
                   first version",
            entries: vec![
                json!({"Name": AGENT, "DocumentVersion": "1"}),
                json!({"Name": AGENT, "AssociationName": "second-of-the-same-document"}),
            ],
            names: vec![AGENT, AGENT],
        },
    ]
}

/// A batch persists one association per entry, reports each of them in `Successful`, and
/// what it persisted is what a later read reports.
#[tokio::test]
async fn a_batch_creates_one_association_per_entry() {
    for BatchRow {
        case,
        entries,
        names,
    } in batch_rows()
    {
        let (router, engine) = service();
        let response = batched(
            &router,
            case,
            &json!({"Entries": entries, "AssociationDispatchAssumeRole": DISPATCH_ROLE}),
        )
        .await;

        let successful = reported(&response, "Successful");
        assert_eq!(
            (successful.len(), reported(&response, "Failed").len()),
            (names.len(), 0),
            "case {case:?}: expected one created association per entry and no failure, got \
             {response:?}"
        );
        assert_eq!(
            stored_associations(&engine),
            names.len(),
            "case {case:?}: the store holds one association per entry, got {response:?}"
        );

        let mut ids: Vec<String> = Vec::new();
        for (position, (description, name)) in successful.iter().zip(&names).enumerate() {
            assert_eq!(
                (
                    text(description, "Name").as_str(),
                    text(description, "AssociationVersion").as_str()
                ),
                (*name, "1"),
                "case {case:?}: entry {position} reports the document it associates and the \
                 version a create mints, got {description:?}"
            );
            assert_eq!(
                text(description, "AssociationDispatchAssumeRole"),
                DISPATCH_ROLE,
                "case {case:?}: entry {position} dispatches under the role the request named, \
                 got {description:?}"
            );
            let id = text(description, "AssociationId");
            assert!(
                !ids.contains(&id),
                "case {case:?}: entry {position} was minted an id another entry already has \
                 ({id}), got {response:?}"
            );

            // Round trip: the association the batch persisted is the one a read reports.
            let read = described(&router, case, &json!({"AssociationId": id})).await;
            assert_eq!(
                without_overview(&read),
                without_overview(description),
                "case {case:?}: entry {position} reads back as the batch reported it"
            );
            ids.push(id);
        }

        // And every one of them is in the listing the Region serves.
        let (_, listing) = call(&router, "ListAssociations", &json!({"MaxResults": 50})).await;
        let listed: Vec<String> = reported(&listing, "Associations")
            .iter()
            .map(|association| text(association, "AssociationId"))
            .collect();
        for id in &ids {
            assert!(
                listed.contains(id),
                "case {case:?}: expected the listing to report the created association {id}, \
                 got {listing:?}"
            );
        }
    }
}

/// The entry a failure carries is the entry AS IT WAS SENT: every member the request set,
/// and no member it did not — beside `ApplyOnlyAtCronInterval`, whose modelled default the
/// centralized walk applies to an entry that named none.
fn assert_entry_echoed(sent: &Value, echoed: &Value, case: &str) {
    let sent = sent.as_object().expect("an entry is an object");
    for (member, value) in sent {
        assert_eq!(
            &echoed[member], value,
            "case {case:?}: expected the failure to echo {member:?} as it was sent, got \
             {echoed:?}"
        );
    }
    let extra: Vec<&str> = echoed
        .as_object()
        .expect("the echoed entry is an object")
        .keys()
        .map(String::as_str)
        .filter(|member| !sent.contains_key(*member) && *member != "ApplyOnlyAtCronInterval")
        .collect();
    assert!(
        extra.is_empty(),
        "case {case:?}: the failure echoes members the entry never set ({extra:?}), got \
         {echoed:?}"
    );
}

/// One entry a batch cannot create: what prepares the store for it, the entry, the
/// description the failure carries, and the error a single create of that same entry is
/// refused with.
struct FailureRow {
    case: &'static str,
    /// A `CreateAssociation` to make first, for a refusal that is about what the store
    /// already holds.
    prepared: Option<Value>,
    entry: Value,
    message: String,
    /// The modelled error the same entry is refused with when it is created on its own —
    /// which is the class of refusal the batch reports instead of throwing.
    code: &'static str,
}

/// The table of refusals: one row per class of failure an entry can run into.
fn failure_rows() -> Vec<FailureRow> {
    vec![
        FailureRow {
            case: "a document that doesn't exist",
            prepared: None,
            entry: json!({"Name": "no-such-document"}),
            message: "Document does not exist: no-such-document".to_string(),
            code: "InvalidDocument",
        },
        FailureRow {
            case: "a version the document doesn't carry",
            prepared: None,
            entry: json!({"Name": AGENT, "DocumentVersion": "2"}),
            message: format!("Document {AGENT} has no version '2'"),
            code: "InvalidDocumentVersion",
        },
        FailureRow {
            case: "a parameter the document doesn't declare",
            prepared: None,
            entry: json!({"Name": AGENT, "Parameters": {"nope": ["x"]}}),
            message: format!("Document {AGENT} doesn't support a parameter named nope"),
            code: "InvalidParameters",
        },
        FailureRow {
            case: "a required parameter left out",
            prepared: None,
            entry: json!({"Name": SHELL}),
            // `AWS-RunShellScript` declares `commands` with no default, so a create has to
            // supply a value for it (`state::PUBLISHED_DOCUMENTS`).
            message: "Missing required parameters: commands".to_string(),
            code: "InvalidParameters",
        },
        FailureRow {
            case: "a schedule in neither of the two forms",
            prepared: None,
            entry: json!({"Name": AGENT, "ScheduleExpression": "every 5 minutes"}),
            message: associations::INVALID_SCHEDULE.to_string(),
            code: "InvalidSchedule",
        },
        FailureRow {
            case: "an output location naming no bucket",
            prepared: None,
            entry: json!({
                "Name": AGENT,
                "OutputLocation": {"S3Location": {"OutputS3KeyPrefix": "out"}},
            }),
            message: association_create::INVALID_OUTPUT_LOCATION.to_string(),
            code: "InvalidOutputLocation",
        },
        FailureRow {
            case: "targets and target maps together",
            prepared: None,
            entry: json!({
                "Name": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
                "TargetMaps": [{"BucketName": ["logs"]}],
            }),
            message: associations::TARGETS_AND_TARGET_MAPS.to_string(),
            code: "InvalidTargetMaps",
        },
        FailureRow {
            case: "a node already carrying that document's association",
            prepared: Some(json!({"Name": AGENT, "InstanceId": NODE})),
            entry: json!({"Name": AGENT, "InstanceId": NODE}),
            message: association_create::ASSOCIATION_ALREADY_EXISTS.to_string(),
            code: "AssociationAlreadyExists",
        },
    ]
}

/// An entry a batch cannot create is REPORTED rather than refusing the call: the entries
/// beside it are created, and the failure carries the entry, a description and its source.
#[tokio::test]
async fn an_entry_that_cannot_be_created_is_reported_in_failed() {
    for FailureRow {
        case,
        prepared,
        entry,
        message,
        code,
    } in failure_rows()
    {
        let (router, engine) = service();
        let mut held = 0;
        if let Some(prepared) = &prepared {
            created(&router, case, prepared).await;
            held += 1;
        }

        // The failing entry travels beside a sound one, which the batch still creates.
        let sound = json!({"Name": AGENT, "InstanceId": OTHER_NODE});
        let response = batched(
            &router,
            case,
            &json!({"Entries": [entry.clone(), sound.clone()]}),
        )
        .await;

        let successful = reported(&response, "Successful");
        let failed = reported(&response, "Failed");
        assert_eq!(
            (successful.len(), failed.len()),
            (1, 1),
            "case {case:?}: expected the sound entry created and the other reported failed, \
             got {response:?}"
        );
        assert_eq!(
            text(&successful[0], "InstanceId"),
            OTHER_NODE,
            "case {case:?}: the association created is the sound entry's, got {response:?}"
        );
        assert_entry_echoed(&entry, &failed[0]["Entry"], case);
        assert_eq!(
            text(&failed[0], "Message"),
            message,
            "case {case:?}: expected the failure's own description, got {response:?}"
        );
        assert_eq!(
            text(&failed[0], "Fault"),
            "Client",
            "case {case:?}: the entry is what the failure is a property of, got {response:?}"
        );
        assert_eq!(
            stored_associations(&engine),
            held + 1,
            "case {case:?}: the failed entry persists nothing and the sound one persists one \
             association, got {response:?}"
        );

        // The same entry on its own is refused with the modelled error of that class, which
        // is the refusal the batch reports instead of throwing.
        let (status, parsed) = call(&router, "CreateAssociation", &entry).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, code),
            "case {case:?}: expected a single create of the same entry to be refused with \
             {code}, got {status} with {parsed:?}"
        );
    }
}

/// A managed node may be named by at most one entry of a batch, and a request that names
/// one twice is refused whole.
#[tokio::test]
async fn a_node_named_by_more_than_one_entry_refuses_the_whole_batch() {
    // One row per way a batch can name nodes through the deprecated `InstanceId` member:
    // the same node twice, the same node three times, two different nodes, and one node
    // named once beside an entry naming none.
    let rows: Vec<(&str, Vec<Value>, Option<&str>)> = vec![
        (
            "the same node in two entries",
            vec![
                json!({"Name": AGENT, "InstanceId": NODE}),
                json!({"Name": SHELL, "InstanceId": NODE, "Parameters": {"commands": ["hi"]}}),
            ],
            Some("DuplicateInstanceId"),
        ),
        (
            "the same node in two of three entries",
            vec![
                json!({"Name": AGENT, "InstanceId": NODE}),
                json!({"Name": AGENT}),
                json!({"Name": AGENT, "InstanceId": NODE}),
            ],
            Some("DuplicateInstanceId"),
        ),
        (
            "two different nodes",
            vec![
                json!({"Name": AGENT, "InstanceId": NODE}),
                json!({"Name": AGENT, "InstanceId": OTHER_NODE}),
            ],
            None,
        ),
        (
            "one node named once beside an entry naming none",
            vec![
                json!({"Name": AGENT, "InstanceId": NODE}),
                json!({"Name": AGENT, "Targets": [{"Key": "InstanceIds", "Values": [NODE]}]}),
            ],
            None,
        ),
    ];

    for (case, entries, refusal) in rows {
        let (router, engine) = service();
        let wanted = entries.len();
        let (status, parsed) = batch(&router, &json!({"Entries": entries})).await;
        match refusal {
            Some(refusal) => {
                assert_eq!(
                    (status, error_code(&parsed).as_str()),
                    (400, refusal),
                    "case {case:?}: expected the whole batch refused with {refusal}, got \
                     {status} with {parsed:?}"
                );
                assert_eq!(
                    stored_associations(&engine),
                    0,
                    "case {case:?}: a refused batch persists no association"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the batch to be served, got {status} with \
                     {parsed:?}"
                );
                assert_eq!(
                    (reported(&parsed, "Successful").len(), stored_associations(&engine)),
                    (wanted, wanted),
                    "case {case:?}: expected every entry created, got {parsed:?}"
                );
            }
        }
    }
}

/// The documented association limit is measured over the Region the request was signed
/// for: an entry past it is reported like any other entry that cannot be created, and a
/// Region that can take no association at all refuses the whole call.
#[tokio::test]
async fn the_regions_association_limit_bounds_what_a_batch_creates() {
    let (router, engine) = service();

    // Fill the Region to one short of the documented limit, through the front door, so
    // every record counted is one a create persisted.
    let page = 250;
    while stored_associations(&engine) + page < associations::MAX_ASSOCIATIONS_PER_REGION {
        let entries: Vec<Value> = (0..page).map(|_| json!({"Name": AGENT})).collect();
        batched(&router, "filling the Region", &json!({"Entries": entries})).await;
    }
    while stored_associations(&engine) + 1 < associations::MAX_ASSOCIATIONS_PER_REGION {
        batched(
            &router,
            "filling the Region",
            &json!({"Entries": [{"Name": AGENT}]}),
        )
        .await;
    }
    assert_eq!(
        stored_associations(&engine),
        associations::MAX_ASSOCIATIONS_PER_REGION - 1,
        "the Region holds one association short of the documented limit"
    );

    // Two entries, one place left: the first is created, and the second is reported in
    // `Failed` with the limit's own description rather than refusing the call.
    let case = "two entries for the one place the Region has left";
    let response = batched(
        &router,
        case,
        &json!({"Entries": [{"Name": AGENT}, {"Name": AGENT}]}),
    )
    .await;
    let failed = reported(&response, "Failed");
    assert_eq!(
        (reported(&response, "Successful").len(), failed.len()),
        (1, 1),
        "case {case:?}: expected the place that was left filled and the other entry \
         reported, got {response:?}"
    );
    assert_eq!(
        (text(&failed[0], "Message"), text(&failed[0], "Fault")),
        (
            association_create::ASSOCIATION_LIMIT_EXCEEDED.to_string(),
            "Client".to_string()
        ),
        "case {case:?}: expected the documented limit as the failure's description, got \
         {response:?}"
    );
    assert_eq!(
        stored_associations(&engine),
        associations::MAX_ASSOCIATIONS_PER_REGION,
        "case {case:?}: the Region now holds the limit's worth of associations"
    );

    // The Region can now take no association at all, so no entry of a batch could be
    // created and the whole call is refused.
    let held = stored_associations(&engine);
    let (status, parsed) = batch(
        &router,
        &json!({"Entries": [{"Name": AGENT}, {"Name": AGENT}]}),
    )
    .await;
    assert_eq!(
        (status, error_code(&parsed).as_str()),
        (400, "AssociationLimitExceeded"),
        "a batch sent to a Region at its limit is refused, got {status} with {parsed:?}"
    );
    assert_eq!(
        stored_associations(&engine),
        held,
        "a batch refused at the Region's limit persists no association"
    );
}

/// How many entries one batch carries is bounded by this emulator rather than by the
/// model, and a request past that ceiling is refused before anything is created.
#[tokio::test]
async fn a_batch_carries_at_most_the_entries_this_emulator_serves() {
    // One row per size a batch can take around the ceiling: one entry, a handful, the
    // ceiling itself and one past it.
    let ceiling = association_create::MAX_BATCH_ENTRIES;
    let rows: [(&str, usize, bool); 4] = [
        ("one entry", 1, true),
        ("a handful of entries", 8, true),
        ("the ceiling itself", ceiling, true),
        ("one entry past the ceiling", ceiling + 1, false),
    ];

    for (case, count, served) in rows {
        let (router, engine) = service();
        let entries: Vec<Value> = (0..count).map(|_| json!({"Name": AGENT})).collect();
        let (status, parsed) = batch(&router, &json!({"Entries": entries})).await;
        if served {
            assert_eq!(
                (status, stored_associations(&engine)),
                (200, count),
                "case {case:?}: expected every entry created, got {status} with {parsed:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected a batch past the ceiling refused, got {status} \
                 with {parsed:?}"
            );
            assert_eq!(
                stored_associations(&engine),
                0,
                "case {case:?}: a batch past the ceiling persists no association"
            );
        }
    }
}

/// The Region an association is created in comes from the request's credential scope, so a
/// batch sent to another Region's endpoint creates associations only that endpoint reports.
#[tokio::test]
async fn a_batch_creates_its_associations_in_the_region_it_was_signed_for() {
    let (router, _engine) = service();
    let response = batched(
        &router,
        "a batch signed for the suite's Region",
        &json!({"Entries": [{"Name": AGENT}, {"Name": AGENT, "AssociationName": "second"}]}),
    )
    .await;
    let ids: Vec<String> = reported(&response, "Successful")
        .iter()
        .map(|description| text(description, "AssociationId"))
        .collect();

    for id in &ids {
        let (status, parsed) = call_in_region(
            &router,
            OTHER_REGION,
            "DescribeAssociation",
            &json!({"AssociationId": id}),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "AssociationDoesNotExist"),
            "an association created through {REGION} is not reachable through another \
             Region's endpoint, got {status} with {parsed:?}"
        );
    }
}
