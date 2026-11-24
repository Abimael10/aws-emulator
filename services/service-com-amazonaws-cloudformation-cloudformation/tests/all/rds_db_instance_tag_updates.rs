// SPDX-License-Identifier: CC0-1.0

//! The TAGS of an `AWS::RDS::DBInstance` a stack deployed, across the stack UPDATE that
//! declares a different set of them.
//!
//! RULE under test: **a resource's tag set is carried by no configuration-update call of the
//! owning service, so a deployment that declares it differently applies it IN PLACE through
//! the pair of calls that service publishes for the tags of a resource that already exists —
//! the stack reaches `UPDATE_COMPLETE`, the resource keeps the identity the stack published
//! for it, and the owning service's own reads report exactly the tags the revision
//! declares.** Here the owning service is Amazon RDS: `ModifyDBInstance` has no `Tags` member
//! at all, while `AddTagsToResource` "adds metadata tags to an Amazon RDS resource" and
//! `RemoveTagsFromResource` "removes metadata tags from an Amazon RDS resource", both
//! addressed by the resource's ARN — so a revised `Tags` property reaches that service as
//! those two calls, and both `ListTagsForResource` and the instance's own
//! `DescribeDBInstances` record answer with the tags the revision asked for.
//!
//! Why it matters: a stack-managed tag is the source of truth a client keys cost allocation,
//! backup selection and tag-based IAM conditions off, so a deployment that reports
//! `UPDATE_COMPLETE` while the instance still carries the previous values has not taken
//! effect where it is read.
//!
//! The rows differ along what the rule generalises over — how the declared SET changes and
//! the FORM the revision states it in: a value changed, a tag added, one of two removed with
//! the other kept, a tag renamed, a value emptied, every declared tag removed by an empty
//! list and by dropping the property, tags declared on an instance created with none, and the
//! same set restated beside a member the instance's own modify carries, which owes the
//! tagging calls nothing at all.
//!
//! Every row's instance lives in ONE stack, over one VPC, two subnets and one DB subnet
//! group: the rows are independent resources of the same deployment, so one `CreateStack` and
//! one `UpdateStack` exercise them all and each read is taken by the identifier that
//! deployment published.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    call, create_stack, member, rds, router, settled_stack, xml_blocks, xml_text, REGION,
};

/// The stack every row is deployed in.
const STACK: &str = "cfn-rds-db-instance-tags";

/// The three keys CloudFormation records on every resource it provisions — the stack's name,
/// the stack's id and the resource's logical id — which no declaration states and which every
/// row expects the instance to go on carrying across the update.
///
/// Source: `crate::state::provisioning`'s `STACK_NAME_TAG`, `STACK_ID_TAG` and
/// `LOGICAL_ID_TAG`.
const PROVENANCE_KEYS: &[&str] = &[
    "aws:cloudformation:stack-name",
    "aws:cloudformation:stack-id",
    "aws:cloudformation:logical-id",
];

/// One declared tag of the instance's `Tags` property: the `[{Key, Value}]` entry form the
/// type publishes.
#[derive(Debug, Clone, Copy)]
struct Tag {
    key: &'static str,
    value: &'static str,
}

const STEP_CREATE: Tag = Tag {
    key: "step",
    value: "create",
};

/// The same tag — same key, so the same tag of the instance — with another value.
const STEP_UPDATE: Tag = Tag {
    key: "step",
    value: "update",
};

/// The same tag again with the value a tag may be at its documented minimum: "a value is the
/// optional value of the tag. The string value can be from 1 to 256 Unicode characters"
/// (`Tag$Value`), and the empty string is a value rather than the absence of a tag.
const STEP_EMPTY: Tag = Tag {
    key: "step",
    value: "",
};

/// A tag under ANOTHER key, which is a tag of its own.
const OWNER: Tag = Tag {
    key: "owner",
    value: "platform",
};

/// The first tag under a DIFFERENT key: a rename is a tag removed and a tag added.
const STEP_RENAMED: Tag = Tag {
    key: "phase",
    value: "create",
};

/// The `BackupRetentionPeriod` every row's create declares, and the one the row that also
/// revises a member of the instance's own modify call states instead ("the number of days for
/// which automated backups are retained", a member of `ModifyDBInstance`).
const CREATED_RETENTION: u32 = 1;
const REVISED_RETENTION: u32 = 2;

/// One row: the logical id and DB instance identifier it declares, the tag list the create and
/// the revision state, and the retention the revision declares. `None` is the property not
/// stated at all, which is a different declaration from the empty list.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    instance: &'static str,
    created: Option<&'static [Tag]>,
    revised: Option<&'static [Tag]>,
    revised_retention: u32,
}

const ROWS: &[Row] = &[
    Row {
        case: "a tag value changed, every other property restated",
        logical_id: "Revalued",
        instance: "cfn-db-tags-revalued",
        created: Some(&[STEP_CREATE]),
        revised: Some(&[STEP_UPDATE]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "a tag added beside the one the instance was created with",
        logical_id: "Added",
        instance: "cfn-db-tags-added",
        created: Some(&[STEP_CREATE]),
        revised: Some(&[STEP_CREATE, OWNER]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "one of two tags removed, the other kept",
        logical_id: "RemovedOne",
        instance: "cfn-db-tags-removed-one",
        created: Some(&[STEP_CREATE, OWNER]),
        revised: Some(&[OWNER]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "the one tag renamed, which is one tag removed and another added",
        logical_id: "Renamed",
        instance: "cfn-db-tags-renamed",
        created: Some(&[STEP_CREATE]),
        revised: Some(&[STEP_RENAMED]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "the one tag's value emptied, which is a value and not the absence of a tag",
        logical_id: "Emptied",
        instance: "cfn-db-tags-emptied-value",
        created: Some(&[STEP_CREATE]),
        revised: Some(&[STEP_EMPTY]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "every declared tag removed, the property stated as an empty list",
        logical_id: "Cleared",
        instance: "cfn-db-tags-cleared",
        created: Some(&[STEP_CREATE, OWNER]),
        revised: Some(&[]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "every declared tag removed, the property no longer stated at all",
        logical_id: "Dropped",
        instance: "cfn-db-tags-dropped",
        created: Some(&[STEP_CREATE]),
        revised: None,
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "tags declared on an instance that was created with none",
        logical_id: "Introduced",
        instance: "cfn-db-tags-introduced",
        created: None,
        revised: Some(&[STEP_UPDATE, OWNER]),
        revised_retention: CREATED_RETENTION,
    },
    Row {
        case: "the same tags restated, only the retention the modify carries revised",
        logical_id: "Restated",
        instance: "cfn-db-tags-restated",
        created: Some(&[STEP_CREATE, OWNER]),
        revised: Some(&[STEP_CREATE, OWNER]),
        revised_retention: REVISED_RETENTION,
    },
];

/// The whole stack at one revision: the network the instances sit in, the DB subnet group over
/// its two subnets, and every row's DB instance with the tag list and the retention this
/// revision declares for it.
fn template(revised: bool) -> String {
    let mut resources = json!({
        "Vpc": {
            "Type": "AWS::EC2::VPC",
            "Properties": {"CidrBlock": "10.0.0.0/16"},
        },
        "Subnet0": {
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": "10.0.1.0/24",
                "AvailabilityZone": "us-east-1a",
            },
        },
        "Subnet1": {
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": "10.0.2.0/24",
                "AvailabilityZone": "us-east-1b",
            },
        },
        "Subnets": {
            "Type": "AWS::RDS::DBSubnetGroup",
            "Properties": {
                "DBSubnetGroupName": "cfn-db-tags-subnets",
                "DBSubnetGroupDescription": "the subnets the tagged instances sit in",
                "SubnetIds": [{"Ref": "Subnet0"}, {"Ref": "Subnet1"}],
            },
        },
    });
    for row in ROWS {
        let tags = if revised { row.revised } else { row.created };
        let retention = if revised {
            row.revised_retention
        } else {
            CREATED_RETENTION
        };
        let mut properties = json!({
            "DBInstanceIdentifier": row.instance,
            "Engine": "mysql",
            "DBInstanceClass": "db.t3.micro",
            "DBSubnetGroupName": {"Ref": "Subnets"},
            "BackupRetentionPeriod": retention.to_string(),
        });
        if let Some(tags) = tags {
            let declared: Vec<Value> = tags
                .iter()
                .map(|tag| json!({"Key": tag.key, "Value": tag.value}))
                .collect();
            properties["Tags"] = Value::Array(declared);
        }
        resources[row.logical_id] = json!({
            "Type": "AWS::RDS::DBInstance",
            "Properties": properties,
        });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
    })
    .to_string()
}

/// One tag as Amazon RDS reports it.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReportedTag {
    key: String,
    value: String,
}

/// The tags one answer carries in its `TagList`, LESS the three CloudFormation records itself
/// — which every read is asserted to go on carrying, since they are no part of any
/// declaration and no deployment's to change.
///
/// `TagList` is a list of `Tag` structures, so the list is taken as a BLOCK first and its
/// `<Tag>` entries read inside it: a DB instance's record carries other lists that a flat scan
/// would sweep up. A member the answer leaves empty is that member's empty value.
fn reported_tags(case: &str, body: &str) -> Vec<ReportedTag> {
    let list = xml_blocks(body, "TagList")
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut reported: Vec<ReportedTag> = xml_blocks(&list, "Tag")
        .iter()
        .map(|entry| ReportedTag {
            key: xml_text(entry, "Key").unwrap_or_default(),
            value: xml_text(entry, "Value").unwrap_or_default(),
        })
        .collect();
    for key in PROVENANCE_KEYS {
        assert!(
            reported.iter().any(|tag| tag.key == *key),
            "case {case:?}: expected the instance to go on carrying CloudFormation's own \
             {key:?} record, got {reported:?}"
        );
    }
    reported.retain(|tag| !PROVENANCE_KEYS.contains(&tag.key.as_str()));
    reported.sort();
    reported
}

/// The tags a declaration asks the instance to end up carrying, in the order the reads above
/// answer in.
fn wanted_tags(tags: Option<&[Tag]>) -> Vec<ReportedTag> {
    let mut wanted: Vec<ReportedTag> = tags
        .unwrap_or(&[])
        .iter()
        .map(|tag| ReportedTag {
            key: tag.key.to_string(),
            value: tag.value.to_string(),
        })
        .collect();
    wanted.sort();
    wanted
}

/// `DescribeDBInstances` for one instance, by the identifier the deployment published.
async fn described_instance(case: &str, router: &MultiServiceRouter, instance: &str) -> String {
    let (status_code, body) = rds(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", instance)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeDBInstances({instance}) must answer 200, got {status_code} \
         {body}"
    );
    body
}

/// `ListTagsForResource` for one instance, by the ARN Amazon RDS reports for it — the way a
/// client reads the tag set of a resource that service holds.
async fn listed_tags(case: &str, router: &MultiServiceRouter, arn: &str) -> String {
    let (status_code, body) = rds(router, "ListTagsForResource", &[("ResourceName", arn)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: ListTagsForResource({arn}) must answer 200, got {status_code} {body}"
    );
    body
}

/// Both of that service's reads of one instance's tag set agree with `wanted`, and the
/// instance still answers to the identifier the stack published for it.
async fn assert_tags(
    case: &str,
    router: &MultiServiceRouter,
    instance: &str,
    wanted: &[ReportedTag],
) {
    let described = described_instance(case, router, instance).await;
    assert_eq!(
        xml_text(&described, "DBInstanceIdentifier").as_deref(),
        Some(instance),
        "case {case:?}: expected the instance to keep the identifier the stack published, got \
         {described}"
    );
    assert_eq!(
        reported_tags(case, &described),
        wanted,
        "case {case:?}: expected DescribeDBInstances to report the declared tag set, got \
         {described}"
    );
    let arn = xml_text(&described, "DBInstanceArn").unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "case {case:?}: expected the instance to report its ARN, got {described}"
    );
    let listed = listed_tags(case, router, &arn).await;
    assert_eq!(
        reported_tags(case, &listed),
        wanted,
        "case {case:?}: expected ListTagsForResource to report the declared tag set, got \
         {listed}"
    );
}

#[tokio::test]
async fn an_update_applies_the_declared_tags_to_the_db_instance_it_deployed() {
    let router = router();

    let (status_code, body) = create_stack(&router, STACK, &template(false), &[]).await;
    assert_eq!(
        status_code, 200,
        "CreateStack must answer 200, got {status_code} {body}"
    );
    let described = settled_stack(&router, "the create", STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the stack to deploy, got {described} (reasons {:?})",
        stack_event_reasons(&router, REGION, STACK).await
    );

    // The tags the CREATE asked for are each instance's, which is what makes the update's
    // reconciliation observable at all.
    for row in ROWS {
        assert_tags(row.case, &router, row.instance, &wanted_tags(row.created)).await;
    }

    let (status_code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", STACK),
            member("TemplateBody", &template(true)),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "UpdateStack must answer 200, got {status_code} {body}"
    );
    let described = settled_stack(&router, "the update", STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the update to complete, got {described} (reasons {:?})",
        stack_event_reasons(&router, REGION, STACK).await
    );

    for row in ROWS {
        // The tags are exactly the ones the revision declares, read back through both of
        // Amazon RDS's own reads of them...
        assert_tags(row.case, &router, row.instance, &wanted_tags(row.revised)).await;
        // ...and the member the instance's own modify call carries landed on it, so the
        // configuration half of the change is applied beside the tagging half.
        let described = described_instance(row.case, &router, row.instance).await;
        assert_eq!(
            xml_text(&described, "BackupRetentionPeriod").as_deref(),
            Some(row.revised_retention.to_string().as_str()),
            "case {:?}: expected the instance to report the revised retention {}, got \
             {described}",
            row.case,
            row.revised_retention
        );
    }
}
