// SPDX-License-Identifier: CC0-1.0

//! WHICH METADATA OPTIONS a caller may launch with — the condition-key half of the launch
//! authorization decision, driven end to end over the wire EC2 speaks (`ec2Query`) with the
//! role, its policy and its session held by the IAM and STS services mounted on the SAME
//! engine, so the document under test is the one `iam:PutRolePolicy` stored and the principal
//! a refusal blames is the one `sts:AssumeRole` minted.
//!
//! THE RULE UNDER TEST: EC2 takes the IAM decision about a request in the condition context
//! the REQUEST's own parameters answer, and a launch's metadata options are such parameters —
//! `MetadataOptions.HttpTokens` ("`required` - IMDSv2 is required, which means that IMDSv1 is
//! disabled", `InstanceMetadataOptionsRequest$HttpTokens`), `MetadataOptions.HttpEndpoint`
//! ("enables or disables the HTTP metadata endpoint on your instances") and
//! `MetadataOptions.HttpPutResponseHopLimit` ("the maximum number of hops that the metadata
//! token can travel") — so a document conditioned on one of those keys is DECIDED against
//! the value the launch asks for: a launch the documents in force then do not permit is
//! refused HTTP 403 `UnauthorizedOperation` with the fixed sentence that error class carries
//! ("if you have the required permissions, the error response is DryRunOperation. Otherwise,
//! it is UnauthorizedOperation", `RunInstancesRequest$DryRun`) and launches nothing, and only
//! a launch the clause admits is served — with the options it asked for readable back on the
//! instance it created.
//!
//! The rows span what the rule generalises over: which metadata-option key the document is
//! written over (the IMDSv2 requirement, the endpoint, the hop limit), how it is written (an
//! `Allow` the clause narrows, a `Deny` of everything the clause does not admit, an
//! unconditioned grant as the control), the operator (string equality, its negation, a
//! numeric bound driven at the bound, under it and past it), and which value the launch
//! carries for the key (the admitted one, another, none at all). No row is the
//! specification: each is one input along the rule.

use smithy_server_runtime::MultiServiceRouter;

use super::account_wide_read_authorization::{ec2_as, element, emulator, ACCOUNT_KEY_ID, REGION};
use super::denied_launch_probe_encoded_message::{assumed, role_with, sentence_and_blob};
use super::ec2_query::{elements, error_code, error_message};

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// The sentence EC2 opens an `UnauthorizedOperation` refusal with.
const NOT_AUTHORIZED_SENTENCE: &str = "You are not authorized to perform this operation.";

/// The IAM action a launch is judged by.
const LAUNCH_ACTION: &str = "ec2:RunInstances";

/// The `instance` resource a launch is judged on: the type with every id, the request naming
/// no instance that exists yet — and so the resource a refusal of a launch names.
fn instance_resource() -> String {
    ::engine::emulator_arn("ec2", REGION, "instance/*")
}

/// The clause a refusal reached by the absence of any allow closes with, read from the shared
/// wording every service that reaches that decision states.
fn no_allow() -> String {
    service_common::iam_authorization::no_identity_policy_allows(LAUNCH_ACTION)
}

/// The clause a refusal reached by an explicit `Deny` in an identity-based document closes
/// with, read from that same shared wording.
fn explicit_deny() -> String {
    service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
}

/// A one-statement document allowing the launch over every resource, under `condition`
/// (written as the `Condition` element's own JSON) when the row states one.
fn allow_launch(condition: Option<&str>) -> String {
    let condition = condition
        .map(|condition| format!(",\"Condition\":{condition}"))
        .unwrap_or_default();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":\"{LAUNCH_ACTION}\",\"Resource\":\"*\"{condition}}}]}}"
    )
}

/// A document allowing everything of this service and DENYING the launch under `condition`:
/// the same guardrail written from the other side.
fn deny_launch_under(condition: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Effect\":\"Allow\",\"Action\":\"ec2:*\",\"Resource\":\"*\"}},\
         {{\"Effect\":\"Deny\",\"Action\":\"{LAUNCH_ACTION}\",\"Resource\":\"*\",\
         \"Condition\":{condition}}}]}}"
    )
}

/// An image id of the account a launch can name, read back over the wire from the account's
/// own credential rather than written here.
async fn image_id(router: &MultiServiceRouter) -> String {
    let (status, images) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "the published images must be readable, got {status} {images}"
    );
    elements(&images, "imageId")
        .into_iter()
        .next()
        .expect("a fresh account publishes at least one image")
}

/// Every instance id the account holds, read back through `DescribeInstances`.
async fn instance_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, instances) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    elements(&instances, "instanceId")
}

/// What a row expects one launch to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the documents in force admit the metadata options the launch asks for.
    Served,
    /// HTTP 403 `UnauthorizedOperation` because no document allows the launch under the
    /// options it asks for.
    NoAllow,
    /// HTTP 403 `UnauthorizedOperation` because a statement about the request says `Deny`.
    ExplicitDeny,
}

/// One launch a row drives: the metadata-option members it sets, and what it must answer.
struct Launch {
    /// The `MetadataOptions` members the launch carries, by the member's own name.
    options: &'static [(&'static str, &'static str)],
    expect: Expect,
}

/// One row: the document in force on a role of its own, and the launches it decides.
struct Row {
    case: &'static str,
    role: &'static str,
    document: String,
    launches: Vec<Launch>,
}

/// The form fields of a launch carrying `options` as its `MetadataOptions`.
fn launch_params<'a>(
    image_id: &'a str,
    options: &'a [(&'a str, &'a str)],
) -> Vec<(String, &'a str)> {
    let mut params: Vec<(String, &str)> = vec![
        ("ImageId".to_string(), image_id),
        ("MinCount".to_string(), "1"),
        ("MaxCount".to_string(), "1"),
    ];
    for (member, value) in options {
        params.push((format!("MetadataOptions.{member}"), value));
    }
    params
}

/// The XML element each metadata-option member is reported back under on an instance
/// (`InstanceMetadataOptionsResponse`'s `@xmlName`s,
/// `smithy_query(query="shape",shape="InstanceMetadataOptionsResponse")`).
fn reported_element(member: &str) -> &'static str {
    match member {
        "HttpTokens" => "httpTokens",
        "HttpEndpoint" => "httpEndpoint",
        "HttpPutResponseHopLimit" => "httpPutResponseHopLimit",
        other => panic!("no reported member for {other:?}"),
    }
}

/// Drive one launch of `row` as the session `akid` speaks for and assert what it answered —
/// and, either way, what it left behind: a refused launch creates nothing, and a served one
/// is readable back with the very options it asked for.
async fn assert_launch(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    image_id: &str,
    launch: &Launch,
) {
    let before = instance_ids(router, case).await;
    let params = launch_params(image_id, launch.options);
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    let (status, body) = ec2_as(router, akid, "RunInstances", &borrowed).await;
    match launch.expect {
        Expect::Served => {
            assert_eq!(
                status, 200,
                "case {case:?} / {:?}: the admitted launch must be served, got {status} {body}",
                launch.options
            );
            let instance_id = element(case, &body, "instanceId");
            // The options it asked for are the ones the instance holds: read back through the
            // operation that reports them.
            let (status, described) = ec2_as(
                router,
                ACCOUNT_KEY_ID,
                "DescribeInstances",
                &[("InstanceId.1", instance_id.as_str())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} / {:?}: the read back must be served, got {status} {described}",
                launch.options
            );
            for (member, value) in launch.options {
                let reported = element(case, &described, reported_element(member));
                assert_eq!(
                    reported, *value,
                    "case {case:?} / {:?}: expected {member} {value:?} on the launched \
                     instance, got {reported:?}",
                    launch.options
                );
            }
            let after = instance_ids(router, case).await;
            assert_eq!(
                after.len(),
                before.len() + 1,
                "case {case:?} / {:?}: expected exactly the launched instance, got {after:?}",
                launch.options
            );
        }
        Expect::NoAllow | Expect::ExplicitDeny => {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, UNAUTHORIZED_OPERATION),
                "case {case:?} / {:?}: expected the unauthorized refusal, got {status} {body}",
                launch.options
            );
            // A refusal of a launch hands the caller an encoded authorization failure
            // message, so the sentence under test is what precedes the blob.
            let (sentence, _) = sentence_and_blob(case, &error_message(&body));
            let reason = if launch.expect == Expect::ExplicitDeny {
                explicit_deny()
            } else {
                no_allow()
            };
            assert_eq!(
                sentence,
                format!(
                    "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to perform: \
                     {LAUNCH_ACTION} on resource: {} {reason}.",
                    instance_resource()
                ),
                "case {case:?} / {:?}: expected IAM's refusal wording, got {body}",
                launch.options
            );
            let after = instance_ids(router, case).await;
            assert_eq!(
                after, before,
                "case {case:?} / {:?}: a refused launch must create nothing, got {after:?}",
                launch.options
            );
        }
    }
}

/// RULE: a launch is decided over the condition keys its own metadata options answer, so a
/// document written over one of those keys admits exactly the launches whose options satisfy
/// it and the rest are refused `UnauthorizedOperation` with nothing created.
#[tokio::test]
async fn a_launch_is_decided_by_the_metadata_options_the_policy_admits() {
    let router = emulator();
    let image_id = image_id(&router).await;
    let rows = vec![
        Row {
            case: "a grant admitting only IMDSv2-enforced launches",
            role: "imdsv2-enforced-role",
            document: allow_launch(Some(
                "{\"StringEquals\":{\"ec2:MetadataHttpTokens\":\"required\"}}",
            )),
            launches: vec![
                Launch {
                    options: &[("HttpTokens", "required")],
                    expect: Expect::Served,
                },
                Launch {
                    options: &[("HttpTokens", "optional")],
                    expect: Expect::NoAllow,
                },
                // The launch carries no value for the key at all, so the clause is not
                // satisfied and the `Allow` it narrows grants nothing.
                Launch {
                    options: &[],
                    expect: Expect::NoAllow,
                },
            ],
        },
        Row {
            case: "the same guardrail written as a deny of every other launch",
            role: "imdsv2-deny-written-role",
            document: deny_launch_under(
                "{\"StringNotEquals\":{\"ec2:MetadataHttpTokens\":\"required\"}}",
            ),
            launches: vec![
                Launch {
                    options: &[("HttpTokens", "required")],
                    expect: Expect::Served,
                },
                Launch {
                    options: &[("HttpTokens", "optional")],
                    expect: Expect::ExplicitDeny,
                },
            ],
        },
        Row {
            case: "a grant keyed on the metadata endpoint the launch leaves reachable",
            role: "metadata-endpoint-role",
            document: allow_launch(Some(
                "{\"StringEquals\":{\"ec2:MetadataHttpEndpoint\":\"disabled\"}}",
            )),
            launches: vec![
                Launch {
                    options: &[("HttpEndpoint", "disabled")],
                    expect: Expect::Served,
                },
                Launch {
                    options: &[("HttpEndpoint", "enabled")],
                    expect: Expect::NoAllow,
                },
            ],
        },
        Row {
            case: "a grant bounding how far the metadata token may travel",
            role: "metadata-hop-limit-role",
            document: allow_launch(Some(
                "{\"NumericLessThanEquals\":{\"ec2:MetadataHttpPutResponseHopLimit\":\"2\"}}",
            )),
            launches: vec![
                Launch {
                    options: &[("HttpPutResponseHopLimit", "1")],
                    expect: Expect::Served,
                },
                // The bound itself, which a less-than-or-equal clause admits.
                Launch {
                    options: &[("HttpPutResponseHopLimit", "2")],
                    expect: Expect::Served,
                },
                Launch {
                    options: &[("HttpPutResponseHopLimit", "3")],
                    expect: Expect::NoAllow,
                },
            ],
        },
        Row {
            // The control: the refusals above are the clause's doing, not the launch's.
            case: "an unconditioned grant of the same launch",
            role: "unconditioned-launcher-role",
            document: allow_launch(None),
            launches: vec![Launch {
                options: &[("HttpTokens", "optional")],
                expect: Expect::Served,
            }],
        },
    ];

    for row in &rows {
        role_with(&router, row.case, row.role, &row.document).await;
        let (akid, principal, _) =
            assumed(&router, row.case, row.role, "metadata-launcher", None).await;
        for launch in &row.launches {
            assert_launch(
                &router,
                row.case,
                &akid,
                &principal,
                &image_id,
                launch,
            )
            .await;
        }
    }
}
