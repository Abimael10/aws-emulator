// SPDX-License-Identifier: CC0-1.0

//! Reading the version HISTORY of a resource this service versions, end to end:
//! `ListAssociationVersions` over an association a create and its updates have versioned, and
//! `ListDocumentVersions` over a document — including one Systems Manager publishes.
//!
//! RULE 1: a version listing reports the versions it holds NEWEST VERSION FIRST, so the first
//! entry is the version in force — what the resource is configured with now — and the last is
//! the one its create minted.
//!
//! RULE 2: a version of an association is the association's WHOLE configuration as of that
//! version — "the targets specified for the association when the association version was
//! created", "the cron or rate schedule specified for the association when the association
//! version was created" (`AssociationVersionInfo$Targets`, `$ScheduleExpression`) — so a
//! member an update does not carry is reported on the version that update appended as well,
//! and the entry a client reads first says what the association is doing.
//!
//! RULE 3: `CalendarNames` is a list on every version: an association no Change Calendar gates
//! is gated under NONE of them, which is the empty list rather than an absent member.
//!
//! Every case goes through the front door a client uses: the association suites over the
//! dispatch [`crate::state_manager_associations`] publishes, the document suite over the one
//! [`crate::parameter_store`] publishes, so nothing here reaches into the state struct.
//!
//! The suites are tables. Each row is one input along the axis a rule generalises over — how
//! many versions the history holds, which member the call that appended one carried, which
//! members it left for the configuration in force to supply, and (for documents) whether the
//! document is one a caller created or one Systems Manager publishes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{call, created, described, service, AGENT, NODE, SHELL};

/// The `AssociationVersions` of a listing that must have succeeded, in the order the listing
/// reported them.
async fn association_versions(
    router: &MultiServiceRouter,
    case: &str,
    association_id: &str,
) -> Vec<Value> {
    let (status, parsed) = call(
        router,
        "ListAssociationVersions",
        &json!({"AssociationId": association_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListAssociationVersions, got {status} with \
         {parsed:?}"
    );
    parsed["AssociationVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// `UpdateAssociation` with `body`, which must have succeeded.
async fn update(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, "UpdateAssociation", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateAssociation, got {status} with {parsed:?}"
    );
    parsed["AssociationDescription"].clone()
}

/// One history of one association: the create it starts from, the updates appended to it, and
/// the members the version in force must report once they have all been applied.
struct SnapshotRow {
    case: &'static str,
    /// The members the create carries, beside the document it applies.
    created_with: Value,
    /// The members each update carries, beside the `AssociationId`, in order.
    updates: Vec<Value>,
    /// `(member, value)` pairs the NEWEST version must report, whichever call set them.
    in_force: Vec<(&'static str, Value)>,
}

/// Each row changes a DIFFERENT member and leaves the rest to the configuration in force, so
/// the rule is asserted over the class of updates rather than over the one a recording made.
fn snapshot_rows() -> Vec<SnapshotRow> {
    let targets = json!([{"Key": "tag:Environment", "Values": ["production"]}]);
    vec![
        SnapshotRow {
            case: "an update that changes only the schedule, leaving the targeting in force",
            created_with: json!({
                "Targets": targets,
                "MaxConcurrency": "10%",
                "ComplianceSeverity": "MEDIUM"
            }),
            updates: vec![json!({"ScheduleExpression": "rate(30 minutes)"})],
            in_force: vec![
                ("AssociationVersion", json!("2")),
                ("ScheduleExpression", json!("rate(30 minutes)")),
                ("Targets", targets.clone()),
                ("MaxConcurrency", json!("10%")),
                ("ComplianceSeverity", json!("MEDIUM")),
                ("ApplyOnlyAtCronInterval", json!(false)),
            ],
        },
        SnapshotRow {
            case: "an update that changes only the targeting, leaving the schedule in force",
            created_with: json!({
                "Targets": [{"Key": "InstanceIds", "Values": [NODE]}],
                "ScheduleExpression": "cron(0 2 ? * SUN *)",
                "ScheduleOffset": 3,
                "ApplyOnlyAtCronInterval": true
            }),
            updates: vec![json!({"Targets": targets, "ApplyOnlyAtCronInterval": true})],
            in_force: vec![
                ("AssociationVersion", json!("2")),
                ("Targets", targets.clone()),
                ("ScheduleExpression", json!("cron(0 2 ? * SUN *)")),
                ("ScheduleOffset", json!(3)),
            ],
        },
        SnapshotRow {
            case: "an update that changes only the error ceiling, leaving the name and the \
                   parameters of the document in force",
            created_with: json!({
                "Name": SHELL,
                "Parameters": {"commands": ["echo one"]},
                "AssociationName": "the-name",
                "Targets": targets
            }),
            updates: vec![json!({"MaxErrors": "2"})],
            in_force: vec![
                ("AssociationVersion", json!("2")),
                ("MaxErrors", json!("2")),
                ("Name", json!(SHELL)),
                ("Parameters", json!({"commands": ["echo one"]})),
                ("AssociationName", json!("the-name")),
                ("Targets", targets.clone()),
            ],
        },
        SnapshotRow {
            case: "three updates, each carrying one member, so the newest version carries all \
                   three beside what the create set",
            created_with: json!({"Targets": targets, "MaxConcurrency": "1"}),
            updates: vec![
                json!({"ScheduleExpression": "rate(1 hour)"}),
                json!({"ComplianceSeverity": "HIGH"}),
                json!({"SyncCompliance": "MANUAL", "Duration": 2}),
            ],
            in_force: vec![
                ("AssociationVersion", json!("4")),
                ("ScheduleExpression", json!("rate(1 hour)")),
                ("ComplianceSeverity", json!("HIGH")),
                ("SyncCompliance", json!("MANUAL")),
                ("Duration", json!(2)),
                ("MaxConcurrency", json!("1")),
                ("Targets", targets.clone()),
            ],
        },
    ]
}

/// RULE 1 + RULE 2 + RULE 3: the listing reports the newest version first; that entry is the
/// association's whole configuration — the members the updates carried over the ones the create
/// set — and it agrees with what a describe of the association reports; and every entry carries
/// `CalendarNames` as a list.
#[tokio::test]
async fn an_association_version_is_the_whole_configuration_and_the_newest_is_served_first() {
    for row in snapshot_rows() {
        let case = row.case;
        let (router, _engine) = service();
        let mut create_body = json!({"Name": AGENT});
        merge(&mut create_body, &row.created_with);
        let created = created(&router, case, &create_body).await;
        let association_id = created["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for members in &row.updates {
            let mut body = json!({"AssociationId": association_id});
            merge(&mut body, members);
            update(&router, case, &body).await;
        }

        let versions = association_versions(&router, case, &association_id).await;
        let held = row.updates.len() + 1;
        assert_eq!(
            versions.len(),
            held,
            "case {case:?}: expected the create's version and one per update, got {versions:?}"
        );
        // Newest first: the version numbers count DOWN from the version in force.
        let reported: Vec<String> = versions
            .iter()
            .map(|version| {
                version["AssociationVersion"]
                    .as_str()
                    .unwrap_or("<unset>")
                    .to_string()
            })
            .collect();
        let newest_first: Vec<String> = (1..=held).rev().map(|number| number.to_string()).collect();
        assert_eq!(
            reported, newest_first,
            "case {case:?}: expected the versions newest first, got {reported:?}"
        );

        // The first entry is the configuration in force: every member any call set is on it.
        let newest = &versions[0];
        for (member, value) in &row.in_force {
            assert_eq!(
                &newest[*member], value,
                "case {case:?}: expected the newest version's {member} to be {value}, got \
                 {newest:?}"
            );
        }
        // A read of the association reports the same configuration, so the two cannot
        // disagree about what the association is doing now.
        let read = described(&router, case, &json!({"AssociationId": association_id})).await;
        for (member, value) in &row.in_force {
            if *member == "AssociationVersion" {
                continue;
            }
            assert_eq!(
                &read[*member], value,
                "case {case:?}: expected the describe's {member} to be {value}, got {read:?}"
            );
        }
        // No Change Calendar gates any of these associations, so every version reports the
        // empty list rather than no list at all.
        for version in &versions {
            assert_eq!(
                version["CalendarNames"],
                json!([]),
                "case {case:?}: expected CalendarNames to be the empty list, got {version:?}"
            );
        }
        // The oldest entry is still the create's own version, carrying what the create set
        // and nothing a later update carried.
        let oldest = versions
            .last()
            .expect("a history holds at least the create's version");
        assert_eq!(
            oldest["AssociationVersion"],
            json!("1"),
            "case {case:?}: expected the last entry to be the version the create minted, got \
             {oldest:?}"
        );
    }
}

/// RULE 2 over the version an update is applied ON TOP OF: appending a version leaves the
/// versions before it exactly as they were, whatever the update changes.
#[tokio::test]
async fn appending_a_version_leaves_the_versions_before_it_as_they_were() {
    for row in snapshot_rows() {
        let case = row.case;
        let (router, _engine) = service();
        let mut create_body = json!({"Name": AGENT});
        merge(&mut create_body, &row.created_with);
        let created = created(&router, case, &create_body).await;
        let association_id = created["AssociationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let mut before = association_versions(&router, case, &association_id).await;
        for members in &row.updates {
            let mut body = json!({"AssociationId": association_id});
            merge(&mut body, members);
            update(&router, case, &body).await;
            let after = association_versions(&router, case, &association_id).await;
            // The newest entry is the version just appended; the entries after it are the
            // history as the listing reported it before this update.
            assert_eq!(
                after.len(),
                before.len() + 1,
                "case {case:?}: expected one more version after the update, got {after:?}"
            );
            assert_eq!(
                &after[1..],
                before.as_slice(),
                "case {case:?}: expected the versions before the appended one to be unchanged"
            );
            before = after;
        }
    }
}

/// RULE 1 for documents: `ListDocumentVersions` reports the newest version first, flagging the
/// default version wherever it sits in the history — for a document a caller created and for
/// one Systems Manager publishes.
#[tokio::test]
async fn a_document_version_listing_serves_the_newest_version_first() {
    use crate::parameter_store::{call as document_call, router};

    let router = router();
    // (case, the name, how many versions it holds, the version made default)
    let rows: Vec<(&str, &str, usize, usize)> = vec![
        ("one version, which is the default", "HistoryOneVersion", 1, 1),
        ("two versions, the default still the first", "HistoryTwoVersions", 2, 1),
        ("five versions, the default moved to the middle", "HistoryFiveVersions", 5, 3),
        ("five versions, the default moved to the newest", "HistoryDefaultNewest", 5, 5),
    ];
    for (case, name, held, default_version) in rows {
        let content = |body: &str| {
            format!(
                "{{\n  \"schemaVersion\": \"2.2\",\n  \"description\": \"{body}\",\n  \
                 \"mainSteps\": [\n    {{\n      \"action\": \"aws:runShellScript\",\n      \
                 \"name\": \"runShell\",\n      \"inputs\": {{ \"runCommand\": [\"echo hi\"] }}\n \
                 }}\n  ]\n}}"
            )
        };
        let (status, parsed) = document_call(
            &router,
            "CreateDocument",
            &json!({"Name": name, "Content": content("version 1")}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateDocument, got {status} with {parsed:?}"
        );
        for number in 2..=held {
            let (status, parsed) = document_call(
                &router,
                "UpdateDocument",
                &json!({"Name": name, "Content": content(&format!("version {number}"))}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from UpdateDocument, got {status} with \
                 {parsed:?}"
            );
        }
        if default_version != 1 {
            let (status, parsed) = document_call(
                &router,
                "UpdateDocumentDefaultVersion",
                &json!({"Name": name, "DocumentVersion": default_version.to_string()}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from UpdateDocumentDefaultVersion, got \
                 {status} with {parsed:?}"
            );
        }

        let (status, parsed) =
            document_call(&router, "ListDocumentVersions", &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListDocumentVersions, got {status} with \
             {parsed:?}"
        );
        let listed = parsed["DocumentVersions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let reported: Vec<String> = listed
            .iter()
            .map(|entry| {
                entry["DocumentVersion"]
                    .as_str()
                    .unwrap_or("<unset>")
                    .to_string()
            })
            .collect();
        let newest_first: Vec<String> = (1..=held).rev().map(|number| number.to_string()).collect();
        assert_eq!(
            reported, newest_first,
            "case {case:?}: expected the versions newest first, got {parsed:?}"
        );
        for entry in &listed {
            let number = entry["DocumentVersion"].as_str().unwrap_or_default();
            assert_eq!(
                entry["IsDefaultVersion"],
                json!(number == default_version.to_string()),
                "case {case:?}: expected version {number} to be flagged default exactly when \
                 it is version {default_version}, got {entry:?}"
            );
        }
    }

    // A document Systems Manager publishes lists its versions the same way, without any
    // create: `AWS-ConfigureAWSPackage` is an entry of the catalogue this issue did not cite
    // (`state::PUBLISHED_DOCUMENTS`).
    let (status, parsed) = document_call(
        &router,
        "ListDocumentVersions",
        &json!({"Name": "AWS-ConfigureAWSPackage"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a published document\": expected HTTP 200 from ListDocumentVersions, got \
         {status} with {parsed:?}"
    );
    let listed = parsed["DocumentVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !listed.is_empty(),
        "case \"a published document\": expected the catalogue entry to list its versions, got \
         {parsed:?}"
    );
    assert_eq!(
        listed[0]["IsDefaultVersion"],
        json!(true),
        "case \"a published document\": expected the newest version served first to be the \
         default one, got {parsed:?}"
    );

    // An entry the catalogue publishes SEVERAL revisions of lists one entry per revision,
    // newest first, each dated by its own publication: the version a read naming no selector
    // resolves to is the newest, and a client can order the history by date.
    let case = "a published document with several revisions";
    let (status, parsed) = document_call(
        &router,
        "ListDocumentVersions",
        &json!({"Name": "AWSSupport-TroubleshootSSH"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListDocumentVersions, got {status} with {parsed:?}"
    );
    let listed = parsed["DocumentVersions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.len() > 1,
        "case {case:?}: expected the revisions the catalogue publishes, got {parsed:?}"
    );
    let mut previous: Option<(u64, f64)> = None;
    for (index, entry) in listed.iter().enumerate() {
        let number: u64 = entry["DocumentVersion"]
            .as_str()
            .unwrap_or_default()
            .parse()
            .unwrap_or_default();
        let date = entry["CreatedDate"].as_f64().unwrap_or_default();
        assert_eq!(
            entry["IsDefaultVersion"],
            json!(index == 0),
            "case {case:?}: expected only the newest version to be the default, got {entry:?}"
        );
        assert_eq!(
            entry["Status"],
            json!("Active"),
            "case {case:?}: expected every published version to be Active, got {entry:?}"
        );
        if let Some((newer_number, newer_date)) = previous {
            assert!(
                number < newer_number && date < newer_date,
                "case {case:?}: expected version {number} to be older than {newer_number} and \
                 published before it, got {date} against {newer_date}"
            );
        }
        previous = Some((number, date));
    }
}

/// Merge the members of `extra` onto the object `body`.
fn merge(body: &mut Value, extra: &Value) {
    let (Some(target), Some(source)) = (body.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (member, value) in source {
        target.insert(member.clone(), value.clone());
    }
}
