// SPDX-License-Identifier: CC0-1.0

//! The `S3` event source a SAM function declares, as the deployed bucket really carries it.
//!
//! The rule under test: **a template whose `AWS::Serverless::Function` declares an `S3` event
//! source leaves the bucket that source names carrying one `LambdaConfigurations` entry per
//! event type it lists — naming that function and carrying the key-name `Filter` the source
//! states — beside the `AWS::Lambda::Permission` that admits the Amazon S3 service principal,
//! with the bucket created after that grant.** The event source becomes no resource of its
//! own: "Bucket: S3 bucket name. This bucket must exist in the same template" (the source's own
//! `Bucket` property), and what makes a bucket publish to a function is that bucket's own
//! `NotificationConfiguration`, so the transform has to write it there.
//!
//! The configuration is read back through AMAZON S3's own API, on the bucket the deployed stack
//! published, because that is where a client reads it and the only place that says the wiring
//! really exists — a transform that expanded nothing deploys a bucket, a function and a
//! `CREATE_COMPLETE` stack exactly as this one does, and publishes nothing.
//!
//! The rows differ along what the rule generalises over: how many event types the source lists
//! and in which form (one value, a list of two), whether it states a key-name filter, and
//! whether the bucket already declares a configuration of its own for the entries to be added
//! beside.

use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, describe_stack, member, router, wait_for_the_transition,
    xml_blocks, xml_text, xml_texts,
};
use crate::stack_resource_provisioning::{output_value, s3};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The logical id every row's function is declared under, and the name of its event source —
/// the two the generated permission's own logical id is built from.
const FUNCTION_ID: &str = "Processor";
const EVENT_ID: &str = "ObjectCreated";

/// The handler every row deploys. What it does is beside the point here: what is under test is
/// the wiring the transform writes, read off the bucket.
const HANDLER: &str = "def handler(event, context):\\n    return {}\\n";

/// One destination configuration a row expects the deployed bucket to carry: the events it is
/// notified about, and the `(Name, Value)` filter rules it carries — an empty slice being a
/// configuration with no `Filter` at all.
struct Expected<'a> {
    events: &'a [&'a str],
    rules: &'a [(&'a str, &'a str)],
}

/// One row: the event source the function declares, the configuration the bucket declares for
/// itself, and the configurations the bucket must report once the stack is deployed.
struct Case<'a> {
    case: &'a str,
    stack: &'a str,
    bucket: &'a str,
    /// The `Events` member of the event source, as JSON: one value or a list of them.
    events: &'a str,
    /// The `Filter` member of the event source, as JSON, or `None` where it states none.
    filter: Option<&'a str>,
    /// The `NotificationConfiguration` the bucket itself declares, as JSON, or `None`.
    declared: Option<&'a str>,
    expected: &'a [Expected<'a>],
}

/// The template one row submits: the bucket, and the function whose `Events` name an `S3`
/// source on it.
fn template(case: &Case<'_>) -> String {
    let filter = case
        .filter
        .map(|filter| format!(r#","Filter":{filter}"#))
        .unwrap_or_default();
    let declared = case
        .declared
        .map(|configuration| format!(r#","NotificationConfiguration":{configuration}"#))
        .unwrap_or_default();
    format!(
        r#"{{"Transform":"AWS::Serverless-2016-10-31","Resources":{{
             "Data":{{"Type":"AWS::S3::Bucket","Properties":{{
                "BucketName":"{bucket}"{declared}}}}},
             "{FUNCTION_ID}":{{"Type":"AWS::Serverless::Function","Properties":{{
                "Handler":"index.handler","Runtime":"python3.12",
                "InlineCode":"{HANDLER}",
                "Events":{{"{EVENT_ID}":{{"Type":"S3","Properties":{{
                   "Bucket":{{"Ref":"Data"}},"Events":{events}{filter}}}}}}}}}}}}},
           "Outputs":{{
             "BucketName":{{"Value":{{"Ref":"Data"}}}},
             "FunctionArn":{{"Value":{{"Fn::GetAtt":["{FUNCTION_ID}","Arn"]}}}}}}}}"#,
        bucket = case.bucket,
        events = case.events,
    )
}

/// `GET /<bucket>?notification` as a client makes it.
async fn bucket_notifications(router: &MultiServiceRouter, bucket: &str) -> (u16, String) {
    let answer = s3(router, "GET", &format!("/{bucket}?notification"), b"").await;
    (answer.status, answer.body)
}

/// One function configuration as Amazon S3 reports it: the destination it names, the events it
/// is notified about, and the `(Name, Value)` filter rules it carries.
type Reported = (String, Vec<String>, Vec<(String, String)>);

/// The function configurations Amazon S3 reports for the bucket: the destination each names,
/// the events it is notified about, and the filter rules it carries.
fn installed(xml: &str) -> Vec<Reported> {
    xml_blocks(xml, "CloudFunctionConfiguration")
        .iter()
        .map(|entry| {
            let rules = xml_blocks(entry, "FilterRule")
                .iter()
                .map(|rule| {
                    (
                        xml_text(rule, "Name").unwrap_or_default(),
                        xml_text(rule, "Value").unwrap_or_default(),
                    )
                })
                .collect();
            (
                xml_text(entry, "CloudFunction").unwrap_or_default(),
                xml_texts(entry, "Event"),
                rules,
            )
        })
        .collect()
}

/// Every row: the deployed bucket carries the configurations the event source states, and the
/// stack holds the grant the transform synthesized for it.
#[tokio::test]
async fn a_sam_object_event_source_is_installed_on_the_bucket_it_names() {
    let cases: &[Case<'_>] = &[
        Case {
            case: "one event type, with a prefix filter",
            stack: "sam-object-event-prefix",
            bucket: "sam-object-event-prefix-data",
            events: r#""s3:ObjectCreated:*""#,
            filter: Some(r#"{"S3Key":{"Rules":[{"Name":"prefix","Value":"incoming/"}]}}"#),
            declared: None,
            expected: &[Expected {
                events: &["s3:ObjectCreated:*"],
                rules: &[("prefix", "incoming/")],
            }],
        },
        Case {
            case: "two event types stated as a list, with no filter at all",
            stack: "sam-object-event-pair",
            bucket: "sam-object-event-pair-data",
            events: r#"["s3:ObjectCreated:Put","s3:ObjectRemoved:Delete"]"#,
            filter: None,
            declared: None,
            expected: &[
                Expected {
                    events: &["s3:ObjectCreated:Put"],
                    rules: &[],
                },
                Expected {
                    events: &["s3:ObjectRemoved:Delete"],
                    rules: &[],
                },
            ],
        },
        Case {
            case: "a bucket that already declares a configuration of its own",
            stack: "sam-object-event-beside",
            bucket: "sam-object-event-beside-data",
            events: r#""s3:ObjectCreated:*""#,
            filter: Some(r#"{"S3Key":{"Rules":[{"Name":"suffix","Value":".csv"}]}}"#),
            declared: Some(
                r#"{"LambdaConfigurations":[{"Event":"s3:ObjectRemoved:*",
                     "Function":{"Fn::GetAtt":["Processor","Arn"]}}]}"#,
            ),
            expected: &[
                Expected {
                    events: &["s3:ObjectRemoved:*"],
                    rules: &[],
                },
                Expected {
                    events: &["s3:ObjectCreated:*"],
                    rules: &[("suffix", ".csv")],
                },
            ],
        },
    ];

    for case in cases {
        let name = case.case;
        let router = router();
        let body = template(case);
        let (status, created) = create_stack(
            &router,
            case.stack,
            &body,
            // The template declares a macro, which is acknowledged with
            // CAPABILITY_AUTO_EXPAND, and the role the transform synthesizes for the
            // function with the IAM capability.
            &acknowledging_iam(&[member("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND")]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: CreateStack must answer 200, got {status} {created}"
        );
        wait_for_the_transition().await;

        let (status, described) = describe_stack(&router, case.stack).await;
        assert_eq!(
            status, 200,
            "case {name:?}: DescribeStacks must answer 200, got {status} {described}"
        );
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {name:?}: the stack must reach CREATE_COMPLETE, got {described}"
        );

        // The grant the transform synthesized for the event source, under the logical id the
        // function's own id and the event's name make.
        let (status, resources) = crate::stack_lifecycle::call(
            &router,
            "DescribeStackResources",
            &[member("StackName", case.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: DescribeStackResources must answer 200, got {status} {resources}"
        );
        let permission = format!("{FUNCTION_ID}{EVENT_ID}Permission");
        assert!(
            xml_texts(&resources, "LogicalResourceId").contains(&permission),
            "case {name:?}: the stack must hold the grant {permission:?} the event source \
             needs, got {resources}"
        );
        assert!(
            xml_texts(&resources, "ResourceType").contains(&"AWS::Lambda::Permission".to_string()),
            "case {name:?}: that grant must be an AWS::Lambda::Permission, got {resources}"
        );

        // And what the BUCKET carries, read through Amazon S3's own API.
        let bucket = output_value(&described, "BucketName");
        let function = output_value(&described, "FunctionArn");
        assert!(
            !bucket.is_empty() && !function.is_empty(),
            "case {name:?}: the stack must publish the bucket and the function, got {described}"
        );
        let (status, answer) = bucket_notifications(&router, &bucket).await;
        assert_eq!(
            status, 200,
            "case {name:?}: GetBucketNotificationConfiguration({bucket:?}) must answer 200, got \
             {status} {answer}"
        );
        let reported = installed(&answer);
        let wanted: Vec<Reported> = case
            .expected
            .iter()
            .map(|configuration| {
                (
                    function.clone(),
                    configuration
                        .events
                        .iter()
                        .map(|event| (*event).to_string())
                        .collect(),
                    configuration
                        .rules
                        .iter()
                        .map(|(rule, value)| {
                            // "prefix" and "suffix" are reported under the spelling Amazon S3
                            // canonicalises them to, which is the one reading of that rule
                            // this tree states.
                            let reported =
                                service_com_amazonaws_s3_amazons3::state::canonical_filter_rule_name(
                                    rule,
                                )
                                .map_or_else(|| (*rule).to_string(), str::to_string);
                            (reported, (*value).to_string())
                        })
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            reported, wanted,
            "case {name:?}: the bucket {bucket:?} must report {wanted:?}, got {reported:?} \
             ({answer})"
        );
    }
}
