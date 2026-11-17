// SPDX-License-Identifier: CC0-1.0

//! The terminal `SUCCESS` a Cloud Control API resource request reports, as the barrier the
//! OWNING service's own state has already passed.
//!
//! The rule every case here holds the service to: **a resource operation of this facade is
//! applied in the state of the service that OWNS the request's `TypeName`, and the request
//! reaches the terminal status the model documents as "The resource operation has
//! successfully completed" (`SUCCESS`) only once that service itself holds the resource the
//! operation asked for — so at the instant a poll FIRST observes `SUCCESS`, with no further
//! waiting, the owning service's own API reports a created resource in its ready state and
//! serves it, and reports a deleted one as absent in its own terms; where that service will
//! not do the work, the request reaches `FAILED` carrying the handler's verdict rather than
//! reporting a success nothing performed.**
//!
//! The rows differ along what the rule generalises over: which service owns the type and
//! over which wire it answers, the form of the type's primary identifier (a name the create
//! chose, a url the owning service assigned, a hierarchical name), the shape of the resource
//! asked for (a simple and a composite primary key, a capacity mode, a tag set), and what
//! "the owning service holds it" means for the type — an item round-trip through DynamoDB's
//! data plane, a message round-trip through a queue, the value a parameter read reports.
//!
//! `dynamodb_table_read_through`, `sqs_queue_read_through` and `ssm_parameter_read_through`
//! hold the read-through half of the same rule per owning service, and `refusal_wording` holds
//! the SENTENCE each owning handler words its already-exists verdict with; what is asserted
//! here is the BARRIER the terminal status stands for, across the owning services together.

use serde_json::{json, Map, Value};

use super::emulator::{
    code, message, progress, text, Emulator, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PENDING,
    STATUS_SUCCESS,
};
use super::sqs_queue_read_through::{queue_url, QUEUE};

/// The type whose resources live in the Amazon DynamoDB service's own store.
const TABLE: &str = "AWS::DynamoDB::Table";

/// The type whose resources live in the AWS Systems Manager service's own store.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// The error code the synchronous resource operations refuse a resource that is not there
/// with, as the model declares it.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The `HandlerErrorCode` a create whose resource the owning service already holds fails
/// with.
const HANDLER_ALREADY_EXISTS: &str = "AlreadyExists";

/// What every data-plane round-trip below writes, and what a read of it must report back.
const WRITTEN: &str = "written-right-after-success";

/// What the owning service holds the row's resource in, and how that service is asked about
/// it: the service of this emulator that owns the row's resource type, which is where the
/// effect of a Cloud Control API resource operation has to land.
#[derive(Clone, Copy, Debug)]
enum Owner {
    /// Amazon DynamoDB, which owns `AWS::DynamoDB::Table`.
    Dynamodb,
    /// Amazon SQS, which owns `AWS::SQS::Queue`.
    Sqs,
    /// AWS Systems Manager, which owns `AWS::SSM::Parameter`.
    Ssm,
}

impl Owner {
    /// The `TypeName` whose resources this service owns.
    fn type_name(self) -> &'static str {
        match self {
            Self::Dynamodb => TABLE,
            Self::Sqs => QUEUE,
            Self::Ssm => PARAMETER,
        }
    }

    /// One emulator with this service mounted beside Cloud Control API, so a resource
    /// operation of the facade has the owning service's own store to land in.
    fn emulator(self) -> Emulator {
        match self {
            Self::Dynamodb => Emulator::with_dynamodb(),
            Self::Sqs => Emulator::with_sqs(),
            Self::Ssm => Emulator::with_ssm(),
        }
    }

    /// Assert the OWNING service already holds and serves the resource `identifier` names,
    /// in its own terms: it reports it in its ready state, and its own plane round-trips
    /// through it.
    ///
    /// Nothing here waits: every call is made at the instant the caller observed the
    /// request's terminal status, which is what makes the status a barrier rather than an
    /// announcement.
    async fn assert_serves(
        self,
        emulator: &Emulator,
        case: &str,
        identifier: &str,
        desired: &Value,
    ) {
        match self {
            Self::Dynamodb => {
                let (status, body) = emulator
                    .dynamodb("DescribeTable", json!({ "TableName": identifier }))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: dynamodb:DescribeTable expected HTTP 200, got {status} \
                     with body {body:?}"
                );
                let described = &body["Table"];
                assert_eq!(
                    described["TableStatus"],
                    json!("ACTIVE"),
                    "case {case:?}: expected the owning service to report a settled table, \
                     got {described:?}"
                );
                assert_eq!(
                    described["KeySchema"], desired["KeySchema"],
                    "case {case:?}: expected the primary key the create declared, got \
                     {described:?}"
                );
                // The data plane serves it, which a table nothing provisioned would not.
                let item = key_item(desired);
                emulator
                    .dynamodb_ok(
                        "PutItem",
                        json!({ "TableName": identifier, "Item": item.clone() }),
                    )
                    .await;
                let read = emulator
                    .dynamodb_ok(
                        "GetItem",
                        json!({
                            "TableName": identifier,
                            "Key": item.clone(),
                            "ConsistentRead": true,
                        }),
                    )
                    .await;
                assert_eq!(
                    read["Item"], item,
                    "case {case:?}: expected the table to hold the item written to it, got \
                     {read:?}"
                );
            }
            Self::Sqs => {
                let (status, body) = emulator
                    .sqs(
                        "GetQueueAttributes",
                        json!({ "QueueUrl": identifier, "AttributeNames": ["QueueArn"] }),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: sqs:GetQueueAttributes expected HTTP 200, got {status} \
                     with body {body:?}"
                );
                assert!(
                    body["Attributes"]["QueueArn"].is_string(),
                    "case {case:?}: expected the owning service to report the queue's own \
                     ARN, got {body:?}"
                );
                emulator
                    .sqs_ok(
                        "SendMessage",
                        json!({ "QueueUrl": identifier, "MessageBody": WRITTEN }),
                    )
                    .await;
                let received = emulator
                    .sqs_ok("ReceiveMessage", json!({ "QueueUrl": identifier }))
                    .await;
                let bodies: Vec<String> = received["Messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|carried| text(carried, "Body"))
                    .collect();
                assert_eq!(
                    bodies,
                    vec![WRITTEN.to_string()],
                    "case {case:?}: expected the queue to deliver the message sent to it, \
                     got {received:?}"
                );
            }
            Self::Ssm => {
                let (status, body) = emulator
                    .ssm("GetParameter", json!({ "Name": identifier }))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: ssm:GetParameter expected HTTP 200, got {status} with \
                     body {body:?}"
                );
                let parameter = &body["Parameter"];
                assert_eq!(
                    parameter["Name"],
                    json!(identifier),
                    "case {case:?}: expected the parameter's own name, got {parameter:?}"
                );
                assert_eq!(
                    parameter["Value"], desired["Value"],
                    "case {case:?}: expected the value the create carried, got {parameter:?}"
                );
            }
        }
    }

    /// Assert the OWNING service no longer holds the resource `identifier` names: every
    /// operation of its own API that addresses the resource refuses it with that service's
    /// own error code and its own sentence.
    async fn assert_absent(
        self,
        emulator: &Emulator,
        case: &str,
        identifier: &str,
        desired: &Value,
    ) {
        for (operation, request, expected_code, expected_message) in
            self.absent_refusals(identifier, desired)
        {
            let (status, body) = match self {
                Self::Dynamodb => emulator.dynamodb(operation, request).await,
                Self::Sqs => emulator.sqs(operation, request).await,
                Self::Ssm => emulator.ssm(operation, request).await,
            };
            assert_eq!(
                status, 400,
                "case {case:?} ({operation}): expected HTTP 400 for a resource the owning \
                 service no longer holds, got {status} with body {body:?}"
            );
            assert!(
                code(&body).contains(expected_code),
                "case {case:?} ({operation}): expected {expected_code}, got {:?}",
                code(&body)
            );
            if let Some(expected) = expected_message {
                assert_eq!(
                    message(&body),
                    expected,
                    "case {case:?} ({operation}): unexpected wording"
                );
            }
        }
    }

    /// The calls of the owning service's own API that must refuse the absent resource, each
    /// with the error code and — where that service words one for the condition — the
    /// message it answers.
    ///
    /// Catalog source: the refusal each owning service is recorded answering for a resource
    /// it does not hold. Amazon DynamoDB names the table it could not find on the control
    /// plane and carries its fixed text alone on the data plane; Amazon SQS answers one
    /// fixed sentence for every url-addressed call; the Systems Manager refusal for a
    /// parameter lookup carries no message at all, so only its code is pinned.
    fn absent_refusals(
        self,
        identifier: &str,
        desired: &Value,
    ) -> Vec<(&'static str, Value, &'static str, Option<String>)> {
        match self {
            Self::Dynamodb => vec![
                (
                    "DescribeTable",
                    json!({ "TableName": identifier }),
                    RESOURCE_NOT_FOUND,
                    Some(format!(
                        "Requested resource not found: Table: {identifier} not found"
                    )),
                ),
                (
                    "PutItem",
                    json!({ "TableName": identifier, "Item": key_item(desired) }),
                    RESOURCE_NOT_FOUND,
                    Some("Requested resource not found".to_string()),
                ),
            ],
            Self::Sqs => {
                let sentence = "The specified queue does not exist.";
                vec![
                    (
                        "GetQueueAttributes",
                        json!({ "QueueUrl": identifier, "AttributeNames": ["QueueArn"] }),
                        "QueueDoesNotExist",
                        Some(sentence.to_string()),
                    ),
                    (
                        "SendMessage",
                        json!({ "QueueUrl": identifier, "MessageBody": WRITTEN }),
                        "QueueDoesNotExist",
                        Some(sentence.to_string()),
                    ),
                ]
            }
            Self::Ssm => vec![(
                "GetParameter",
                json!({ "Name": identifier }),
                "ParameterNotFound",
                None,
            )],
        }
    }
}

/// One item of the table `desired` declares, keyed on every attribute its `KeySchema` names
/// — so a round-trip is written against the row's OWN primary key rather than one shape
/// every row shares.
fn key_item(desired: &Value) -> Value {
    let mut item = Map::new();
    for element in desired["KeySchema"].as_array().cloned().unwrap_or_default() {
        item.insert(text(&element, "AttributeName"), json!({ "S": WRITTEN }));
    }
    Value::Object(item)
}

/// One row of the table: the case, the service that owns the row's type, the desired state a
/// create carries, and the primary identifier the resource must answer to afterwards.
type Row = (&'static str, Owner, Value, String);

/// The rows every case below is driven over.
///
/// They differ along the owning service and its wire, the form of the primary identifier,
/// the shape of the primary key a table is defined over, the capacity mode it is created in,
/// and the properties the desired state carries.
fn rows() -> Vec<Row> {
    vec![
        (
            "an on-demand table keyed on one attribute",
            Owner::Dynamodb,
            json!({
                "TableName": "barrier-on-demand",
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
                "Tags": [{ "Key": "barrier", "Value": "on-demand" }],
            }),
            "barrier-on-demand".to_string(),
        ),
        (
            "a provisioned table keyed on a partition and a sort attribute",
            Owner::Dynamodb,
            json!({
                "TableName": "barrier-provisioned",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "sk", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "sk", "AttributeType": "S" },
                ],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 3,
                    "WriteCapacityUnits": 2,
                },
            }),
            "barrier-provisioned".to_string(),
        ),
        (
            "a queue, identified by the url the owning service assigned",
            Owner::Sqs,
            json!({
                "QueueName": "barrier-queue",
                "Tags": [{ "Key": "barrier", "Value": "queue" }],
            }),
            queue_url("barrier-queue"),
        ),
        (
            "a hierarchical parameter, identified by its own name",
            Owner::Ssm,
            json!({
                "Name": "/barrier/config/value",
                "Type": "String",
                "Value": WRITTEN,
                "Description": "written through the facade",
            }),
            "/barrier/config/value".to_string(),
        ),
    ]
}

/// Assert `event` reports one of the modelled NON-TERMINAL statuses, which is how an
/// accepted resource operation request is answered.
fn assert_accepted(event: &Value, case: &str, operation: &str) -> String {
    let status = text(event, "OperationStatus");
    assert!(
        [STATUS_PENDING, STATUS_IN_PROGRESS].contains(&status.as_str()),
        "case {case:?}: expected the accepted {operation} to be answered in a non-terminal \
         status, got {status:?} in {event:?}"
    );
    text(event, "RequestToken")
}

#[tokio::test]
async fn a_create_that_reports_success_is_a_resource_the_owning_service_already_serves() {
    for (case, owner, desired, identifier) in rows() {
        let emulator = owner.emulator();
        let type_name = owner.type_name();

        let accepted = emulator.create_ok(type_name, &desired, None).await;
        let request_token = assert_accepted(&accepted, case, "CreateResource");
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: expected the create to complete, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "Identifier"),
            identifier,
            "case {case:?}: expected the resource's primary identifier, got {settled:?}"
        );

        // With no further waiting: the owning service already holds and serves it.
        owner
            .assert_serves(&emulator, case, &identifier, &desired)
            .await;
    }
}

#[tokio::test]
async fn a_delete_that_reports_success_is_a_resource_the_owning_service_no_longer_holds() {
    for (case, owner, desired, identifier) in rows() {
        let emulator = owner.emulator();
        let type_name = owner.type_name();
        let provisioned = emulator.provision(type_name, &desired).await;
        assert_eq!(
            provisioned, identifier,
            "case {case:?}: expected the resource's primary identifier, got {provisioned:?}"
        );

        let (status, body) = emulator.delete(type_name, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteResource expected HTTP 200, got {status} with body {body:?}"
        );
        let accepted = progress(&body);
        let request_token = assert_accepted(&accepted, case, "DeleteResource");
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: expected the delete to complete, got {settled:?}"
        );

        // With no further waiting: the owning service holds it no longer, and answers for
        // its absence in its own terms.
        owner
            .assert_absent(&emulator, case, &identifier, &desired)
            .await;

        // And the facade reports the resource as one that cannot be found.
        let (status, body) = emulator.get(type_name, &identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: GetResource after the delete expected HTTP 400, got {status} with \
             body {body:?}"
        );
        assert!(
            code(&body).contains(RESOURCE_NOT_FOUND),
            "case {case:?}: expected {RESOURCE_NOT_FOUND}, got {:?}",
            code(&body)
        );
    }
}

#[tokio::test]
async fn a_create_the_owning_service_will_not_perform_reports_failed_not_success() {
    for (case, owner, desired, identifier) in rows() {
        let emulator = owner.emulator();
        let type_name = owner.type_name();
        let provisioned = emulator.provision(type_name, &desired).await;
        assert_eq!(
            provisioned, identifier,
            "case {case:?}: expected the resource's primary identifier, got {provisioned:?}"
        );

        // The resource is already there, which is the OWNING handler's verdict: the second
        // create is accepted like any other and polled to a TERMINAL FAILED carrying that
        // verdict, because a status of SUCCESS would claim work that service did not do.
        let accepted = emulator.create_ok(type_name, &desired, None).await;
        let request_token = assert_accepted(&accepted, case, "CreateResource");
        let settled = emulator.terminal(&request_token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected a create of a resource the owning service already holds \
             to fail, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            HANDLER_ALREADY_EXISTS,
            "case {case:?}: expected the {HANDLER_ALREADY_EXISTS} handler error code, got \
             {settled:?}"
        );
        // The SENTENCE that verdict carries belongs to the service that owns the type, which
        // `refusal_wording` holds per type; what every type shares, and what this case is
        // about, is that the failure is reported at all.
        assert!(
            !text(&settled, "StatusMessage").is_empty(),
            "case {case:?}: expected the owning handler's diagnostic, got {settled:?}"
        );

        // And the resource the refused create named is exactly the one that service still
        // holds and serves.
        owner
            .assert_serves(&emulator, case, &identifier, &desired)
            .await;
    }
}
