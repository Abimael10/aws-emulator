// SPDX-License-Identifier: CC0-1.0

//! WHICH FRAME a refusal about a FIELD of the ARN a table-naming member carried is stated
//! in, driven end to end.
//!
//! RULE under test: a `TableName` may be spelled as the table's ARN ("You can also provide
//! the Amazon Resource Name (ARN) of the table in this parameter"), and a value that is a
//! well-formed ARN of this service naming something else is refused for the FIELD of the ARN
//! that is wrong — its resource type, or its region. That diagnosis is stated over the ARN
//! the request sent, and the FRAME it is stated in belongs to the OPERATION rather than to
//! the field: an operation states every ARN field's refusal the same way, as the diagnosis
//! sentence over the value, and the single-item read states every one of them as that
//! member's own constraint clause instead.
//!
//! The rows differ along what the rule generalises over: the FIELD that is wrong (the
//! resource type, in each of the ways it can be, and the region), the OPERATION that
//! resolved the name — both planes, reads and writes — and therefore the frame it states.
//! The last assertions drive the table's OWN ARN through the same operations, so the
//! refusals above are about the values sent and not about an ARN in the member.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok, REGION};
use crate::table_items::create_active_table;

/// The table every row addresses.
const TABLE: &str = "ArnFieldFrames";

/// The account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// A Region this endpoint does not serve, for the row about the ARN's region field.
const OTHER_REGION: &str = "eu-west-1";

/// The emulator with [`TABLE`] in place, so no row is refused for a missing table.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    router
}

/// `TABLE`'s ARN with `resource` in place of its whole resource field.
fn arn_with_resource(resource: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:{resource}")
}

/// Every operation whose `TableName` resolves an ARN, with the rest of a request that gets
/// as far as that resolution, and the frame it states an ARN field's refusal in.
fn addressed_operations(arn: &str) -> Vec<(&'static str, Value, Frame)> {
    vec![
        (
            "DescribeKinesisStreamingDestination",
            json!({"TableName": arn}),
            Frame::Sentence,
        ),
        (
            "EnableKinesisStreamingDestination",
            json!({"TableName": arn,
                   "StreamArn": format!("arn:aws:kinesis:{REGION}:{ACCOUNT}:stream/cdc")}),
            Frame::Sentence,
        ),
        ("DescribeTable", json!({"TableName": arn}), Frame::Sentence),
        (
            "DescribeTimeToLive",
            json!({"TableName": arn}),
            Frame::Sentence,
        ),
        (
            "CreateBackup",
            json!({"TableName": arn, "BackupName": "frames"}),
            Frame::Sentence,
        ),
        ("Scan", json!({"TableName": arn}), Frame::Sentence),
        (
            "Query",
            json!({"TableName": arn, "KeyConditionExpression": "pk = :k",
                   "ExpressionAttributeValues": {":k": {"S": "one"}}}),
            Frame::Sentence,
        ),
        (
            "PutItem",
            json!({"TableName": arn, "Item": {"pk": {"S": "one"}}}),
            Frame::Sentence,
        ),
        (
            "DeleteItem",
            json!({"TableName": arn, "Key": {"pk": {"S": "one"}}}),
            Frame::Sentence,
        ),
        // The single-item READ states the same facts as that member's own constraint.
        (
            "GetItem",
            json!({"TableName": arn, "Key": {"pk": {"S": "one"}}}),
            Frame::MemberClause,
        ),
    ]
}

/// The two frames an ARN-field refusal is observed to be stated in.
#[derive(Debug, Clone, Copy)]
enum Frame {
    /// The diagnosis alone, naming the ARN the request sent.
    Sentence,
    /// The `tableName` member's own constraint clause, with the ARN echoed.
    MemberClause,
}

impl Frame {
    /// The whole message this frame states `diagnosis` about `arn` as.
    fn message(self, diagnosis: &str, arn: &str) -> String {
        match self {
            Self::Sentence => format!("1 validation error detected: {diagnosis} in '{arn}'"),
            Self::MemberClause => format!(
                "1 validation error detected: Value '{arn}' at 'tableName' failed to satisfy \
                 constraint: {diagnosis}"
            ),
        }
    }
}

#[tokio::test]
async fn every_field_of_a_table_arn_is_refused_in_the_frame_its_operation_states() {
    let router = seeded().await;

    // (case, the submitted TableName, the diagnosis its wrong field earns). The resource
    // type can be wrong in several ways — another resource of this service, a mis-cased
    // table type, no type at all — and the region is wrong when it is not the one this
    // endpoint serves.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a backup ARN",
            arn_with_resource(&format!("backup/{TABLE}")),
            "Invalid resource type",
        ),
        (
            "an import ARN",
            arn_with_resource("import/01700000000000-abcdef12"),
            "Invalid resource type",
        ),
        (
            "a mis-cased table type",
            arn_with_resource(&format!("Table/{TABLE}")),
            "Invalid resource type",
        ),
        (
            "an ARN with no resource type at all",
            arn_with_resource(TABLE),
            "Invalid resource type",
        ),
        (
            "a table ARN of another Region",
            format!("arn:aws:dynamodb:{OTHER_REGION}:{ACCOUNT}:table/{TABLE}"),
            "Invalid AWS region",
        ),
    ];

    for (case, arn, diagnosis) in &rows {
        for (operation, request, frame) in addressed_operations(arn) {
            let (status, answered) = call(&router, operation, request).await;
            let expected = frame.message(diagnosis, arn);
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (400, "ValidationException", expected.clone()),
                "case {case:?} ({operation}): expected {expected:?}, got {status} with \
                 {answered}"
            );
        }
    }
}

#[tokio::test]
async fn the_table_s_own_arn_still_addresses_it_on_every_one_of_those_operations() {
    let router = seeded().await;
    let own = arn_with_resource(&format!("table/{TABLE}"));

    // The same operations, addressed by the ARN whose every field is right: each one is
    // served, so the refusals above are about the FIELD that was wrong. The streaming
    // destination is the one row that is not served by an address alone — it needs a
    // stream this endpoint holds — so it is read back rather than enabled.
    let described = ok(&router, "DescribeTable", json!({"TableName": own.clone()})).await;
    assert_eq!(
        described["Table"]["TableName"],
        json!(TABLE),
        "case the table's own ARN (DescribeTable): expected the table it names, got {described}"
    );
    let ttl = ok(
        &router,
        "DescribeTimeToLive",
        json!({"TableName": own.clone()}),
    )
    .await;
    assert_eq!(
        ttl["TimeToLiveDescription"]["TimeToLiveStatus"],
        json!("DISABLED"),
        "case the table's own ARN (DescribeTimeToLive): expected the setting it holds, got {ttl}"
    );
    let destination = ok(
        &router,
        "DescribeKinesisStreamingDestination",
        json!({"TableName": own.clone()}),
    )
    .await;
    assert_eq!(
        destination["TableName"],
        json!(TABLE),
        "case the table's own ARN (DescribeKinesisStreamingDestination): expected the table \
         it names, got {destination}"
    );
    ok(
        &router,
        "PutItem",
        json!({"TableName": own.clone(), "Item": {"pk": {"S": "one"}}}),
    )
    .await;
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": own.clone(), "Key": {"pk": {"S": "one"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read["Item"]["pk"],
        json!({"S": "one"}),
        "case the table's own ARN (GetItem): expected the item the ARN-addressed write \
         stored, got {read}"
    );
    let scanned = ok(&router, "Scan", json!({"TableName": own})).await;
    assert_eq!(
        scanned["Count"],
        json!(1),
        "case the table's own ARN (Scan): expected the one stored item, got {scanned}"
    );
}
