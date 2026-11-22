// SPDX-License-Identifier: CC0-1.0

//! Which document version an association was pinned to, how a document an association call
//! cannot resolve is worded, and what an association reports once its execution is over.
//!
//! RULE 1 — an association is pinned to the document-version SELECTOR the call that wrote it
//! resolved against the document, which is the default version where the request named none
//! ("Can be a specific version or the default version",
//! `smithy_query(query="doc",shape="CreateAssociationRequest",member="DocumentVersion")`), and
//! every projection of the association reports it: the create's own answer,
//! `DescribeAssociation`, `ListAssociations` and an update's answer.
//!
//! RULE 2 — an association call that names a document this endpoint does not hold is refused
//! `InvalidDocument` ("The specified SSM document doesn't exist",
//! `smithy_query(query="shape",shape="InvalidDocument")`), worded as the fixed text
//! `Document does not exist: ` followed by the `Name` the request carried, verbatim.
//!
//! RULE 3 — an association's execution is over once its targets have been evaluated and the
//! document applied on the nodes they selected, and the association then reports that result:
//! `Status`/`DetailedStatus` `Success`, a count of targets by status over the nodes the run
//! reached — empty for a run that reached none — and the instant the run finished as both of
//! its execution dates. A `DeleteAssociation` takes the association away, so a read of it
//! afterwards is refused `AssociationDoesNotExist` naming the id the request carried.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"); nothing reaches into the state struct.
//!
//! The tables' rows differ along what each rule generalises over: the version selector's
//! form, which document is associated, which operation projects the association, the spelling
//! of the document name a refusal echoes, and how many registered nodes the run reached.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, create, created, described, error_code, service, AGENT, NODE, REGION, SHELL,
};

/// A document this suite creates, so a case can name a version NUMBER of a document a caller
/// wrote rather than only the labels a published document carries.
const WRITTEN: &str = "AssociationPinnedDocument";

/// How long a case waits for an association's execution to be owed its result before failing.
/// Far longer than the transient window the result is owed after, so a loaded host cannot
/// fail a case that is about WHETHER the association settles.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A JSON `Command` document, parameterised by the description its content declares so two
/// versions of it differ in content and in nothing else.
fn command_json(description: &str) -> String {
    format!(
        r#"{{
  "schemaVersion": "2.2",
  "description": "{description}",
  "mainSteps": [
    {{
      "action": "aws:runShellScript",
      "name": "runShell",
      "inputs": {{ "runCommand": ["echo hi"] }}
    }}
  ]
}}"#
    )
}

/// A string member of a body, or `<unset>` so a failing assertion names what was there.
fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or("<unset>").to_string()
}

/// The refusal message of a failure body, under either spelling of the member.
fn message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// Write [`WRITTEN`] with a second version appended, so its versions are `1` and `2` and its
/// default version is `1`.
async fn write_two_versions(router: &MultiServiceRouter) {
    let (status, response) = call(
        router,
        "CreateDocument",
        &json!({ "Name": WRITTEN, "Content": command_json("version one") }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the document the version table associates must be created, got {status} {response}"
    );
    let (status, response) = call(
        router,
        "UpdateDocument",
        &json!({
            "Name": WRITTEN,
            "Content": command_json("version two"),
            "DocumentVersion": "$LATEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a second version of it must be appended, got {status} {response}"
    );
}

/// The association `id` as `ListAssociations` reports it.
async fn listed(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, response) = call(
        router,
        "ListAssociations",
        &json!({ "AssociationFilterList": [{ "key": "AssociationId", "value": id }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listing must be served, got {status} with {response}"
    );
    response["Associations"]
        .as_array()
        .and_then(|associations| associations.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: the listing must report the association, got {response}"))
}

/// One row of the version table: the selector a call carries, and the selector the
/// association is then pinned to.
struct Pinned {
    case: &'static str,
    /// The document the association applies.
    document: &'static str,
    /// The `DocumentVersion` the call carries, or `None` for a call that names none.
    selector: Option<&'static str>,
    /// What every projection of the association reports for `DocumentVersion`.
    pinned: &'static str,
}

/// RULE 1: the version selector a create resolved is what the create, a read and a listing all
/// report.
#[tokio::test]
async fn an_association_reports_the_document_version_its_create_resolved() {
    let (router, _engine) = service();
    write_two_versions(&router).await;

    let rows = [
        Pinned {
            case: "a published document with no selector",
            document: AGENT,
            selector: None,
            pinned: "$DEFAULT",
        },
        Pinned {
            case: "a written document with no selector",
            document: WRITTEN,
            selector: None,
            pinned: "$DEFAULT",
        },
        Pinned {
            case: "the default-version label",
            document: WRITTEN,
            selector: Some("$DEFAULT"),
            pinned: "$DEFAULT",
        },
        Pinned {
            case: "the latest-version label",
            document: WRITTEN,
            selector: Some("$LATEST"),
            pinned: "$LATEST",
        },
        Pinned {
            case: "the first version by number",
            document: WRITTEN,
            selector: Some("1"),
            pinned: "1",
        },
        Pinned {
            case: "a later version by number",
            document: WRITTEN,
            selector: Some("2"),
            pinned: "2",
        },
    ];

    for row in &rows {
        let mut request = json!({
            "Name": row.document,
            "AssociationName": format!("pinned-{}", row.case.replace(' ', "-")),
        });
        if let Some(selector) = row.selector {
            request["DocumentVersion"] = json!(selector);
        }
        let created = created(&router, row.case, &request).await;
        assert_eq!(
            text(&created, "DocumentVersion"),
            row.pinned,
            "case {:?}: expected the create to report the version it resolved, got {created}",
            row.case
        );
        let id = text(&created, "AssociationId");

        let read = described(&router, row.case, &json!({ "AssociationId": id })).await;
        assert_eq!(
            text(&read, "DocumentVersion"),
            row.pinned,
            "case {:?}: expected the read to report the version the create resolved, got {read}",
            row.case
        );
        let entry = listed(&router, row.case, &id).await;
        assert_eq!(
            text(&entry, "DocumentVersion"),
            row.pinned,
            "case {:?}: expected the listing to report the same version, got {entry}",
            row.case
        );
    }
}

/// RULE 1 for an update: the version an update resolved is what the association is pinned to
/// from then on, whether the update named a selector or left it out.
#[tokio::test]
async fn an_update_reports_the_document_version_it_resolved() {
    let (router, _engine) = service();
    write_two_versions(&router).await;

    // (case, what the update carries beside the id, the selector the association then holds)
    let rows: &[(&str, Value, &str)] = &[
        (
            "an update naming no version",
            json!({}),
            "$DEFAULT",
        ),
        (
            "an update naming a version by number",
            json!({ "DocumentVersion": "2" }),
            "2",
        ),
        (
            "an update naming the latest-version label",
            json!({ "DocumentVersion": "$LATEST" }),
            "$LATEST",
        ),
        (
            "an update naming another document and no version",
            json!({ "Name": AGENT }),
            "$DEFAULT",
        ),
    ];

    for (case, applied, pinned) in rows {
        let created = created(
            &router,
            case,
            &json!({
                "Name": WRITTEN,
                "DocumentVersion": "1",
                "AssociationName": format!("updated-{}", case.replace(' ', "-")),
            }),
        )
        .await;
        let id = text(&created, "AssociationId");

        let mut request = applied.clone();
        request["AssociationId"] = json!(id);
        let (status, response) = call(&router, "UpdateAssociation", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the update must be served, got {status} with {response}"
        );
        let updated = response["AssociationDescription"].clone();
        assert_eq!(
            text(&updated, "DocumentVersion"),
            *pinned,
            "case {case:?}: expected the update to report the version it resolved, got {updated}"
        );
        let read = described(&router, case, &json!({ "AssociationId": id })).await;
        assert_eq!(
            text(&read, "DocumentVersion"),
            *pinned,
            "case {case:?}: expected a read after the update to report that version, got {read}"
        );
    }
}

/// RULE 2: the refusal names the document the request asked for, after the fixed text.
#[tokio::test]
async fn a_document_an_association_call_cannot_resolve_is_named_by_the_refusal() {
    let (router, _engine) = service();

    // One row per spelling of a name that resolves to no document: a bare name nothing was
    // created under, one carrying an Amazon prefix, a name differing from a published
    // document's, and the full ARN spelling `Name` also admits.
    let names: Vec<String> = vec![
        "no-such-document".to_string(),
        "AWS-NoSuchRunbook".to_string(),
        format!("{AGENT}-unwritten"),
        format!(
            "arn:aws:ssm:{REGION}:{}:document/NeverWritten",
            engine::emulator_account_id()
        ),
    ];

    for name in &names {
        let expected = format!("Document does not exist: {name}");
        let (status, response) = create(&router, &json!({ "Name": name })).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidDocument"),
            "case {name:?}: a create naming no document is refused InvalidDocument, got \
             {status} with {response}"
        );
        assert_eq!(
            message(&response),
            expected,
            "case {name:?}: expected the refusal to name the document asked for, got {response}"
        );

        // An UPDATE of a sound association naming the same document is refused in the same
        // words: the refusal is about the document, not about which call named it.
        let association = created(
            &router,
            name,
            &json!({ "Name": AGENT, "AssociationName": "refusal-wording" }),
        )
        .await;
        let (status, response) = call(
            &router,
            "UpdateAssociation",
            &json!({
                "AssociationId": text(&association, "AssociationId"),
                "Name": name,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidDocument"),
            "case {name:?}: an update naming no document is refused InvalidDocument, got \
             {status} with {response}"
        );
        assert_eq!(
            message(&response),
            expected,
            "case {name:?}: expected the update's refusal to name the document asked for, got \
             {response}"
        );
    }
}

/// RULE 3: the association reports the result of its execution, and reports nothing at all
/// once it has been deleted.
#[tokio::test]
async fn an_association_reports_its_execution_and_then_nothing_once_deleted() {
    let (router, _engine) = service();

    // (case, the create) — one row per targeting form that selects no registered node, since
    // no node is registered here: no targets at all, a tag, a node id, and every node of the
    // Region.
    let rows: &[(&str, Value)] = &[
        ("no targets at all", json!({ "Name": AGENT })),
        (
            "a tag no node carries",
            json!({
                "Name": AGENT,
                "Targets": [{ "Key": "tag:Env", "Values": ["prod"] }],
            }),
        ),
        (
            "a node id nothing is registered under",
            json!({
                "Name": SHELL,
                "Parameters": { "commands": ["echo hi"] },
                "Targets": [{ "Key": "InstanceIds", "Values": [NODE] }],
            }),
        ),
        (
            "every node of the Region",
            json!({
                "Name": AGENT,
                "Targets": [{ "Key": "InstanceIds", "Values": ["*"] }],
            }),
        ),
    ];

    for (case, request) in rows {
        let mut request = request.clone();
        request["AssociationName"] = json!(format!("settled-{}", case.replace(' ', "-")));
        let created = created(&router, case, &request).await;
        let id = text(&created, "AssociationId");

        // Looked at again until the result the clock owes is due, the way a client waits for
        // an association to converge.
        let deadline = Instant::now() + SETTLE_DEADLINE;
        let settled = loop {
            let read = described(&router, case, &json!({ "AssociationId": id })).await;
            if read["Overview"]["Status"] == json!("Success") {
                break read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the association to settle with nothing but the clock \
                 running, got {read}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            settled["Overview"]["DetailedStatus"],
            json!("Success"),
            "case {case:?}: expected the detail of a finished run, got {settled}"
        );
        assert_eq!(
            settled["Overview"]["AssociationStatusAggregatedCount"],
            json!({}),
            "case {case:?}: expected a count of targets by status over the no node the run \
             reached, got {settled}"
        );
        assert!(
            settled["LastExecutionDate"].is_number()
                && settled["LastSuccessfulExecutionDate"] == settled["LastExecutionDate"],
            "case {case:?}: expected the finished run's instant as both execution dates, got \
             {settled}"
        );
        assert_eq!(
            text(&settled, "DocumentVersion"),
            "$DEFAULT",
            "case {case:?}: a settled association still reports the version it was pinned to, \
             got {settled}"
        );

        // Deleted, the association is readable no more: the read is refused naming the id the
        // request carried.
        let (status, response) =
            call(&router, "DeleteAssociation", &json!({ "AssociationId": id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must be served, got {status} with {response}"
        );
        let (status, response) = call(
            &router,
            "DescribeAssociation",
            &json!({ "AssociationId": id }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "AssociationDoesNotExist"),
            "case {case:?}: a read of a deleted association is refused, got {status} with \
             {response}"
        );
        assert_eq!(
            message(&response),
            format!("Association {id} does not exist."),
            "case {case:?}: expected the refusal to name the association read, got {response}"
        );
    }
}
