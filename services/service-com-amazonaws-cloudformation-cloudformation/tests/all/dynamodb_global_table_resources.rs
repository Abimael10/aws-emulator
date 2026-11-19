// SPDX-License-Identifier: CC0-1.0

//! `AWS::DynamoDB::GlobalTable` end to end: the global table a stack declares really exists
//! in Amazon DynamoDB — as a table in the stack's own Region and a replica in every other
//! Region the declaration lists.
//!
//! RULE: a declared resource of a type this emulator provisions is created in the service
//! that owns it, and a resource reported `CREATE_COMPLETE` is one every call it was
//! published for resolves against. For a global table that means the data plane: `PutItem`
//! on the published name in the stack's Region succeeds, the item appears in each replica
//! Region, and a write made in a replica Region appears in the others — because a global
//! table IS one table across Regions. The identifiers the stack publishes are that service's
//! own (`Arn`, `StreamArn`, `TableId`), and so is the capacity each regional table reports.
//!
//! The rows differ along what the rule generalises over: how many Regions the group spans,
//! the capacity mode (and so whether the capacity is stated as a provisioned auto scaling
//! range or left on demand), whether the template names the table or leaves CloudFormation
//! to name it, and whether a replica overrides the group's read capacity.
//!
//! Every case drives the real request path for BOTH services through one router — the
//! CloudFormation calls over the awsQuery wire (the harness `stack_lifecycle.rs` owns) and
//! the DynamoDB calls as a client's SDK makes them, in each Region — so what is asserted is
//! what a client that deploys a global table by template and then uses it observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::failure_reasons;
use crate::stack_lifecycle::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text, ACCOUNT_ID, REGION,
};

/// The awsJson version Amazon DynamoDB's model declares (`awsJson1_0`).
const DYNAMODB_JSON: &str = "application/x-amz-json-1.0";

/// The Region the replica of a two-Region group lives in, and the third Region a wider group
/// adds.
const PEER_REGION: &str = "us-west-2";
const THIRD_REGION: &str = "eu-west-1";

/// How long the group of a deployed global table may take to answer in a Region, and how
/// often it is asked.
///
/// Generous over the spans the owning service owes — the table's own creation window, then
/// each replica's — so a loaded machine is not read as a defect; what is waited FOR is the
/// condition, and a wait that ends is the point.
const REPLICATED_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const REPLICATED_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Call one DynamoDB operation through the same router IN `region`, signed for `dynamodb`
/// the way a client's SDK signs it.
///
/// The Region travels because a global table spans Regions: the suite over one table
/// (`dynamodb_table_resources.rs`) calls in the Region the stack is deployed in and needs no
/// other, while every assertion here is about which Region answers what.
async fn dynamodb_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://dynamodb.{region}.amazonaws.com/"))
        .header("content-type", DYNAMODB_JSON)
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/dynamodb/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed DynamoDB request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    let document: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, document)
}

/// The `Table` description DynamoDB answers for `name` in `region`, or `Null` where that
/// service holds no such table there.
async fn described_in(router: &MultiServiceRouter, region: &str, name: &str) -> Value {
    let (status, document) =
        dynamodb_in(router, region, "DescribeTable", &json!({"TableName": name})).await;
    if status == 200 {
        return document["Table"].clone();
    }
    Value::Null
}

/// The `Replicas` entry a description carries for `region`, or `Null` where it carries none.
fn replica_in(description: &Value, region: &str) -> Value {
    description["Replicas"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|replica| replica["RegionName"] == json!(region))
        .unwrap_or(Value::Null)
}

/// Poll `DescribeTable` in `region` until its description satisfies `settled`, answering it —
/// the waiter a client runs for a transition the owning service owes.
async fn table_when<F>(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    settled: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    let deadline = std::time::Instant::now() + REPLICATED_DEADLINE;
    loop {
        let described = described_in(router, region, name).await;
        if settled(&described) {
            return described;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {name} in {region} never settled within {REPLICATED_DEADLINE:?}, \
             last {described}"
        );
        tokio::time::sleep(REPLICATED_POLL_INTERVAL).await;
    }
}

/// Poll `GetItem` in `region` until the item keyed `id` is there, answering it — the waiter a
/// client runs for a write made in another Region of the group, which replicates rather than
/// landing instantly.
async fn item_when_replicated(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
    id: &str,
) -> Value {
    let deadline = std::time::Instant::now() + REPLICATED_DEADLINE;
    let request = json!({"TableName": name, "Key": {"pk": {"S": id}}});
    loop {
        let (status, document) = dynamodb_in(router, region, "GetItem", &request).await;
        if status == 200 && document["Item"] != Value::Null {
            return document["Item"].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: {id} never reached {name} in {region} within \
             {REPLICATED_DEADLINE:?}, last {status} with {document}"
        );
        tokio::time::sleep(REPLICATED_POLL_INTERVAL).await;
    }
}

/// The `OutputValue` the settled stack reports for `key`.
fn output(xml: &str, key: &str) -> Option<String> {
    let block = xml_blocks(xml, "Outputs").into_iter().next()?;
    xml_blocks(&block, "member")
        .into_iter()
        .find(|output| xml_text(output, "OutputKey").as_deref() == Some(key))
        .and_then(|output| xml_text(&output, "OutputValue"))
}

/// One row of the table: a declared global table, and what it must leave behind.
struct Row<'a> {
    case: &'a str,
    stack: &'a str,
    /// The `AWS::DynamoDB::GlobalTable` properties the template declares.
    properties: Value,
    /// The table name the template declares, or `None` where CloudFormation names it.
    declared_name: Option<&'a str>,
    /// The Regions the group spans beside the stack's own.
    peers: Vec<&'a str>,
    /// The provisioned pair each regional table must report, or `None` for an on-demand
    /// group: the Region, its read capacity and the group's write capacity.
    provisioned: Vec<(&'a str, i64, i64)>,
    /// The attribute the group's `TimeToLiveSpecification` expires items by, where the
    /// declaration states one: a group-level setting, so every Region of the group must
    /// report it in force.
    time_to_live: Option<&'a str>,
}

/// The template one row deploys: the global table, and the identifiers a client reads it
/// through.
fn template(row: &Row<'_>) -> String {
    json!({
        "Resources": {
            "Group": {
                "Type": "AWS::DynamoDB::GlobalTable",
                "Properties": row.properties.clone(),
            },
        },
        "Outputs": {
            "TableName": {"Value": {"Ref": "Group"}},
            "TableArn": {"Value": {"Fn::GetAtt": ["Group", "Arn"]}},
            "StreamArn": {"Value": {"Fn::GetAtt": ["Group", "StreamArn"]}},
            "TableId": {"Value": {"Fn::GetAtt": ["Group", "TableId"]}},
        },
    })
    .to_string()
}

/// The two rows: an on-demand group of two Regions under a declared name, and a provisioned
/// group of three Regions whose capacity is stated as the auto scaling ranges this type
/// states a provisioned capacity in, each replica overriding its own read half.
fn rows() -> Vec<Row<'static>> {
    vec![
        Row {
            case: "an on-demand group of two Regions",
            stack: "ddb-global-on-demand",
            properties: json!({
                "TableName": "declared-global-table",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                // Stated ONCE for the whole global table, so it has to be in force in
                // every Region of the group.
                "TimeToLiveSpecification": {"AttributeName": "expiresAt", "Enabled": true},
                "Replicas": [{"Region": REGION}, {"Region": PEER_REGION}],
            }),
            declared_name: Some("declared-global-table"),
            peers: vec![PEER_REGION],
            provisioned: Vec::new(),
            time_to_live: Some("expiresAt"),
        },
        Row {
            case: "a provisioned group of three Regions",
            stack: "ddb-global-provisioned",
            properties: json!({
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PROVISIONED",
                "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                "WriteProvisionedThroughputSettings": {
                    "WriteCapacityAutoScalingSettings": {
                        "MinCapacity": 5,
                        "MaxCapacity": 20,
                        "TargetTrackingScalingPolicyConfiguration": {"TargetValue": 70.0},
                    },
                },
                "Replicas": [
                    {
                        "Region": REGION,
                        "ReadProvisionedThroughputSettings": {"ReadCapacityUnits": 4},
                    },
                    {
                        "Region": PEER_REGION,
                        "ReadProvisionedThroughputSettings": {"ReadCapacityUnits": 7},
                    },
                    {
                        "Region": THIRD_REGION,
                        "ReadProvisionedThroughputSettings": {
                            "ReadCapacityAutoScalingSettings": {
                                "MinCapacity": 2,
                                "MaxCapacity": 8,
                                "SeedCapacity": 3,
                            },
                        },
                    },
                ],
            }),
            declared_name: None,
            peers: vec![PEER_REGION, THIRD_REGION],
            provisioned: vec![(REGION, 4, 5), (PEER_REGION, 7, 5), (THIRD_REGION, 3, 5)],
            time_to_live: None,
        },
    ]
}

#[tokio::test]
async fn a_declared_global_table_is_addressable_in_every_region_of_its_group() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let (status, body) = create_stack(&router, row.stack, &template(&row), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} {body}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        let stack_status = xml_text(&described, "StackStatus");
        assert_eq!(
            stack_status.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {stack_status:?} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The name the stack publishes is the one DynamoDB holds the table under, in the
        // stack's own Region.
        let name = output(&described, "TableName").unwrap_or_default();
        if let Some(declared) = row.declared_name {
            assert_eq!(
                name, declared,
                "case {case:?}: expected Ref to answer the declared table name, got {name:?}"
            );
        }
        let local = described_in(&router, REGION, &name).await;
        assert_eq!(
            (
                local["TableStatus"].clone(),
                local["GlobalTableVersion"].clone(),
            ),
            (json!("ACTIVE"), json!("2019.11.21")),
            "case {case:?}: expected an ACTIVE global table in {REGION}, got {local}"
        );

        // Every Region the declaration lists is a replica of it, reported ACTIVE — and the
        // table answers for itself in each of those Regions, reporting the others.
        for peer in &row.peers {
            assert_eq!(
                replica_in(&local, peer)["ReplicaStatus"].clone(),
                json!("ACTIVE"),
                "case {case:?}: expected {peer} to be an ACTIVE replica, got {local}"
            );
            let reported = table_when(&router, case, peer, &name, |table| {
                table["TableStatus"] == json!("ACTIVE")
            })
            .await;
            assert_eq!(
                (
                    reported["GlobalTableVersion"].clone(),
                    replica_in(&reported, REGION)["ReplicaStatus"].clone(),
                ),
                (json!("2019.11.21"), json!("ACTIVE")),
                "case {case:?}: expected the replica in {peer} to report the group, got \
                 {reported}"
            );
        }

        // The data plane: a write in the stack's own Region succeeds and reaches every
        // replica Region, and a write made in a replica Region reaches the others — which is
        // what makes the declared resource a global table rather than a name.
        let (status, written) = dynamodb_in(
            &router,
            REGION,
            "PutItem",
            &json!({
                "TableName": name,
                "Item": {"pk": {"S": "from-the-stack-region"}, "note": {"S": "local"}},
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutItem in {REGION} to be served after CREATE_COMPLETE, \
             got {status} with {written}"
        );
        for peer in &row.peers {
            let item = item_when_replicated(&router, case, peer, &name, "from-the-stack-region").await;
            assert_eq!(
                item["note"].clone(),
                json!({"S": "local"}),
                "case {case:?}: expected the item written in {REGION} to reach {peer}, got \
                 {item}"
            );
            let (status, written) = dynamodb_in(
                &router,
                peer,
                "PutItem",
                &json!({
                    "TableName": name,
                    "Item": {"pk": {"S": format!("from-{peer}")}, "note": {"S": "peer"}},
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected PutItem in {peer} to be served, got {status} with \
                 {written}"
            );
            let item =
                item_when_replicated(&router, case, REGION, &name, &format!("from-{peer}")).await;
            assert_eq!(
                item["note"].clone(),
                json!({"S": "peer"}),
                "case {case:?}: expected the item written in {peer} to reach {REGION}, got \
                 {item}"
            );
        }

        // The identifiers the stack publishes are the ones DynamoDB reports for the table in
        // the stack's Region.
        assert_eq!(
            (
                output(&described, "TableArn"),
                output(&described, "TableId"),
                output(&described, "StreamArn"),
            ),
            (
                local["TableArn"].as_str().map(ToString::to_string),
                local["TableId"].as_str().map(ToString::to_string),
                local["LatestStreamArn"].as_str().map(ToString::to_string),
            ),
            "case {case:?}: expected the stack to publish DynamoDB's own identifiers, got \
             {local}"
        );
        assert_eq!(
            output(&described, "TableArn"),
            Some(format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{name}")),
            "case {case:?}: expected the documented table ARN, got {local}"
        );

        // ...and the capacity each regional table reports is the one the declaration stated
        // for it: the group's write half, and that Region's own read half.
        for (region, read, write) in &row.provisioned {
            let reported = described_in(&router, region, &name).await;
            assert_eq!(
                (
                    reported["ProvisionedThroughput"]["ReadCapacityUnits"].clone(),
                    reported["ProvisionedThroughput"]["WriteCapacityUnits"].clone(),
                ),
                (json!(read), json!(write)),
                "case {case:?}: expected {region} provisioned for {read} read and {write} \
                 write units, got {reported}"
            );
        }

        // A setting the declaration states ONCE for the group is in force in every Region
        // of it, which only that Region's own read establishes.
        if let Some(attribute) = row.time_to_live {
            for region in std::iter::once(REGION).chain(row.peers.iter().copied()) {
                let reported = time_to_live_when_enabled(&router, case, region, &name).await;
                assert_eq!(
                    (
                        reported["TimeToLiveStatus"].clone(),
                        reported["AttributeName"].clone(),
                    ),
                    (json!("ENABLED"), json!(attribute)),
                    "case {case:?}: expected Time to Live on {attribute} in force in {region}, \
                     got {reported}"
                );
            }
        }
    }
}

/// Poll `DescribeTimeToLive` in `region` until the table reports the setting in force,
/// answering the description it reported then — the waiter a client runs for a setting
/// DynamoDB applies asynchronously ("it can take up to one hour for the change to fully
/// process").
async fn time_to_live_when_enabled(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    name: &str,
) -> Value {
    let deadline = std::time::Instant::now() + REPLICATED_DEADLINE;
    let request = json!({"TableName": name});
    loop {
        let (status, document) =
            dynamodb_in(router, region, "DescribeTimeToLive", &request).await;
        let reported = document["TimeToLiveDescription"].clone();
        if status == 200 && reported["TimeToLiveStatus"] == json!("ENABLED") {
            return reported;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: Time to Live on {name} in {region} never came into force within \
             {REPLICATED_DEADLINE:?}, last {status} with {document}"
        );
        tokio::time::sleep(REPLICATED_POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn deleting_the_stack_takes_every_regional_table_of_the_group_away() {
    let router = router();
    let case = "a two-Region group deleted with its stack";
    let stack = "ddb-global-deleted";
    let row = Row {
        case,
        stack,
        properties: json!({
            "TableName": "deleted-global-table",
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "Replicas": [{"Region": REGION}, {"Region": PEER_REGION}],
        }),
        declared_name: Some("deleted-global-table"),
        peers: vec![PEER_REGION],
        provisioned: Vec::new(),
        time_to_live: None,
    };
    create_stack(&router, stack, &template(&row), &[]).await;
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, because {:?}",
        failure_reasons(&router, stack).await
    );
    let name = "deleted-global-table";
    table_when(&router, case, PEER_REGION, name, |table| {
        table["TableStatus"] == json!("ACTIVE")
    })
    .await;

    call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    settled_stack(&router, case, stack).await;
    // "Deleting a stack deletes the resources in it": the group's tables are gone in every
    // Region it spanned, which only that service answering for neither establishes.
    for region in [REGION, PEER_REGION] {
        let gone = table_when(&router, case, region, name, |table| *table == Value::Null).await;
        assert_eq!(
            gone,
            Value::Null,
            "case {case:?}: expected DynamoDB to hold no table in {region} after the stack \
             was deleted, got {gone}"
        );
    }
}

/// One row of the refusal table: the case, the declared properties, and the sentence the
/// resource must fail with.
type RefusedRow = (&'static str, &'static str, Value, &'static str);

#[tokio::test]
async fn a_declaration_this_emulator_cannot_apply_fails_the_resource_saying_why() {
    // RULE: a declaration the provider cannot apply fails the resource with the reason,
    // rather than being reported created with less than it asked for.
    let rows: [RefusedRow; 5] = [
        (
            "no replica in the stack's own Region",
            "ddb-global-no-local",
            json!({
                "TableName": "no-local-replica",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "Replicas": [{"Region": PEER_REGION}],
            }),
            "Replicas must include a replica in the Region the stack is deployed in \
             (us-east-1)",
        ),
        (
            "one Region listed twice",
            "ddb-global-twice",
            json!({
                "TableName": "twice-listed-region",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "Replicas": [{"Region": REGION}, {"Region": REGION}],
            }),
            "Replicas names the Region us-east-1 more than once",
        ),
        (
            "a property this emulator does not apply",
            "ddb-global-unapplied",
            json!({
                "TableName": "unapplied-property",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "MultiRegionConsistency": "STRONG",
                "Replicas": [{"Region": REGION}, {"Region": PEER_REGION}],
            }),
            "this emulator does not apply the MultiRegionConsistency property of \
             AWS::DynamoDB::GlobalTable",
        ),
        (
            "a replica entry that names no Region",
            "ddb-global-unregioned",
            json!({
                "TableName": "unregioned-replica",
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "Replicas": [
                    {"Region": REGION},
                    {"ReadOnDemandThroughputSettings": {"MaxReadRequestUnits": 5}},
                ],
            }),
            "every entry of Replicas must state the Region it is for",
        ),
        (
            "an index member this emulator does not apply",
            "ddb-global-index-member",
            json!({
                "TableName": "unapplied-index-member",
                "AttributeDefinitions": [
                    {"AttributeName": "pk", "AttributeType": "S"},
                    {"AttributeName": "sk", "AttributeType": "S"},
                ],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "GlobalSecondaryIndexes": [{
                    "IndexName": "BySk",
                    "KeySchema": [{"AttributeName": "sk", "KeyType": "HASH"}],
                    "Projection": {"ProjectionType": "ALL"},
                    "ContributorInsightsSpecification": {"Enabled": true},
                }],
                "Replicas": [{"Region": REGION}, {"Region": PEER_REGION}],
            }),
            "this emulator does not apply the \
             GlobalSecondaryIndexes.ContributorInsightsSpecification property of \
             AWS::DynamoDB::GlobalTable",
        ),
    ];

    for (case, stack, properties, expected) in rows {
        let router = router();
        let row = Row {
            case,
            stack,
            properties,
            declared_name: None,
            peers: Vec::new(),
            provisioned: Vec::new(),
            time_to_live: None,
        };
        create_stack(&router, stack, &template(&row), &[]).await;
        let described = settled_stack(&router, case, stack).await;
        let status = xml_text(&described, "StackStatus").unwrap_or_default();
        let reasons = failure_reasons(&router, stack).await;
        assert!(
            status.starts_with("ROLLBACK_") || status == "CREATE_FAILED",
            "case {case:?}: expected the stack not to complete, got {status:?} because \
             {reasons:?}"
        );
        assert!(
            reasons.iter().any(|reason| reason.contains(expected)),
            "case {case:?}: expected a failure saying {expected:?}, got {reasons:?}"
        );
    }
}
