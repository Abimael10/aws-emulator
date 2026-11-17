// SPDX-License-Identifier: CC0-1.0

//! The unit tests of `ops::put_events`' own helpers: which invocation a target
//! ARN names, and the values a delivery renders from an event.

use super::{invoked_under_target_role, kinesis_partition_key, sqs_queue, TargetKind};

/// One row of the table below: the case, the target's ARN, the kind that ARN
/// names, the IAM action its invocation performs, and whether a target of
/// that ARN's service is invoked under the target's own role.
type Row = (&'static str, String, Option<TargetKind>, Option<&'static str>, bool);

/// Which invocation a target ARN names, the IAM action that invocation
/// performs, and whether the target's `RoleArn` is what governs it — over a
/// TABLE of ARNs rather than one target: the rows differ along the service,
/// along the resource type within a service, and along the forms that name no
/// invocable resource at all.
///
/// The kinds, and the action each needs, are the ones the EventBridge
/// targets reference states for them: `sqs:SendMessage` for a queue,
/// `lambda:InvokeFunction` for a function, `sns:Publish` for a topic,
/// `states:StartExecution` for a state machine, `kinesis:PutRecord` for a
/// data stream, `firehose:PutRecord` for a delivery stream and
/// `glue:notifyEvent` for a workflow. Which of them is
/// invoked under the target's role is that reference's other sentence: a role
/// for a state machine, a Kinesis data stream, a Firehose delivery stream and
/// a Glue workflow, a resource-based policy for a queue, a topic and a
/// function. That half is a question about the target's SERVICE rather than
/// about the resource it names — the role is assumed before the call is made
/// — so it is answered for an ARN naming no invocable resource too, and only
/// an ARN naming no service at all is governed by no role.
#[test]
fn a_target_arn_names_the_invocation_its_service_is_reached_by() {
    let account = ::engine::emulator_account_id();
    // (case, the target's ARN, the kind it names, the action it performs,
    //  whether a target of its service is invoked under the target's role)
    let rows: Vec<Row> = vec![
        (
            "an SQS queue",
            format!("arn:aws:sqs:eu-west-2:{account}:orders"),
            Some(TargetKind::Queue),
            Some("sqs:SendMessage"),
            false,
        ),
        (
            "a FIFO queue, whose name carries a suffix",
            format!("arn:aws:sqs:eu-west-2:{account}:orders.fifo"),
            Some(TargetKind::Queue),
            Some("sqs:SendMessage"),
            false,
        ),
        (
            "a Lambda function",
            format!("arn:aws:lambda:eu-west-2:{account}:function:handler"),
            Some(TargetKind::Function),
            Some("lambda:InvokeFunction"),
            false,
        ),
        (
            "a Lambda function alias, which is still a function",
            format!("arn:aws:lambda:eu-west-2:{account}:function:handler:live"),
            Some(TargetKind::Function),
            Some("lambda:InvokeFunction"),
            false,
        ),
        (
            "an SNS topic",
            format!("arn:aws:sns:eu-west-2:{account}:fanout"),
            Some(TargetKind::Topic),
            Some("sns:Publish"),
            false,
        ),
        (
            "a Step Functions state machine",
            format!("arn:aws:states:eu-west-2:{account}:stateMachine:flow"),
            Some(TargetKind::StateMachine),
            Some("states:StartExecution"),
            true,
        ),
        (
            "a Kinesis data stream",
            format!("arn:aws:kinesis:eu-west-2:{account}:stream/telemetry"),
            Some(TargetKind::Stream),
            Some("kinesis:PutRecord"),
            true,
        ),
        (
            "a Glue workflow",
            format!("arn:aws:glue:eu-west-2:{account}:workflow/nightly"),
            Some(TargetKind::Workflow),
            Some("glue:notifyEvent"),
            true,
        ),
        // A resource WITHIN a workflow rather than the workflow: a run is
        // not something an event is delivered to.
        (
            "a Glue workflow run",
            format!("arn:aws:glue:eu-west-2:{account}:workflow/nightly/run/wr_1"),
            None,
            None,
            true,
        ),
        // A resource of the same service that is not a workflow at all.
        (
            "a Glue table",
            format!("arn:aws:glue:eu-west-2:{account}:table/sales/orders"),
            None,
            None,
            true,
        ),
        // A resource WITHIN a stream rather than the stream: a consumer is
        // not something a record is written to.
        (
            "a Kinesis stream consumer",
            format!("arn:aws:kinesis:eu-west-2:{account}:stream/telemetry/consumer/reader"),
            None,
            None,
            true,
        ),
        // A resource of a service this invokes, but not one it can invoke.
        (
            "a Step Functions activity",
            format!("arn:aws:states:eu-west-2:{account}:activity:worker"),
            None,
            None,
            true,
        ),
        (
            "a Lambda layer",
            format!("arn:aws:lambda:eu-west-2:{account}:layer:shared"),
            None,
            None,
            false,
        ),
        (
            "a CodePipeline pipeline",
            format!("arn:aws:codepipeline:eu-west-2:{account}:orders-pipeline"),
            Some(TargetKind::Pipeline),
            Some("codepipeline:StartPipelineExecution"),
            true,
        ),
        // Resources of that same service that are NOT a pipeline: each is keyed by a
        // compound resource field, so no execution is started for it.
        (
            "a CodePipeline webhook",
            format!("arn:aws:codepipeline:eu-west-2:{account}:webhook:on-push"),
            None,
            None,
            true,
        ),
        (
            "a CodePipeline custom action type",
            format!("arn:aws:codepipeline:eu-west-2:{account}:actiontype:Build/Custom/1"),
            None,
            None,
            true,
        ),
        // A CloudWatch Logs log group, in both forms of its ARN: the group
        // alone and the group with everything under it, which is the ARN a
        // client reads off `DescribeLogGroups`.
        (
            "a CloudWatch log group",
            format!("arn:aws:logs:eu-west-2:{account}:log-group:/demo:*"),
            Some(TargetKind::LogGroup),
            Some("logs:PutLogEvents"),
            false,
        ),
        (
            "a CloudWatch log group named on its own",
            format!("arn:aws:logs:eu-west-2:{account}:log-group:/demo"),
            Some(TargetKind::LogGroup),
            Some("logs:PutLogEvents"),
            false,
        ),
        // A resource WITHIN a group is not the group, so no log event is
        // written for it.
        (
            "a CloudWatch log stream",
            format!("arn:aws:logs:eu-west-2:{account}:log-group:/demo:log-stream:one"),
            None,
            None,
            false,
        ),
        // An Amazon Data Firehose delivery stream, which a record is put
        // into, and a resource field that names no stream at all.
        (
            "a Firehose delivery stream",
            format!("arn:aws:firehose:eu-west-2:{account}:deliverystream/orders"),
            Some(TargetKind::DeliveryStream),
            Some("firehose:PutRecord"),
            true,
        ),
        (
            "a Firehose resource that is not a delivery stream",
            format!("arn:aws:firehose:eu-west-2:{account}:deliverystream"),
            None,
            None,
            true,
        ),
        // Services with no invocation here at all.
        (
            "an ECS task definition",
            format!("arn:aws:ecs:eu-west-2:{account}:task-definition/batch:1"),
            None,
            None,
            true,
        ),
        (
            "a value that is not an ARN",
            "orders".to_string(),
            None,
            None,
            false,
        ),
    ];

    for (case, arn, expected, action, under_role) in rows {
        let kind = TargetKind::of(&arn);
        assert_eq!(
            kind, expected,
            "case {case:?}: expected {expected:?}, got {kind:?}"
        );
        assert_eq!(
            kind.map(TargetKind::action),
            action,
            "case {case:?}: expected the action {action:?}, got {:?}",
            kind.map(TargetKind::action)
        );
        let governed = invoked_under_target_role(&arn);
        assert_eq!(
            governed, under_role,
            "case {case:?}: expected invoked-under-role {under_role}, got {governed}"
        );
    }
}

/// The url a queue ARN is addressed by, and the forms that address none.
#[test]
fn a_queue_arn_resolves_to_the_url_sqs_addresses_it_by() {
    let account = ::engine::emulator_account_id();
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "a queue of this account",
            format!("arn:aws:sqs:eu-west-2:{account}:orders"),
            Some(format!("https://sqs.eu-west-2.amazonaws.com/{account}/orders")),
        ),
        (
            "a queue in another Region",
            format!("arn:aws:sqs:us-east-1:{account}:orders"),
            Some(format!("https://sqs.us-east-1.amazonaws.com/{account}/orders")),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:sns:eu-west-2:{account}:orders"),
            None,
        ),
        (
            "an SQS ARN whose resource carries a separator",
            format!("arn:aws:sqs:eu-west-2:{account}:orders/dead"),
            None,
        ),
        ("a value that is not an ARN", "orders".to_string(), None),
    ];
    for (case, arn, expected) in rows {
        let url = sqs_queue(&arn);
        assert_eq!(
            url, expected,
            "case {case:?}: expected {expected:?}, got {url:?}"
        );
    }
}

/// An entry's `Detail` nests as deep as its CONTAINERS go: a container
/// delimiter inside a JSON string is text, not structure, so it does not
/// count — which is what lets the published nesting bound be charged on the
/// document before anything parses it. The rows differ along where the
/// delimiters sit, how the string around them is escaped, and whether the
/// deepest nesting is the first one reached.
#[test]
fn a_detail_nests_as_deep_as_its_containers_and_no_deeper() {
    // (case, the document, the nesting depth it has)
    let rows: Vec<(&str, &str, usize)> = vec![
        ("no container at all", "7", 0),
        ("a flat object", r#"{"a":1}"#, 1),
        ("an object inside an object", r#"{"a":{"b":1}}"#, 2),
        ("an array inside an object", r#"{"a":[1,2]}"#, 2),
        ("arrays inside an array", r#"{"a":[[[1]]]}"#, 4),
        ("delimiters inside a string value", r#"{"a":"{{{[[["}"#, 1),
        ("an escaped quote before a delimiter", r#"{"a":"\"{"}"#, 1),
        ("an escaped backslash ending the string", r#"{"a":"\\","b":{"c":1}}"#, 2),
        ("the deepest of two siblings", r#"{"a":{"b":1},"c":{"d":{"e":1}}}"#, 3),
        ("a delimiter in a KEY", r#"{"{a}":{"b":1}}"#, 2),
    ];
    for (case, document, depth) in rows {
        let measured = crate::state::json_nesting_depth(document);
        assert_eq!(
            measured, depth,
            "case {case:?}: expected a nesting depth of {depth}, got {measured}"
        );
    }
}

/// A `Detail` anywhere within the published nesting bound is JUDGED without
/// descending into it: the depth charge and the form check cost no stack, so
/// an entry is screened on the thread its request arrived on however deeply
/// it nests, and a document past the bound is refused without being read.
/// (Building the parse tree is the read that does descend, and it is reached
/// only for a document this has bounded.)
#[test]
fn a_detail_is_judged_without_descending_into_it() {
    // A stack far smaller than any thread this emulator serves on, so the
    // judgement is shown to cost no depth rather than to fit a large one.
    const SMALL_STACK_BYTES: usize = 256 * 1024;
    let deepest = crate::state::MAX_DETAIL_DEPTH;
    let mut document = "1".to_string();
    for _ in 0..deepest {
        document = format!(r#"{{"a":{document}}}"#);
    }
    // One level past the bound, which must be refused rather than read.
    let over = format!(r#"{{"a":{document}}}"#);
    let judged = ::std::thread::Builder::new()
        .stack_size(SMALL_STACK_BYTES)
        .spawn(move || {
            (
                crate::state::validate_detail(&document),
                crate::state::validate_detail(&over),
            )
        })
        .expect("the probe thread starts")
        .join()
        .expect("the probe thread finishes");
    assert_eq!(
        judged,
        (
            Ok(()),
            Err(crate::state::DetailRefusal::TooDeep(deepest + 1))
        ),
        "a Detail at and past {deepest} levels must be judged on a \
             {SMALL_STACK_BYTES}-byte stack"
    );
}

/// The partition key a Kinesis target's record is written under, for the
/// cases the wire-level table in `tests/all/kinesis_target_delivery.rs` does
/// NOT reach: the value types a delivered event's detail can hold beyond a
/// string, a number and an object (a whole array, a boolean, an explicit
/// null), a path whose own prefix names no container, a union of member
/// names, and a path past the length the shared reading of a path evaluates.
/// Every case the wire table already pins is left to it rather than restated
/// here.
///
/// The rule: the key is the JSON text of the value
/// `KinesisParameters$PartitionKeyPath` selects from the delivered text, and
/// the empty JSON object when it selects no single value.
#[test]
fn a_kinesis_targets_record_is_keyed_on_what_its_path_selects() {
    const EVENT_ID: &str = "11111111-2222-3333-4444-555555555555";
    // Past the length the shared reading of a path evaluates (the wrapper
    // `$['…']` adds five bytes to the name), so this row sits outside that
    // bound rather than merely being long.
    let long_path = format!(
        "$['{}']",
        "a".repeat(::service_common::json_reference_path::MAX_PATH_BYTES)
    );
    let delivered = ::serde_json::json!({
        "id": EVENT_ID,
        "source": "com.demo.orders",
        "detail": {
            "n": 7,
            "tenant": "tenant-alpha",
            "list": [1, 2],
            "flag": true,
            "nothing": ::serde_json::Value::Null,
        },
    })
    .to_string();
    // (case, the target's PartitionKeyPath, the key the record carries)
    let rows: [(&str, &str, &str); 6] = [
        ("a whole array", "$.detail.list", "[1,2]"),
        ("a boolean", "$.detail.flag", "true"),
        ("an explicit null", "$.detail.nothing", "null"),
        ("a path under no container", "$.detail.n.deeper", "{}"),
        // A union asks for a node LIST rather than one place, which this
        // emulator does not follow: it selects nothing rather than being
        // evaluated against the whole document.
        ("a union of members", "$['n','tenant']", "{}"),
        (
            "a path longer than the shared reading evaluates",
            long_path.as_str(),
            "{}",
        ),
    ];
    for (case, path, expected) in rows {
        let key = kinesis_partition_key(Some(path), EVENT_ID, &delivered);
        assert_eq!(
            key, expected,
            "case {case:?}: expected the partition key {expected:?}, got {key:?}"
        );
    }
}
