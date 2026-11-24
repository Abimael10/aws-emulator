// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ECR::PullThroughCacheRule` a stack declares is REGISTERED in Amazon ECR itself,
//! and the rest of its lifecycle is that registry's own calls.
//!
//! The rule under test: **a stack operation deploys a resource through the owning service's
//! own API — the create that registers it, the update call for the members that call carries,
//! the delete that removes it, and that service's own read for drift** — applied to the pull
//! through cache rule of the private container registry. "A pull through cache rule provides a
//! way to cache images from an upstream registry source in your Amazon ECR private registry"
//! (`CreatePullThroughCacheRule`): the rule is what MAKES a reference under its
//! `ecrRepositoryPrefix` resolve, so a stack that reported the resource created while nothing
//! registered the rule publishes a prefix no image is served under.
//!
//! The rows differ along what that rule generalises over: a rule carrying only the two members
//! the create requires and one carrying every member it takes, a one-segment prefix and a
//! multi-segment one, both template spellings (JSON and YAML), the two sides of the
//! replacement classification (a property `UpdatePullThroughCacheRule` carries against one it
//! does not), and the three states a delete can find the registry in.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation, awsJson1.1
//! for Amazon ECR — through the one router `harness.rs` builds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, list_members, member, router, router_with_engine,
    wait_for_the_detection, wait_for_the_transition, xml_blocks, xml_text, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::change_plan::replacement;
use service_com_amazonaws_cloudformation_cloudformation::state::resource_drift_status;

/// The `X-Amz-Target` prefix Amazon ECR's model fixes (service shape
/// `AmazonEC2ContainerRegistry_V20150921`).
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The upstream every row caches from: the one Amazon ECR documents as serving anonymously, so
/// a rule needs no credential to be registered at all.
const UPSTREAM_REGISTRY_URL: &str = "public.ecr.aws";

/// The upstream registry name that URL resolves to, which a row may also state outright
/// (`CreatePullThroughCacheRule$upstreamRegistry`).
const UPSTREAM_REGISTRY: &str = "ecr-public";

/// The two roles the credential rows name, each an IAM role ARN — the kind of value
/// `customRoleArn` documents ("the ARN of the IAM role to be assumed by Amazon ECR").
const FIRST_ROLE: &str = "arn:aws:iam::000000000000:role/ecr-pull-through-first";
const SECOND_ROLE: &str = "arn:aws:iam::000000000000:role/ecr-pull-through-second";

/// The logical id every template below declares the rule under.
const RULE: &str = "CacheRule";

/// One Amazon ECR request, over the awsJson1.1 wire it speaks.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://api.ecr.{REGION}.amazonaws.com/"))
        .header("host", format!("api.ecr.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The rule Amazon ECR holds under `prefix`, or `None` when it holds none.
async fn held_rule(router: &MultiServiceRouter, prefix: &str) -> Option<Value> {
    let (status, body) = ecr(
        router,
        "DescribePullThroughCacheRules",
        &json!({"ecrRepositoryPrefixes": [prefix]}),
    )
    .await;
    if status != 200 {
        return None;
    }
    body["pullThroughCacheRules"]
        .as_array()
        .and_then(|listed| listed.first())
        .cloned()
}

/// Deploy `template` as stack `name` and answer the status and reason it settles with.
async fn deploy(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = create_stack(router, name, template, &[]).await;
    assert_eq!(status, 200, "CreateStack({name}) answered {status}: {body}");
    wait_for_the_transition().await;
    let (status, body) = describe_stack(router, name).await;
    assert_eq!(
        status, 200,
        "DescribeStacks({name}) answered {status}: {body}"
    );
    (
        xml_text(&body, "StackStatus"),
        xml_text(&body, "StackStatusReason"),
    )
}

/// Update stack `name` to `template` and answer the status and reason it settles with.
async fn update(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
) -> (Option<String>, Option<String>) {
    let (status, body) = call(
        router,
        "UpdateStack",
        &[member("StackName", name), member("TemplateBody", template)],
    )
    .await;
    assert_eq!(status, 200, "UpdateStack({name}) answered {status}: {body}");
    wait_for_the_transition().await;
    let (status, body) = describe_stack(router, name).await;
    assert_eq!(
        status, 200,
        "DescribeStacks({name}) answered {status}: {body}"
    );
    (
        xml_text(&body, "StackStatus"),
        xml_text(&body, "StackStatusReason"),
    )
}

/// The physical resource id the stack reports for the rule.
async fn physical_id(router: &MultiServiceRouter, stack: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", RULE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResources({stack}) answered {status}: {body}"
    );
    xml_text(&body, "PhysicalResourceId")
        .unwrap_or_else(|| panic!("expected a physical id for {RULE}, got {body}"))
}

/// A JSON template declaring one rule with `properties`.
fn json_template(properties: Value) -> String {
    json!({
        "Resources": {RULE: {
            "Type": "AWS::ECR::PullThroughCacheRule",
            "Properties": properties,
        }},
    })
    .to_string()
}

/// RULE: a declared pull through cache rule is registered in Amazon ECR under the prefix the
/// declaration states — the physical id the stack reports — carrying exactly the members the
/// declaration asked for and none it did not.
#[tokio::test]
async fn a_declared_pull_through_cache_rule_is_registered_in_ecr_with_its_upstream() {
    // (case, the template, the prefix the stack must report, the members the rule must carry)
    let rows: Vec<(&str, String, &str, Value)> = vec![
        (
            "json, only the two members the create requires",
            json_template(json!({
                "EcrRepositoryPrefix": "cfn-bare",
                "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
            })),
            "cfn-bare",
            json!({
                "ecrRepositoryPrefix": "cfn-bare",
                "upstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
            }),
        ),
        (
            "json, every member the create takes, under a multi-segment prefix",
            json_template(json!({
                "EcrRepositoryPrefix": "cfn-full/images",
                "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
                "UpstreamRegistry": UPSTREAM_REGISTRY,
                "UpstreamRepositoryPrefix": "docker",
                "CustomRoleArn": FIRST_ROLE,
            })),
            "cfn-full/images",
            json!({
                "ecrRepositoryPrefix": "cfn-full/images",
                "upstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
                "upstreamRepositoryPrefix": "docker",
                "customRoleArn": FIRST_ROLE,
            }),
        ),
        (
            "yaml, an upstream prefix and no credential",
            format!(
                "Resources:\n  {RULE}:\n    Type: AWS::ECR::PullThroughCacheRule\n    \
                 Properties:\n      EcrRepositoryPrefix: cfn-yaml\n      \
                 UpstreamRegistryUrl: {UPSTREAM_REGISTRY_URL}\n      \
                 UpstreamRepositoryPrefix: library\n"
            ),
            "cfn-yaml",
            json!({
                "ecrRepositoryPrefix": "cfn-yaml",
                "upstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
                "upstreamRepositoryPrefix": "library",
            }),
        ),
    ];

    for (index, (case, template, prefix, members)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-ptc-create-{index}");
        let (status, reason) = deploy(&router, &stack, template).await;
        assert_eq!(
            status.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        let reported = physical_id(&router, &stack).await;
        assert_eq!(
            reported, *prefix,
            "case {case:?}: expected the declared prefix as the physical id, got {reported:?}"
        );
        let held = held_rule(&router, &reported)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected ECR to hold the rule {prefix:?}"));
        for (member, expected) in members.as_object().expect("the members are an object") {
            assert_eq!(
                held[member], *expected,
                "case {case:?}: expected {member} to be {expected}, got {held}"
            );
        }
        // Nothing the declaration did not state is reported: a client reading member presence
        // as "the template set it" is never handed a value it never wrote. `upstreamRegistry`
        // is in that list for every row, the one that declares it included — that read
        // answers without the member for every rule, whichever way it was created
        // (Amazon ECR's own `DescribePullThroughCacheRules`).
        for absent in [
            "upstreamRegistry",
            "upstreamRepositoryPrefix",
            "customRoleArn",
            "credentialArn",
        ] {
            if members.get(absent).is_some() {
                continue;
            }
            assert_eq!(
                held.get(absent),
                None,
                "case {case:?}: the rule must report no {absent} the declaration did not \
                 state, got {held}"
            );
        }
    }
}

/// RULE: a CHANGED declaration of a member `UpdatePullThroughCacheRule` carries reaches the
/// rule the stack already has, through that call — so the rule keeps its prefix, and the
/// registry's own read answers what the template now declares.
#[tokio::test]
async fn a_changed_credential_reaches_the_rule_the_stack_already_has() {
    let router = router();
    let stack = "ecr-ptc-update";
    let declaration = |role: &str| {
        json_template(json!({
            "EcrRepositoryPrefix": "cfn-rolled",
            "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
            "CustomRoleArn": role,
        }))
    };

    let (status, reason) = deploy(&router, stack, &declaration(FIRST_ROLE)).await;
    assert_eq!(
        status.as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {status:?} ({reason:?})"
    );
    let created = physical_id(&router, stack).await;

    let (status, reason) = update(&router, stack, &declaration(SECOND_ROLE)).await;
    assert_eq!(
        status.as_deref(),
        Some("UPDATE_COMPLETE"),
        "expected the update to complete, got {status:?} ({reason:?})"
    );

    // The rule the stack reports is the SAME one — the prefix is its identity — and Amazon ECR
    // holds the role the template now declares.
    let updated = physical_id(&router, stack).await;
    assert_eq!(
        updated, created,
        "an in-place update keeps the rule's prefix, got {updated:?} after {created:?}"
    );
    let held = held_rule(&router, &updated)
        .await
        .unwrap_or_else(|| panic!("expected ECR to still hold the rule {updated:?}"));
    assert_eq!(
        held["customRoleArn"],
        json!(SECOND_ROLE),
        "expected the role the update declared on the rule, got {held}"
    );
}

/// One row of the replacement plan: the declaration the update states, and what a change set
/// must say the change would do to the rule.
struct PlanRow {
    case: &'static str,
    stack: &'static str,
    /// The prefix this row's own rule caches under, so two rows never ask the registry for
    /// one prefix twice.
    prefix: &'static str,
    /// The property the row restates, and the value it restates it to.
    property: &'static str,
    value: &'static str,
    replacement: &'static str,
}

/// The two sides of the classification, each on a property the base declaration states: the
/// credential `UpdatePullThroughCacheRule` carries, against the upstream members it carries no
/// member for at all — plus the prefix, which IS the rule's identity.
const PLAN_ROWS: &[PlanRow] = &[
    PlanRow {
        case: "the credential the upstream is reached with",
        stack: "ecr-ptc-plan-role",
        prefix: "cfn-plan-role",
        property: "CustomRoleArn",
        value: SECOND_ROLE,
        replacement: replacement::FALSE,
    },
    PlanRow {
        case: "the upstream repository prefix the rule prepends",
        stack: "ecr-ptc-plan-upstream-prefix",
        prefix: "cfn-plan-upstream",
        property: "UpstreamRepositoryPrefix",
        value: "library",
        replacement: replacement::TRUE,
    },
    PlanRow {
        case: "the upstream registry the rule caches from",
        stack: "ecr-ptc-plan-upstream-registry",
        prefix: "cfn-plan-registry",
        property: "UpstreamRegistry",
        value: "ecr",
        replacement: replacement::TRUE,
    },
    PlanRow {
        case: "the prefix the rule caches under, which is its identity",
        stack: "ecr-ptc-plan-prefix",
        prefix: "cfn-plan-prefix",
        property: "EcrRepositoryPrefix",
        value: "cfn-plan-moved",
        replacement: replacement::TRUE,
    },
];

/// The base declaration one plan row restates a property of, under that row's own prefix.
fn plan_base(prefix: &str) -> Value {
    json!({
        "EcrRepositoryPrefix": prefix,
        "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
        "UpstreamRegistry": UPSTREAM_REGISTRY,
        "UpstreamRepositoryPrefix": "docker",
        "CustomRoleArn": FIRST_ROLE,
    })
}

/// RULE: a property of this type the owning service has no call to change on the rule that
/// exists cannot take a new value while the rule goes on existing, so a change set reports the
/// resource's `Replacement` `True`; a property `UpdatePullThroughCacheRule` DOES take is a
/// change to the rule the stack already has, which is `False`.
#[tokio::test]
async fn a_change_set_reports_what_each_changed_property_does_to_the_rule() {
    let (router, engine) = router_with_engine();
    for row in PLAN_ROWS {
        let case = row.case;
        let (status, reason) =
            deploy(&router, row.stack, &json_template(plan_base(row.prefix))).await;
        assert_eq!(
            status.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        settle_the_store(&router, &engine).await;

        let mut restated = plan_base(row.prefix);
        restated[row.property] = json!(row.value);
        let (status, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", row.stack),
                member("ChangeSetName", "replan"),
                member("TemplateBody", &json_template(restated)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the change set must be created, got {status} {body}"
        );
        settle_the_store(&router, &engine).await;

        let (status, body) = call(
            &router,
            "DescribeChangeSet",
            &[
                member("ChangeSetName", "replan"),
                member("StackName", row.stack),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the change set must be describable, got {status} {body}"
        );
        let changes = xml_blocks(&body, "ResourceChange");
        assert_eq!(
            changes.len(),
            1,
            "case {case:?}: expected exactly one planned change, got {body}"
        );
        assert_eq!(
            xml_text(&changes[0], "LogicalResourceId").as_deref(),
            Some(RULE),
            "case {case:?}: the planned change must be the rule's, got {body}"
        );
        assert_eq!(
            xml_text(&changes[0], "Replacement").as_deref(),
            Some(row.replacement),
            "case {case:?}: expected Replacement {:?}, got {}",
            row.replacement,
            changes[0]
        );
    }
}

/// RULE: drift detection reports the rule's configuration as AMAZON ECR reports it, projected
/// onto the type's own property names — so a credential changed outside the stack is reported
/// as a difference between what the template declared and what the registry holds, and a rule
/// nothing touched is in sync.
#[tokio::test]
async fn drift_reports_the_registrys_own_record_of_the_rule() {
    let router = router();
    let stack = "ecr-ptc-drift";
    let template = json_template(json!({
        "EcrRepositoryPrefix": "cfn-drifted",
        "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
        "CustomRoleArn": FIRST_ROLE,
    }));
    let (status, reason) = deploy(&router, stack, &template).await;
    assert_eq!(
        status.as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {status:?} ({reason:?})"
    );

    // Nothing has touched the rule, so the comparison is IN SYNC.
    let reported = detected_drift(&router, stack).await;
    assert_eq!(
        reported.as_deref(),
        Some(resource_drift_status::IN_SYNC),
        "expected the untouched rule to be in sync, got {reported:?}"
    );

    // The credential is changed OUTSIDE the stack, through Amazon ECR's own call.
    let (status, body) = ecr(
        &router,
        "UpdatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": "cfn-drifted",
            "customRoleArn": SECOND_ROLE,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the out-of-band update must be accepted, got {status}: {body}"
    );

    let reported = detected_drift(&router, stack).await;
    assert_eq!(
        reported.as_deref(),
        Some(resource_drift_status::MODIFIED),
        "expected the changed rule to be reported modified, got {reported:?}"
    );
    let differences = reported_differences(&router, stack).await;
    assert!(
        differences.contains(&(
            "/CustomRoleArn".to_string(),
            FIRST_ROLE.to_string(),
            SECOND_ROLE.to_string(),
        )),
        "expected the difference between the declared and the held role, got {differences:?}"
    );
}

/// Run a drift detection over `stack` and answer the drift status reported for the rule.
async fn detected_drift(router: &MultiServiceRouter, stack: &str) -> Option<String> {
    let (status, body) = call(router, "DetectStackDrift", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DetectStackDrift({stack}) answered {status}: {body}"
    );
    wait_for_the_detection().await;
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResourceDrifts({stack}) answered {status}: {body}"
    );
    list_members(&body, "StackResourceDrifts")
        .into_iter()
        .find(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(RULE))
        .and_then(|block| xml_text(&block, "StackResourceDriftStatus"))
}

/// The `(path, expected, actual)` of every property difference the last detection reported for
/// the rule.
async fn reported_differences(
    router: &MultiServiceRouter,
    stack: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = call(
        router,
        "DescribeStackResourceDrifts",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeStackResourceDrifts({stack}) answered {status}: {body}"
    );
    let Some(block) = list_members(&body, "StackResourceDrifts")
        .into_iter()
        .find(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(RULE))
    else {
        return Vec::new();
    };
    list_members(&block, "PropertyDifferences")
        .into_iter()
        .map(|difference| {
            (
                xml_text(&difference, "PropertyPath").unwrap_or_default(),
                xml_text(&difference, "ExpectedValue").unwrap_or_default(),
                xml_text(&difference, "ActualValue").unwrap_or_default(),
            )
        })
        .collect()
}

/// RULE: the delete a stack operation makes is the owning service's own, so the rule is gone
/// from Amazon ECR with the stack that registered it — and a rule that registry no longer
/// holds is already in the state the delete asks for, so the deletion still completes.
#[tokio::test]
async fn the_rule_is_gone_with_the_stack_that_registered_it() {
    // (case, the stack, the prefix, whether the rule is removed OUT OF BAND before the delete)
    let rows: &[(&str, &str, &str, bool)] = &[
        ("the rule the stack still holds", "ecr-ptc-delete", "cfn-deleted", false),
        (
            "a rule the registry no longer holds",
            "ecr-ptc-delete-gone",
            "cfn-deleted-early",
            true,
        ),
    ];

    for (case, stack, prefix, removed_out_of_band) in rows {
        let router = router();
        let template = json_template(json!({
            "EcrRepositoryPrefix": prefix,
            "UpstreamRegistryUrl": UPSTREAM_REGISTRY_URL,
        }));
        let (status, reason) = deploy(&router, stack, &template).await;
        assert_eq!(
            status.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {status:?} ({reason:?})"
        );
        assert!(
            held_rule(&router, prefix).await.is_some(),
            "case {case:?}: the rule must be registered before the delete"
        );

        if *removed_out_of_band {
            let (status, body) = ecr(
                &router,
                "DeletePullThroughCacheRule",
                &json!({"ecrRepositoryPrefix": prefix}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the out-of-band delete must be accepted, got {status}: {body}"
            );
        }

        let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack answered {status}: {body}"
        );
        wait_for_the_transition().await;
        assert!(
            held_rule(&router, prefix).await.is_none(),
            "case {case:?}: the rule must be gone with the stack that registered it"
        );
        // The deletion GOES THROUGH either way: a rule the registry no longer holds is not a
        // refusal, so the stack is taken away rather than left in a failed deletion.
        let (status, body) = describe_stack(&router, stack).await;
        assert_eq!(
            (status, xml_text(&body, "Code").as_deref()),
            (400, Some("ValidationError")),
            "case {case:?}: the deleted stack must be gone, got {status} {body}"
        );
    }
}
