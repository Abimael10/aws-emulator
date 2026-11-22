// SPDX-License-Identifier: CC0-1.0

//! Every tag Amazon EC2 Auto Scaling reports for a group a CloudFormation stack created
//! carries the whole of a `TagDescription`.
//!
//! The RULE under test: a `TagDescription` of an Auto Scaling group carries `ResourceId`,
//! `ResourceType`, `Key`, `Value` and `PropagateAtLaunch` — the last of them "determines
//! whether the tag is added to new instances as they are launched in the group"
//! (`TagDescription$PropagateAtLaunch`) — and that flag is the SERVICE's own decision, taken
//! when the tag is stored rather than copied from the request, so every read of every tag of
//! a group reports it: the tags whose declaration states it and the tags nothing states it
//! for alike. Both reads that report a group's tags answer the same way — the account-wide
//! one (`DescribeTags.Tags[]`) and the group's own (`DescribeAutoScalingGroups`, which
//! carries "the tags for the Auto Scaling group") — so a client that indexes the member off
//! any tag of such a group always finds it.
//!
//! Why the suite lives here: a group a stack declares is tagged from TWO sides at once, and
//! only the assembled emulator has both. The declaration states a `PropagateAtLaunch` of its
//! own for each tag it writes, while CloudFormation ALSO propagates the stack's own tags and
//! the three records it keeps on every resource it provisions — tags no declaration states
//! that member for, which are exactly the ones a serializer that only echoed the request
//! would report without it.
//!
//! The rows span what the rule generalises over: the FORM the declaration states the member
//! in (a JSON Boolean, and the string a template parameter or a YAML scalar renders as), the
//! VALUE it states (both Booleans), the tag's own value (a value and the empty string, which
//! is a value a `TagValue` may be), and whether the member is stated for the tag at all —
//! with the tags CloudFormation adds by itself standing for "nothing states it".

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, form_encode, members_of, outputs_of};

/// The Region every request below is signed for, and the Availability Zone the group places
/// its capacity in — one of that Region's.
const REGION: &str = "us-east-1";
const ZONE: &str = "us-east-1a";

/// The API versions the two services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";

/// The stack under test, and the logical id it declares the group under.
const STACK: &str = "asg-tag-descriptions-stack";
const GROUP_LOGICAL_ID: &str = "Asg";

/// The AMI the launch template names, in Amazon EC2's own id form.
const IMAGE_ID: &str = "ami-00000000000000001";

/// The `ResourceType` every tag of an Auto Scaling group is reported under, from the model's
/// own documentation for `TagDescription$ResourceType`: "The only supported value is
/// `auto-scaling-group`". Spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make this test pass.
const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// The five members a `TagDescription` carries, from the model
/// (`smithy_query(query="shape",shape="TagDescription")`): the group the tag is on, the kind
/// of resource that is, the key, the value, and whether the tag reaches the instances the
/// group launches.
const TAG_DESCRIPTION_MEMBERS: &[&str] = &[
    "ResourceId",
    "ResourceType",
    "Key",
    "Value",
    "PropagateAtLaunch",
];

/// The tag the STACK is created with, which CloudFormation propagates to the resources it
/// provisions: a tag of the group that the group's own declaration says nothing about.
const STACK_TAG_KEY: &str = "stack-owner";
const STACK_TAG_VALUE: &str = "platform-engineering";

/// The three keys CloudFormation records on every resource it provisions — the stack's name,
/// the stack's id and the resource's logical id. They are tags of the group that no
/// declaration states a `PropagateAtLaunch` for either.
///
/// Source: the reserved `aws:cloudformation:` tag keys CloudFormation documents for the
/// resources a stack creates.
const PROVENANCE_KEYS: &[&str] = &[
    "aws:cloudformation:stack-name",
    "aws:cloudformation:stack-id",
    "aws:cloudformation:logical-id",
];

/// How a row's declaration states `PropagateAtLaunch` for its tag.
#[derive(Debug, Clone, Copy)]
enum Declared {
    /// The member is not stated at all, so the flag is the service's to decide.
    Nothing,
    /// The member as a JSON Boolean, which is how a JSON template spells one.
    Boolean(bool),
    /// The member as a STRING, which is what a template parameter, an intrinsic's result or a
    /// YAML scalar renders a Boolean as.
    Text(&'static str),
}

/// One tag the group's `Tags` property declares: its key, its value, how the declaration
/// states `PropagateAtLaunch`, and the flag every read of the tag must then report — `None`
/// where the declaration states none, so the flag is whichever Boolean the service decides
/// for a tag stored without the member.
struct Row {
    case: &'static str,
    key: &'static str,
    value: &'static str,
    declared: Declared,
    expected: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a declaration stating PropagateAtLaunch as the JSON Boolean true",
        key: "environment",
        value: "production",
        declared: Declared::Boolean(true),
        expected: Some("true"),
    },
    Row {
        case: "a declaration stating PropagateAtLaunch as the JSON Boolean false",
        key: "team",
        value: "platform",
        declared: Declared::Boolean(false),
        expected: Some("false"),
    },
    Row {
        case: "a declaration stating PropagateAtLaunch as the string a template renders a \
               Boolean as",
        key: "backup-window",
        value: "02:00-03:00",
        declared: Declared::Text("true"),
        expected: Some("true"),
    },
    Row {
        case: "a declaration stating PropagateAtLaunch=false for a tag whose value is the \
               empty string",
        key: "owner-note",
        value: "",
        declared: Declared::Text("false"),
        expected: Some("false"),
    },
    Row {
        case: "a declaration stating no PropagateAtLaunch at all",
        key: "cost-centre",
        value: "cc-1234",
        declared: Declared::Nothing,
        expected: None,
    },
];

/// How long a wait gives a state the clock owes (the stack's own status moves) and how often
/// it looks. Generous, and only here so a failure is a failure rather than a hang: the wait
/// is on the CONDITION, never on a time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it: a stack's Auto
/// Scaling group is created by CloudFormation calling Amazon EC2 Auto Scaling, which in turn
/// resolves the launch template in Amazon EC2.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// Call one query operation of `service`, answered with its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation operation that must succeed.
async fn cfn(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "cloudformation", CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "CloudFormation {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The template the stack declares: a launch template, and a group carrying the rows' tags.
///
/// The group's name is CloudFormation's to generate ("Ref returns the name of the Auto
/// Scaling group"), which the stack publishes as an output, so the reads below address the
/// group the way a client that deployed the stack does. It launches nothing (`MinSize` and
/// `DesiredCapacity` are zero), because the subject here is the group's tag set.
fn template() -> String {
    let declared: Vec<Value> = ROWS
        .iter()
        .map(|row| {
            let mut tag = json!({"Key": row.key, "Value": row.value});
            match row.declared {
                Declared::Nothing => {}
                Declared::Boolean(flag) => {
                    tag["PropagateAtLaunch"] = Value::Bool(flag);
                }
                Declared::Text(text) => {
                    tag["PropagateAtLaunch"] = Value::String(text.to_string());
                }
            }
            tag
        })
        .collect();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Lt": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateData": {"ImageId": IMAGE_ID, "InstanceType": "t3.micro"},
                },
            },
            GROUP_LOGICAL_ID: {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": {
                    "AvailabilityZones": [ZONE],
                    "MinSize": "0",
                    "MaxSize": "1",
                    "DesiredCapacity": "0",
                    "LaunchTemplate": {
                        "LaunchTemplateId": {"Ref": "Lt"},
                        "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
                    },
                    "Tags": Value::Array(declared),
                },
            },
        },
        "Outputs": {
            "GroupName": {"Value": {"Ref": GROUP_LOGICAL_ID}},
        },
    })
    .to_string()
}

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack settles, and answer the answer it settled in.
async fn settle(router: &MultiServiceRouter) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", STACK)]).await;
        let status = element(&body, "StackStatus");
        if settled(&status) {
            return body;
        }
        assert!(
            Instant::now() < giving_up_at,
            "the stack never left {status:?}: last answer {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The text one member of a reported tag carries, or `None` when the tag carries no such
/// member at all.
///
/// A member whose value is the empty string is PRESENT and empty — written either as a pair
/// of tags or as one self-closing tag — which is a different answer from a member the
/// serializer left out altogether, and telling those two apart is what this reader is for.
/// Read inside ONE tag's own block, where each member appears once.
fn tag_member(entry: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    if let Some(start) = entry.find(&open) {
        let start = start + open.len();
        let close = format!("</{name}>");
        let end = entry[start..].find(&close)? + start;
        return Some(entry[start..end].to_string());
    }
    [format!("<{name}/>"), format!("<{name} />")]
        .iter()
        .any(|empty| entry.contains(empty.as_str()))
        .then(String::new)
}

/// The tags one answer reports for `group`, as the key -> `PropagateAtLaunch` map a client
/// that reads the flag off each tag builds — asserting on the way that every reported tag
/// carries all five members of a `TagDescription`, that it is reported against this group and
/// under the one resource type this service tags, and that the flag is one of the two
/// spellings of a Boolean.
///
/// The `Tags` list is taken as a BLOCK first and its `<member>` entries read inside it: the
/// group's own read carries other `<member>` lists (its Availability Zones among them) that a
/// flat scan over the whole answer would sweep up.
fn reported_flags(case: &str, body: &str, group: &str) -> BTreeMap<String, String> {
    let list = block_of(body, "Tags").unwrap_or_default();
    let mut flags: BTreeMap<String, String> = BTreeMap::new();
    for entry in members_of(list) {
        let missing: Vec<&str> = TAG_DESCRIPTION_MEMBERS
            .iter()
            .copied()
            .filter(|member| tag_member(entry, member).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "case {case:?}: expected every reported tag to carry \
             {TAG_DESCRIPTION_MEMBERS:?}, got one missing {missing:?}: {entry}"
        );
        assert_eq!(
            tag_member(entry, "ResourceId").as_deref(),
            Some(group),
            "case {case:?}: expected every reported tag to be on {group:?}, got {entry}"
        );
        assert_eq!(
            tag_member(entry, "ResourceType").as_deref(),
            Some(GROUP_RESOURCE_TYPE),
            "case {case:?}: expected every reported tag to name the one resource type this \
             service tags, got {entry}"
        );
        let key = tag_member(entry, "Key").unwrap_or_default();
        let propagate = tag_member(entry, "PropagateAtLaunch").unwrap_or_default();
        assert!(
            propagate.parse::<bool>().is_ok(),
            "case {case:?}: expected the PropagateAtLaunch of {key:?} to be a Boolean, got \
             {propagate:?} in {entry}"
        );
        flags.insert(key, propagate);
    }
    flags
}

/// Every tag the group must be carrying once the stack has deployed: the ones its own
/// declaration states, the one the STACK was created with, and the three CloudFormation
/// records itself.
fn expected_keys() -> BTreeSet<String> {
    ROWS.iter()
        .map(|row| row.key.to_string())
        .chain(std::iter::once(STACK_TAG_KEY.to_string()))
        .chain(PROVENANCE_KEYS.iter().map(|key| (*key).to_string()))
        .collect()
}

/// The tags of the group no declaration states `PropagateAtLaunch` for: the row that states
/// none, the tag the stack itself was created with, and the three CloudFormation records —
/// the ones whose flag is the service's own decision.
fn keys_stating_no_flag() -> BTreeSet<String> {
    ROWS.iter()
        .filter(|row| row.expected.is_none())
        .map(|row| row.key.to_string())
        .chain(std::iter::once(STACK_TAG_KEY.to_string()))
        .chain(PROVENANCE_KEYS.iter().map(|key| (*key).to_string()))
        .collect()
}

#[tokio::test]
async fn every_tag_of_a_stack_created_group_reports_propagate_at_launch() {
    let router = router();

    // The stack deploys, carrying a tag of its own for CloudFormation to propagate to the
    // group beside the group's own declared tags.
    cfn(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template().as_str()),
            ("Tags.member.1.Key", STACK_TAG_KEY),
            ("Tags.member.1.Value", STACK_TAG_VALUE),
        ],
    )
    .await;
    let settled_body = settle(&router).await;
    let status = element(&settled_body, "StackStatus");
    assert_eq!(
        status, "CREATE_COMPLETE",
        "expected the stack to deploy, got {status:?} with the answer {settled_body}"
    );
    let group = outputs_of(&settled_body)
        .remove("GroupName")
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| panic!("the stack publishes its group's name: {settled_body}"));

    // The two reads that report a group's tags, each read the way a client reads it: the
    // account-wide one narrowed to this group, and the group's own.
    let account_wide = asg(
        &router,
        "DescribeTags",
        &[
            ("Filters.member.1.Name", "auto-scaling-group"),
            ("Filters.member.1.Values.member.1", group.as_str()),
        ],
    )
    .await;
    let own = asg(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group.as_str())],
    )
    .await;

    // One entry per read, so the two can be compared against each other afterwards.
    let mut answers: Vec<(&str, BTreeMap<String, String>)> = Vec::new();
    for (case, body) in [
        ("the account-wide read", &account_wide),
        ("the group's own read", &own),
    ] {
        let flags = reported_flags(case, body, &group);
        let reported: BTreeSet<String> = flags.keys().cloned().collect();
        assert_eq!(
            reported,
            expected_keys(),
            "case {case:?}: expected the group to carry the declared tags, the stack's own \
             and CloudFormation's three records, got {reported:?}"
        );

        // A tag whose declaration STATES the member reports exactly what it stated, whatever
        // form the declaration spelled it in.
        for row in ROWS {
            let Some(expected) = row.expected else {
                continue;
            };
            assert_eq!(
                flags.get(row.key).map(String::as_str),
                Some(expected),
                "case {case:?}, row {:?}: expected the tag {:?} (value {:?}) to report \
                 PropagateAtLaunch {expected:?}, got {:?}",
                row.case,
                row.key,
                row.value,
                flags.get(row.key)
            );
        }

        // A tag NOTHING states the member for still reports one, and the decision is the
        // service's own rather than the tag's: every such tag of the group reports the same
        // Boolean, whether it came from a declaration that left the member out or from
        // CloudFormation propagating a tag of its own.
        let decided: BTreeSet<&str> = keys_stating_no_flag()
            .iter()
            .filter_map(|key| flags.get(key).map(String::as_str))
            .collect();
        assert_eq!(
            decided.len(),
            1,
            "case {case:?}: expected the tags no declaration states PropagateAtLaunch for \
             ({:?}) to report ONE decision of the service's, got {decided:?} in {flags:?}",
            keys_stating_no_flag()
        );

        answers.push((case, flags));
    }

    // Both reads report the same tag the same way: the projection of a stored tag onto the
    // wire is one projection, so the group's own read and the account-wide read cannot
    // disagree about any tag's flag.
    let [(first_case, first), (second_case, second)] = answers.as_slice() else {
        panic!("both reads were read, got {} answer(s)", answers.len());
    };
    assert_eq!(
        first, second,
        "expected {first_case:?} and {second_case:?} to report every tag's \
         PropagateAtLaunch alike, got {first:?} and {second:?}"
    );
}
