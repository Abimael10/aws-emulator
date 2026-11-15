// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ROUND TRIP of a delivery destination policy: what
//! `GetDeliveryDestinationPolicy` reports back for the document
//! `PutDeliveryDestinationPolicy` assigned, and what `DeleteDeliveryDestinationPolicy`
//! answers for a destination that holds no policy at all.
//!
//! The rules under test:
//!
//! * a read reports the document in the form the service STORES it in, and that form is a
//!   FIXED POINT of the one rendering this service's reads apply — so a document already in
//!   that form (its tokens unspaced, every element the policy language types as "String or
//!   Array of strings" written as one value or as a list of several, every account principal
//!   named by an ARN) comes back from the read BYTE FOR BYTE whatever its statements say, and
//!   a client that assigns back what it read reads the same bytes again; the assigning call
//!   itself reports the document the request wrote. What the policy SAYS is the caller's: its
//!   statements, their order, their `Sid`s and every byte inside a string literal survive the
//!   round trip.
//! * the resource the delete REMOVES is that policy rather than the destination it is
//!   assigned to, so a destination holding none has nothing for the delete to remove and the
//!   call is refused as a missing resource — with the sentence naming the missing POLICY,
//!   which is not the sentence a `deliveryDestinationName` the account holds no destination
//!   under is refused with — however often it is repeated, while a later assignment on the
//!   same destination takes effect and can be deleted again.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so what is
//! asserted here is what a client reads off the wire, and a response that is not computed
//! from the shared store cannot pass.
//!
//! The table spans what the rules generalise over rather than one document: statements
//! written with an account root ARN, a role ARN, a principal list and the bare wildcard;
//! actions and resources written as one value, as a list of several and as the patterns the
//! policy language admits; one statement and two; a nested `Condition`; a member order and a
//! non-ASCII, escape-carrying `Sid` of the caller's own; documents the caller's renderer
//! indented or spaced, which are the same document written differently; and a document
//! written in neither of the two spellings the stored form collapses — an account principal
//! named by its id alone, and one-value `Action` and `Resource` lists — whose stored form is
//! therefore not the text submitted, and is still a fixed point.
//!
//! `tests/all/delivery_destination_policies.rs` is the assignment side of this state and
//! `tests/all/delivery_destination_policy_removal.rs` the deletion side; what is pinned here
//! is the fidelity of the round trip itself.

use serde_json::{json, Value};

use crate::harness::{Logs, REGION};

/// The operations under test, plus the destination operation each case stands a destination
/// up with.
const PUT_POLICY: &str = "PutDeliveryDestinationPolicy";
const GET_POLICY: &str = "GetDeliveryDestinationPolicy";
const DELETE_POLICY: &str = "DeleteDeliveryDestinationPolicy";
const PUT_DESTINATION: &str = "PutDeliveryDestination";

/// The refusal both missing resources of these operations are modelled as, and the two
/// sentences CloudWatch Logs words them with: the destination the request names, and the
/// policy the delete addresses.
const NOT_FOUND: &str = "ResourceNotFoundException";
const NO_POLICY_MESSAGE: &str =
    "Delivery Destination Policy does not exist for this Delivery Destination.";
const NO_DESTINATION_MESSAGE: &str =
    "Requested Delivery Destination does not exist in this account.";

/// The `DeliveryDestinationType` of a trace destination, the one kind created from its type
/// alone. Source: the model's `DeliveryDestinationType` enum.
const TRACE: &str = "XRAY";

/// The account the cross-account grants below are made to, named the three ways an account
/// principal is named: its root ARN, the ARN of one of its roles, and its id alone.
const OTHER_ACCOUNT_ROOT: &str = "arn:aws:iam::210987654321:root";
const OTHER_ACCOUNT_ROLE: &str = "arn:aws:iam::210987654321:role/CrossAccountDelivery";
const OTHER_ACCOUNT_ID: &str = "210987654321";

/// Every delivery destination this account holds in this Region, and ONE of them, named by
/// the ARN this service builds for one — the resources a delivery destination policy grants
/// access to.
fn every_destination() -> String {
    ::engine::emulator_arn("logs", REGION, "delivery-destination:*")
}

fn one_destination() -> String {
    ::engine::emulator_arn("logs", REGION, "delivery-destination:round-trip-0")
}

/// One policy document a case assigns: how the request writes it, and what every later read
/// of it reports.
#[derive(Debug)]
struct Row {
    /// What this row varies, named in every assertion.
    label: &'static str,
    /// The `deliveryDestinationPolicy` the request carries.
    document: String,
    /// The form the service stores that document in, which is what every later read reports.
    /// For a document already in that form it is the document itself.
    stored: String,
}

/// One statement granting an account one delivery action on one destination, written with no
/// whitespace between its tokens — the form the recorded cross-account grant is written in.
fn compact_single_statement() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowCreateDelivery","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":"logs:CreateDelivery","Resource":"{resource}"}}]}}"#,
        resource = one_destination(),
    )
}

/// The SAME document, indented the way a caller's renderer indents one: the whitespace
/// between JSON tokens is not part of what the document says.
fn indented_single_statement() -> String {
    format!(
        r#"{{
  "Version": "2012-10-17",
  "Statement": [
    {{
      "Sid": "AllowCreateDelivery",
      "Effect": "Allow",
      "Principal": {{
        "AWS": "{OTHER_ACCOUNT_ROOT}"
      }},
      "Action": "logs:CreateDelivery",
      "Resource": "{resource}"
    }}
  ]
}}"#,
        resource = one_destination(),
    )
}

/// A grant written with the wildcard principal, an action PATTERN and the wildcard resource,
/// with no whitespace between its tokens.
fn compact_wildcard_statement() -> String {
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"logs:Get*","Resource":"*"}]}"#
        .to_string()
}

/// The SAME document, with the one space a renderer writes after every separator.
fn spaced_wildcard_statement() -> String {
    r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": "*", "Action": "logs:Get*", "Resource": "*"}]}"#
        .to_string()
}

/// The documents every case below assigns, each varying one thing the rule generalises over.
fn rows() -> Vec<Row> {
    let every = every_destination();
    let one = one_destination();
    vec![
        Row {
            label: "an account root ARN principal with a scalar Action and a scalar Resource",
            document: compact_single_statement(),
            stored: compact_single_statement(),
        },
        Row {
            label: "a principal list of a role and a root ARN, with Action and Resource lists",
            document: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowDeliveryActions","Effect":"Allow","Principal":{{"AWS":["{OTHER_ACCOUNT_ROLE}","{OTHER_ACCOUNT_ROOT}"]}},"Action":["logs:CreateDelivery","logs:PutDeliverySource"],"Resource":["{every}","{one}"]}}]}}"#
            ),
            stored: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowDeliveryActions","Effect":"Allow","Principal":{{"AWS":["{OTHER_ACCOUNT_ROLE}","{OTHER_ACCOUNT_ROOT}"]}},"Action":["logs:CreateDelivery","logs:PutDeliverySource"],"Resource":["{every}","{one}"]}}]}}"#
            ),
        },
        Row {
            label: "the wildcard principal with an action pattern and the wildcard resource",
            document: compact_wildcard_statement(),
            stored: compact_wildcard_statement(),
        },
        Row {
            label: "the two documented statements, the second carrying a nested Condition",
            document: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowLogDelivery","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":["logs:CreateDelivery","logs:GetDelivery"],"Resource":"{every}"}},{{"Sid":"AllowDeliveryToDestination","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":"logs:CreateDelivery","Resource":"{one}","Condition":{{"StringEquals":{{"aws:SourceAccount":["{OTHER_ACCOUNT_ID}","123456789012"]}}}}}}]}}"#
            ),
            stored: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowLogDelivery","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":["logs:CreateDelivery","logs:GetDelivery"],"Resource":"{every}"}},{{"Sid":"AllowDeliveryToDestination","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":"logs:CreateDelivery","Resource":"{one}","Condition":{{"StringEquals":{{"aws:SourceAccount":["{OTHER_ACCOUNT_ID}","123456789012"]}}}}}}]}}"#
            ),
        },
        Row {
            label: "a member order of the caller's own and a non-ASCII, escaped Sid",
            document: format!(
                r#"{{"Statement":[{{"Effect":"Allow","Action":"logs:CreateDelivery","Resource":"{every}","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Sid":"lieferung-\u00fcber-gr\u00f6\u00dfe-\"zitat\""}}],"Version":"2012-10-17"}}"#
            ),
            stored: format!(
                r#"{{"Statement":[{{"Effect":"Allow","Action":"logs:CreateDelivery","Resource":"{every}","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Sid":"lieferung-\u00fcber-gr\u00f6\u00dfe-\"zitat\""}}],"Version":"2012-10-17"}}"#
            ),
        },
        Row {
            label: "the same single statement, indented by the caller's renderer",
            document: indented_single_statement(),
            stored: compact_single_statement(),
        },
        Row {
            label: "the same wildcard statement, spaced after every separator",
            document: spaced_wildcard_statement(),
            stored: compact_wildcard_statement(),
        },
        Row {
            label: "an account id principal with one-value Action and Resource lists",
            document: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowCreateDelivery","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ID}"}},"Action":["logs:CreateDelivery"],"Resource":["{every}"]}}]}}"#
            ),
            stored: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"AllowCreateDelivery","Effect":"Allow","Principal":{{"AWS":"{OTHER_ACCOUNT_ROOT}"}},"Action":"logs:CreateDelivery","Resource":"{every}"}}]}}"#
            ),
        },
    ]
}

/// Create one delivery destination to assign a policy to, varying the kind of resource it
/// represents across the table so the round trip is asserted on more than one kind.
async fn create_destination(logs: &Logs, name: &str, index: usize) {
    let body = match index % 3 {
        0 => json!({
            "name": name,
            "deliveryDestinationConfiguration": {
                "destinationResourceArn": format!("arn:aws:s3:::vended-logs-{index}"),
            },
        }),
        1 => json!({
            "name": name,
            "deliveryDestinationConfiguration": {
                "destinationResourceArn":
                    format!("arn:aws:firehose:{REGION}:123456789012:deliverystream/vended-{index}"),
            },
        }),
        _ => json!({"name": name, "deliveryDestinationType": TRACE}),
    };
    logs.ok(PUT_DESTINATION, &body).await;
}

/// Assign one policy over the wire, returning the `policy.deliveryDestinationPolicy` the
/// assigning call itself reports.
async fn assign_policy(logs: &Logs, name: &str, document: &str) -> Option<String> {
    let body = json!({"deliveryDestinationName": name, "deliveryDestinationPolicy": document});
    reported_policy(PUT_POLICY, &body, &logs.ok(PUT_POLICY, &body).await)
}

/// The `policy.deliveryDestinationPolicy` the service reports for `name`, or `None` when the
/// reported `policy` structure carries none — which is what a destination holding no policy
/// reports. The structure itself is asserted to be there for every read.
async fn read_policy(logs: &Logs, name: &str) -> Option<String> {
    let body = json!({"deliveryDestinationName": name});
    reported_policy(GET_POLICY, &body, &logs.ok(GET_POLICY, &body).await)
}

/// The document a response's `policy` structure carries, having asserted that the structure
/// is there and is the modelled structure.
fn reported_policy(operation: &str, body: &Value, response: &Value) -> Option<String> {
    let policy = response.get("policy").unwrap_or_else(|| {
        panic!("{operation} {body}: the response carries the policy structure, got {response}")
    });
    assert!(
        policy.is_object(),
        "{operation} {body}: policy is the modelled structure, got {response}"
    );
    Some(
        policy
            .get("deliveryDestinationPolicy")?
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "{operation} {body}: a reported policy carries deliveryDestinationPolicy, \
                     got {response}"
                )
            })
            .to_string(),
    )
}

/// Delete one destination's policy over the wire, asserting the empty success body this
/// operation models (its output shape is `Unit`).
async fn delete_policy(logs: &Logs, case: &str, name: &str) {
    let body = json!({"deliveryDestinationName": name});
    let response = logs.ok(DELETE_POLICY, &body).await;
    assert!(
        response.is_null() || response.as_object().is_some_and(serde_json::Map::is_empty),
        "case {case:?}: {DELETE_POLICY} reports no members, got {response}"
    );
}

/// Delete one destination's policy over the wire where there is none to delete, asserting the
/// refusal and the sentence `expected_message` — which names the missing resource the call
/// found, the policy or the destination.
async fn delete_refused(logs: &Logs, case: &str, name: &str, expected_message: &str) {
    let body = json!({"deliveryDestinationName": name});
    let (code, message) = logs.refusal(DELETE_POLICY, &body).await;
    assert_eq!(
        code, NOT_FOUND,
        "case {case:?}: a delete with nothing to delete is refused with {NOT_FOUND}, got {code}"
    );
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected the refusal {expected_message:?}, got {message:?}"
    );
}

/// One document parsed, so what it SAYS can be compared across the round trip.
fn parsed(case: &str, document: &str) -> Value {
    serde_json::from_str(document)
        .unwrap_or_else(|error| panic!("case {case:?}: {document:?} is JSON, got {error}"))
}

/// The `Sid` of every statement a document declares, in the order they are written — what the
/// policy says about the grants it carries, which no round trip may reorder or drop.
fn statement_sids(case: &str, document: &Value) -> Vec<Option<String>> {
    document
        .get("Statement")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: a policy declares a Statement list, got {document}"))
        .iter()
        .map(|statement| {
            statement
                .get("Sid")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// A document assigned to a destination is read back in the form the service stores it in,
/// that form is a fixed point of the rendering, and what the document says survives the trip.
/// Every row lives in ONE store, so an assignment that reached past the destination it names
/// cannot pass.
#[tokio::test]
async fn an_assigned_policy_reads_back_in_the_form_it_is_stored_in() {
    let logs = Logs::new();
    let rows = rows();
    let names: Vec<String> = (0..rows.len())
        .map(|index| format!("round-trip-{index}"))
        .collect();

    for (index, row) in rows.iter().enumerate() {
        let name = &names[index];
        create_destination(&logs, name, index).await;

        // "The contents of the policy that you just created": the assigning call reports the
        // document the REQUEST wrote.
        let assigned = assign_policy(&logs, name, &row.document).await;
        assert_eq!(
            assigned.as_deref(),
            Some(row.document.as_str()),
            "case {:?}: expected the assignment to report the document it was given, got \
             {assigned:?}",
            row.label
        );

        // Every later read reports the stored form, byte for byte.
        let read_back = read_policy(&logs, name).await;
        assert_eq!(
            read_back.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected the stored document {:?}, got {read_back:?}",
            row.label,
            row.stored
        );

        // The stored form is a FIXED POINT: assigning back what the read reported reports the
        // same bytes again, so a client that re-applies what it read sees no drift.
        let reassigned = assign_policy(&logs, name, &row.stored).await;
        assert_eq!(
            reassigned.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected re-assigning the stored document to report it unchanged, got \
             {reassigned:?}",
            row.label
        );
        let again = read_policy(&logs, name).await;
        assert_eq!(
            again.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected the stored document after re-assigning it, got {again:?}",
            row.label
        );

        // And what the document SAYS is the caller's: the same policy, with the same
        // statements in the same order under the same `Sid`s — read off the document the
        // SERVICE answered with rather than off the table, so a read that answered something
        // else cannot pass this.
        let submitted = parsed(row.label, &row.document);
        let answered = again.unwrap_or_else(|| {
            panic!(
                "case {:?}: the read reports the document it stored",
                row.label
            )
        });
        let reported = parsed(row.label, &answered);
        assert_eq!(
            reported.get("Version"),
            submitted.get("Version"),
            "case {:?}: expected the submitted Version, got {reported}",
            row.label
        );
        assert_eq!(
            statement_sids(row.label, &reported),
            statement_sids(row.label, &submitted),
            "case {:?}: expected the submitted statements, got {reported}",
            row.label
        );
    }

    // Every destination still reports its OWN document once all of them are assigned: a
    // policy belongs to the destination it was assigned to.
    for (index, row) in rows.iter().enumerate() {
        let reported = read_policy(&logs, &names[index]).await;
        assert_eq!(
            reported.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected the destination to keep its own document, got {reported:?}",
            row.label
        );
    }
}

/// The resource the delete removes is the POLICY: a destination holding one is left holding
/// none, a destination holding none is refused with the sentence naming the missing policy
/// however often the delete is repeated, a later assignment on the same destination takes
/// effect and can be deleted again, and a name the account holds no destination under is
/// refused with the OTHER sentence — the one naming the missing destination.
#[tokio::test]
async fn a_delete_with_no_policy_to_remove_is_refused() {
    let logs = Logs::new();
    let rows = rows();

    // One destination that never had a policy assigned, beside the destinations the rows
    // assign to, so the two states are held in the same store.
    let unassigned = "never-assigned";
    create_destination(&logs, unassigned, 2).await;
    let reported = read_policy(&logs, unassigned).await;
    assert_eq!(
        reported, None,
        "case \"a destination no policy was assigned to\": expected no policy, got {reported:?}"
    );
    delete_refused(
        &logs,
        "a destination no policy was assigned to",
        unassigned,
        NO_POLICY_MESSAGE,
    )
    .await;

    for (index, row) in rows.iter().enumerate() {
        let name = format!("removal-{index}");
        create_destination(&logs, &name, index).await;
        let assigned = assign_policy(&logs, &name, &row.document).await;
        assert_eq!(
            assigned.as_deref(),
            Some(row.document.as_str()),
            "case {:?}: expected the assignment to report the document it was given, got \
             {assigned:?}",
            row.label
        );

        // The first delete takes the policy off; the destination then holds none.
        delete_policy(&logs, row.label, &name).await;
        let reported = read_policy(&logs, &name).await;
        assert_eq!(
            reported, None,
            "case {:?}: expected no policy after the delete, got {reported:?}",
            row.label
        );

        // Every delete after it finds nothing to remove and is refused, as often as it is
        // repeated, and leaves the destination holding none.
        for round in 1..=3 {
            delete_refused(
                &logs,
                &format!("{:?} repeated {round}", row.label),
                &name,
                NO_POLICY_MESSAGE,
            )
            .await;
            let reported = read_policy(&logs, &name).await;
            assert_eq!(
                reported, None,
                "case {:?}: expected no policy after a refused delete, got {reported:?}",
                row.label
            );
        }

        // The destination is still there to assign to: a later assignment takes effect, reads
        // back in the stored form, and can be deleted again.
        let reassigned = assign_policy(&logs, &name, &row.stored).await;
        assert_eq!(
            reassigned.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected the re-assignment to report its document, got {reassigned:?}",
            row.label
        );
        let reported = read_policy(&logs, &name).await;
        assert_eq!(
            reported.as_deref(),
            Some(row.stored.as_str()),
            "case {:?}: expected the re-assigned document, got {reported:?}",
            row.label
        );
        delete_policy(&logs, row.label, &name).await;
        let reported = read_policy(&logs, &name).await;
        assert_eq!(
            reported, None,
            "case {:?}: expected no policy after deleting the re-assigned one, got {reported:?}",
            row.label
        );
    }

    // The policy is deleted off a delivery DESTINATION, so a name the account holds none
    // under is a different missing resource, worded differently — including the names held by
    // a destination that has had its policy deleted, spelled just differently enough to name
    // no destination at all. Each satisfies `DeliveryDestinationName` (1..60, `^[\w-]*$`).
    for (case, name) in [
        ("a name held by no destination", "removal-0x".to_string()),
        ("the held name upper-cased", "REMOVAL-0".to_string()),
        ("dashes written as underscores", "removal_0".to_string()),
        ("a name at the minimum length", "r".to_string()),
        ("a name at the maximum length", "r".repeat(60)),
    ] {
        delete_refused(&logs, case, &name, NO_DESTINATION_MESSAGE).await;
    }
}
