// SPDX-License-Identifier: CC0-1.0

//! The settled request record both of this service's request views report, as the barrier
//! the OWNING service's own state has already passed.
//!
//! The rule every case here holds the service to: **the two views the model gives a resource
//! operation request report ONE record — the `ProgressEvent` `GetResourceRequestStatus`
//! resolves the request's `RequestToken` to, and the summary `ListResourceRequests` reports
//! for that same token — and a record they report as `SUCCESS`, which the model documents as
//! "The resource operation has successfully completed", names a resource the service that
//! OWNS the request's `TypeName` already holds and answers for in its own terms: a settled
//! `CREATE` names one that service reports in its ready state at the instant a poll first
//! observed the terminal status, and a settled `DELETE` names one that service no longer
//! holds.**
//!
//! A facade that settled a request at `SUCCESS` with no resource in the owning service's
//! store leaves "created" and "not created" indistinguishable to a client that polled the
//! documented stabilization barrier and then called the owning service's own API; the model
//! carries `FAILED` and `HandlerFailureException` precisely so a handler that cannot stand
//! the resource up reports the failure instead.
//!
//! The rows differ along what the rule generalises over: which service owns the type and
//! over which wire it answers (a JSON RPC control plane, a query API, a REST API), the form
//! the type's primary identifier takes (a name the desired state chose, an ARN the owning
//! service assigns), and the shape of the resource asked for — the primary key and capacity
//! mode of a table, the kind of a topic.
//!
//! `stabilization_barrier` holds the barrier per owning service through the status call
//! alone, and `request_listing` holds the agreement of the two views without asking any
//! owning service; what is asserted here is the two TOGETHER, so a settled record is backed
//! by the owning service's state whichever view a client read it from.

use serde_json::{json, Value};

use super::emulator::{
    code, element, listing_shaped, progress, text, Emulator, RESOURCE_MODEL, STATUS_SUCCESS,
};
use super::sns_topic_read_through::{attribute, attributes};

/// The `Operation` a create's request record reports, as the model's own enum declares it
/// (`smithy_query(query="shape",shape="Operation")`).
const OPERATION_CREATE: &str = "CREATE";

/// The `Operation` a delete's request record reports; see [`OPERATION_CREATE`].
const OPERATION_DELETE: &str = "DELETE";

/// How many request records one listing call reports here: far more than any case of this
/// suite initiates, so the record a case looks for is on the first page whichever order the
/// listing reports them in.
const EVERY_REQUEST: i64 = 100;

/// Which service owns the row's resource type — the store a resource operation of this
/// facade has to land in — and how that service is asked about one of its resources.
#[derive(Clone, Copy, Debug)]
enum Owner {
    /// Amazon DynamoDB, which owns `AWS::DynamoDB::Table` and answers over a JSON RPC
    /// control plane.
    Dynamodb,
    /// Amazon SNS, which owns `AWS::SNS::Topic` and answers over its query API.
    Sns,
    /// Amazon S3, which owns `AWS::S3::Bucket` and answers over its REST API.
    S3,
}

impl Owner {
    /// The `TypeName` whose resources this service owns.
    fn type_name(self) -> &'static str {
        match self {
            Self::Dynamodb => "AWS::DynamoDB::Table",
            Self::Sns => "AWS::SNS::Topic",
            Self::S3 => "AWS::S3::Bucket",
        }
    }

    /// One emulator with this service mounted beside Cloud Control API over the same engine,
    /// so a resource operation of the facade has the owning service's own store to land in.
    fn emulator(self) -> Emulator {
        match self {
            Self::Dynamodb => Emulator::with_dynamodb(),
            Self::Sns => Emulator::with_sns(),
            Self::S3 => Emulator::with_s3(),
        }
    }

    /// Assert the OWNING service already holds the resource `identifier` names and reports
    /// it in its own terms.
    ///
    /// Nothing here waits: the call is made at the instant the poll first observed the
    /// request's terminal status, which is what makes that status a barrier rather than an
    /// announcement.
    async fn assert_holds(self, emulator: &Emulator, case: &str, identifier: &str) {
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
                    described["TableName"],
                    json!(identifier),
                    "case {case:?}: expected the table the settled record named, got \
                     {described:?}"
                );
                assert_eq!(
                    described["TableStatus"],
                    json!("ACTIVE"),
                    "case {case:?}: expected the owning service to report a settled table, \
                     got {described:?}"
                );
            }
            Self::Sns => {
                let body = attributes(emulator, case, identifier).await;
                assert_eq!(
                    attribute(&body, "TopicArn"),
                    identifier,
                    "case {case:?}: the owning service must hold the topic under the ARN the \
                     settled record named, got {body:?}"
                );
            }
            Self::S3 => {
                let (status, body) = emulator
                    .s3_bucket_call("HEAD", &format!("/{identifier}"))
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: s3:HeadBucket expected HTTP 200, got {status} with body \
                     {body:?}"
                );
            }
        }
    }

    /// Assert the OWNING service no longer holds the resource `identifier` names: its own
    /// API refuses the resource in that service's own terms.
    async fn assert_absent(self, emulator: &Emulator, case: &str, identifier: &str) {
        match self {
            Self::Dynamodb => {
                let (status, body) = emulator
                    .dynamodb("DescribeTable", json!({ "TableName": identifier }))
                    .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: dynamodb:DescribeTable expected HTTP 400 for a table the \
                     owning service no longer holds, got {status} with body {body:?}"
                );
                assert!(
                    code(&body).contains("ResourceNotFoundException"),
                    "case {case:?}: expected the owning service's own error code, got {:?}",
                    code(&body)
                );
            }
            Self::Sns => {
                let (status, body) = emulator
                    .sns("GetTopicAttributes", &[("TopicArn", identifier)])
                    .await;
                assert_eq!(
                    status, 404,
                    "case {case:?}: sns:GetTopicAttributes expected HTTP 404 for a topic the \
                     owning service no longer holds, got {status} with body {body:?}"
                );
                assert_eq!(
                    element(&body, "Code"),
                    "NotFound",
                    "case {case:?}: expected the owning service's own error code, got {body:?}"
                );
            }
            Self::S3 => {
                let (status, body) = emulator
                    .s3_bucket_call("HEAD", &format!("/{identifier}"))
                    .await;
                assert_eq!(
                    status, 404,
                    "case {case:?}: s3:HeadBucket expected HTTP 404 for a bucket the owning \
                     service no longer holds, got {status} with body {body:?}"
                );
            }
        }
    }
}

/// One row of the table: what the case is, which service owns the type, and the desired
/// state the create carries.
type Row = (&'static str, Owner, Value);

/// The resources every case of this suite creates through the facade.
///
/// The rows differ along the owning service and its wire, along the form of the type's
/// primary identifier — a name the desired state chose for a table and a bucket, the ARN
/// Amazon SNS assigns a topic — and along the shape of the resource asked for.
fn rows() -> [Row; 5] {
    [
        (
            "a table whose primary key is a partition key alone, on demand",
            Owner::Dynamodb,
            json!({
                "TableName": "views-on-demand",
                "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        ),
        (
            "a table with a composite primary key and provisioned capacity",
            Owner::Dynamodb,
            json!({
                "TableName": "views-provisioned",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "at", "KeyType": "RANGE" },
                ],
                "AttributeDefinitions": [
                    { "AttributeName": "pk", "AttributeType": "S" },
                    { "AttributeName": "at", "AttributeType": "N" },
                ],
                "ProvisionedThroughput": {
                    "ReadCapacityUnits": 4,
                    "WriteCapacityUnits": 2,
                },
            }),
        ),
        (
            "a topic whose identifier the owning service assigns",
            Owner::Sns,
            json!({ "TopicName": "views-topic", "DisplayName": "Views" }),
        ),
        (
            "a FIFO topic, named as that kind of topic must be",
            Owner::Sns,
            json!({ "TopicName": "views-topic.fifo", "FifoTopic": true }),
        ),
        (
            "a bucket the owning service answers for over its REST wire",
            Owner::S3,
            json!({ "BucketName": "views-bucket" }),
        ),
    ]
}

/// The summary `ListResourceRequests` reports for `token`, required to be there.
async fn summary(emulator: &Emulator, case: &str, token: &str) -> Value {
    let listed = emulator
        .cc_ok(
            "ListResourceRequests",
            json!({ "MaxResults": EVERY_REQUEST }),
        )
        .await;
    let reported = listed["ResourceRequestStatusSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    reported
        .into_iter()
        .find(|summary| text(summary, "RequestToken") == token)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report the request {token:?}, got {listed:?}")
        })
}

/// Assert the two views report ONE record for `token`: the summary the listing reports is
/// the `ProgressEvent` the status call resolves the token to, projected as a listing
/// reports it — every member of that event except the resource model, which the status
/// call surfaces and the listing does not — and it names the operation, type and identifier
/// expected of it.
///
/// The status call is made AFTER the listing, so a record the two views compose differently
/// fails here whichever of them a client read first.
async fn assert_views_agree(
    emulator: &Emulator,
    case: &str,
    token: &str,
    operation: &str,
    type_name: &str,
    identifier: &str,
) {
    let listed = summary(emulator, case, token).await;
    let (status, body) = emulator.status(token).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResourceRequestStatus expected HTTP 200, got {status} with body \
         {body:?}"
    );
    assert_eq!(
        listed,
        listing_shaped(&progress(&body)),
        "case {case:?}: the listing and the status call must report the same ProgressEvent \
         apart from the resource model, got {listed:?} and {:?}",
        progress(&body)
    );
    assert!(
        listed.get(RESOURCE_MODEL).is_none(),
        "case {case:?}: the listing summary must carry no {RESOURCE_MODEL}, got {listed:?}"
    );
    for (view, event) in [("the listing", &listed), ("the status call", &progress(&body))] {
        assert_eq!(
            text(event, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?} ({view}): expected a completed request, got {event:?}"
        );
        assert_eq!(
            text(event, "Operation"),
            operation,
            "case {case:?} ({view}): expected the {operation} operation, got {event:?}"
        );
        assert_eq!(
            text(event, "TypeName"),
            type_name,
            "case {case:?} ({view}): expected the type the request named, got {event:?}"
        );
        assert_eq!(
            text(event, "Identifier"),
            identifier,
            "case {case:?} ({view}): expected the resource the request acted on, got \
             {event:?}"
        );
    }
}

#[tokio::test]
async fn a_settled_create_both_views_report_names_a_resource_the_owning_service_holds() {
    for (case, owner, desired) in rows() {
        let emulator = owner.emulator();
        let type_name = owner.type_name();

        let accepted = emulator.create_ok(type_name, &desired, None).await;
        let token = text(&accepted, "RequestToken");
        let settled = emulator.terminal(&token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: the create must complete rather than fail, got {settled:?}"
        );
        let identifier = text(&settled, "Identifier");
        assert!(
            !identifier.is_empty(),
            "case {case:?}: a settled create must name the resource it provisioned, got \
             {settled:?}"
        );

        // At the instant the poll FIRST observed the terminal status, with no further
        // waiting, the service that owns the type already holds the resource.
        owner.assert_holds(&emulator, case, &identifier).await;

        assert_views_agree(
            &emulator,
            case,
            &token,
            OPERATION_CREATE,
            type_name,
            &identifier,
        )
        .await;
    }
}

#[tokio::test]
async fn a_settled_delete_both_views_report_names_a_resource_the_owning_service_dropped() {
    for (case, owner, desired) in rows() {
        let emulator = owner.emulator();
        let type_name = owner.type_name();

        // Really provisioned first, so what the delete settles about is a resource the
        // owning service held.
        let identifier = emulator.provision(type_name, &desired).await;
        owner.assert_holds(&emulator, case, &identifier).await;

        let (status, body) = emulator.delete(type_name, &identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteResource expected HTTP 200, got {status} with body {body:?}"
        );
        let token = text(&progress(&body), "RequestToken");
        let settled = emulator.terminal(&token).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_SUCCESS,
            "case {case:?}: the delete must complete rather than fail, got {settled:?}"
        );

        // And the removal has already reached the owning service's own store.
        owner.assert_absent(&emulator, case, &identifier).await;

        assert_views_agree(
            &emulator,
            case,
            &token,
            OPERATION_DELETE,
            type_name,
            &identifier,
        )
        .await;
    }
}
