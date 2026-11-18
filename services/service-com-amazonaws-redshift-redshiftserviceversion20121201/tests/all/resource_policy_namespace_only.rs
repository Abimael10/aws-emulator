// SPDX-License-Identifier: CC0-1.0

//! WHICH resource a resource policy attaches to, and WHAT form the service
//! reports the document in, end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! Three rules, each asserted over a table rather than over one request:
//!
//! * a resource policy attaches to a NAMESPACE, so an ARN naming a resource of
//!   any other type is refused by the write, the read and the delete alike, with
//!   the type the request's own ARN carried named in the refusal — whether or
//!   not the resource that ARN names exists. WHICH of the two refusals it earns
//!   is the validation layer that catches it: the service's shared ARN resolver
//!   answers a type segment it resolves no resource of at all, and only a
//!   segment it does resolve reaches the resource-policy rule proper;
//! * a policy body that is no JSON policy document is refused with the service's
//!   fixed parse-failure sentence, which carries no part of the request;
//! * the document the service reports is the one it was given in ITS canonical
//!   form: a policy element written as a one-element list is reported as the
//!   single value that list holds, while a list of several values, and
//!   everything a condition block carries, is reported as it was written.
//!
//! The sentences and the canonical forms are spelled out here independently of
//! the handlers, so a change in either is a failure rather than a test that
//! follows it.
//!
//! [`crate::resource_policies`] covers the rest of the surface: what a write
//! holds, what a replacement and a delete do, and the ARNs that name no policy
//! holder at all.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator, REGION};
use crate::resource_policies::{
    assert_refused, delete, get, policy_with_sid, put, seed_cluster, seed_snapshot,
    serverless_namespace_arn, stored_arns, stored_policy,
};

/// The account every ARN in this suite is written for: the one this emulator
/// serves, so an ARN of an existing resource really is that resource's ARN.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// One Redshift ARN of this account, with `resource` as its resource portion.
fn redshift_arn(resource: &str) -> String {
    format!("arn:aws:redshift:{REGION}:{}:{resource}", account())
}

/// One Redshift Serverless ARN of this account, with `resource` as its resource
/// portion.
fn serverless_arn(resource: &str) -> String {
    format!(
        "arn:aws:redshift-serverless:{REGION}:{}:{resource}",
        account()
    )
}
/// The sentence a resource-policy operation answers an ARN naming a resource of
/// a type the shared ARN resolver RESOLVES, but which carries no resource
/// policy, with.
///
/// Written out here rather than taken from the handler's own catalog: the fixed
/// text is the requirement, and its one variable part is the resource type read
/// from the `ResourceArn` the request carried.
fn unsupported_type_message(resource_type: &str) -> String {
    format!(
        "Resource type {resource_type} isn't supported for the resource policy. \
         Choose another resource type."
    )
}

/// The fixed text the shared ARN resolver opens its own refusal with, up to the
/// resource-type segment it quotes.
const RESOLVER_REFUSAL_PREFIX: &str = "Tagging is not supported for this type of resource: '";

/// The fixed text that closes the resolver's refusal, after the quoted segment.
const RESOLVER_REFUSAL_SUFFIX: &str =
    "' (the ARN is potentially malformed, please check the ARN documentation for more information)";

/// The sentence a resource-policy operation answers an ARN whose resource-type
/// segment names none of the resource types this service resolves an ARN for
/// with: the refusal of the shared ARN resolver, which runs BEFORE the
/// resource-policy rule above.
///
/// Written out here rather than taken from the handler's own catalog, for the
/// reason [`unsupported_type_message`] is: the fixed text is the requirement and
/// its one variable part is the type segment read from the `ResourceArn` the
/// request carried. Assembled from its two halves so each can be asserted on its
/// own.
fn unresolvable_type_message(resource_type: &str) -> String {
    format!("{RESOLVER_REFUSAL_PREFIX}{resource_type}{RESOLVER_REFUSAL_SUFFIX}")
}

/// One row of the resource-type table: the case, the ARN the call carries, the
/// resource type the refusal must name, and whether that type is one the shared
/// ARN resolver resolves a resource of — which decides WHICH of the two
/// sentences the refusal carries.
struct TypeCase {
    case: &'static str,
    resource_arn: String,
    resource_type: &'static str,
    resolvable: bool,
}

impl TypeCase {
    /// The message this row's refusal must carry: the resource-policy rule's
    /// sentence for a type the ARN resolver resolves, the resolver's own for a
    /// segment it resolves nothing for.
    fn expected_message(&self) -> String {
        if self.resolvable {
            unsupported_type_message(self.resource_type)
        } else {
            unresolvable_type_message(self.resource_type)
        }
    }
}

/// The resource types an Amazon Redshift ARN names, as the published ARN forms
/// write them.
///
/// Source for the provisioned forms: the Amazon Redshift Cluster Management
/// Guide's "Constructing an Amazon Redshift Amazon Resource Name (ARN)" (the
/// page `smithy_query(query="doc", shape="TaggedResource",
/// member="ResourceType")` links to), which is where this service's own ARN
/// builders take them from; for the serverless forms, the resources that
/// control plane publishes. The rows span resources that EXIST in this account
/// and resources nothing created, the `:` and the `/` separator, and both
/// services — because the type, not the resource, is what the refusal is about.
fn resource_type_table() -> Vec<TypeCase> {
    vec![
        TypeCase {
            case: "the cluster this account holds",
            resource_arn: state::cluster_arn("policy-types", REGION),
            resource_type: "cluster",
            resolvable: true,
        },
        TypeCase {
            case: "a cluster nothing created",
            resource_arn: state::cluster_arn("no-such-cluster", REGION),
            resource_type: "cluster",
            resolvable: true,
        },
        TypeCase {
            case: "the snapshot this account holds",
            resource_arn: state::snapshot_arn("policy-types", "snap-1", REGION),
            resource_type: "snapshot",
            resolvable: true,
        },
        TypeCase {
            case: "a snapshot nothing took",
            resource_arn: state::snapshot_arn("policy-types", "no-such-snapshot", REGION),
            resource_type: "snapshot",
            resolvable: true,
        },
        TypeCase {
            // A datashare is no resource type the shared ARN resolver resolves a
            // resource for, so the resolver, not the resource-policy rule, is
            // what refuses this one.
            case: "a datashare of a namespace",
            resource_arn: redshift_arn(
                "datashare:3fd3f0cf-11bd-4fd0-9c4b-1c0f2c4b1a4f/sales",
            ),
            resource_type: "datashare",
            resolvable: false,
        },
        TypeCase {
            case: "a cluster parameter group",
            resource_arn: state::parameter_group_arn("default.redshift-1.0", REGION),
            resource_type: "parametergroup",
            resolvable: true,
        },
        TypeCase {
            case: "a cluster subnet group",
            resource_arn: state::subnet_group_arn("policy-types-subnets", REGION),
            resource_type: "subnetgroup",
            resolvable: true,
        },
        TypeCase {
            case: "an HSM configuration",
            resource_arn: state::hsm_configuration_arn("policy-types-hsm", REGION),
            resource_type: "hsmconfiguration",
            resolvable: true,
        },
        TypeCase {
            case: "an event subscription",
            resource_arn: redshift_arn("eventsubscription:policy-types-events"),
            resource_type: "eventsubscription",
            resolvable: false,
        },
        TypeCase {
            case: "a serverless workgroup, whose resource portion uses a slash",
            resource_arn: serverless_arn("workgroup/8a7d2f3c-5b1e-4a90-9c2d-6f0b1e3a4c5d"),
            resource_type: "workgroup",
            resolvable: false,
        },
        TypeCase {
            case: "a serverless snapshot, whose resource portion uses a slash",
            resource_arn: serverless_arn("snapshot/policy-types-snapshot"),
            resource_type: "snapshot",
            resolvable: true,
        },
        TypeCase {
            // An ARN whose resource portion opens with its separator names no
            // type at all, and the refusal says so with the type it read: the
            // empty one, which the resolver resolves nothing for.
            case: "a resource portion that opens with its separator",
            resource_arn: redshift_arn("/policy-types"),
            resource_type: "",
            resolvable: false,
        },
    ]
}

/// A resource policy attaches to a namespace and to nothing else: every other
/// resource type is refused by all three operations, with the type the request
/// carried named, and nothing is attached.
#[tokio::test]
async fn only_a_namespace_takes_a_resource_policy() {
    let emulator = Emulator::new();
    seed_cluster(&emulator, "policy-types").await;
    seed_snapshot(&emulator, "policy-types", "snap-1");

    // The namespace of that very cluster takes a policy, so every refusal below
    // is about the TYPE in the ARN rather than about the resource behind it.
    let namespace = state::namespace_arn("policy-types", REGION);
    let attached = policy_with_sid("Attached");
    let held = put(&emulator, &namespace, &attached).await;
    assert_eq!(
        held.status, 200,
        "case \"the namespace of the cluster\": expected 200, got {} with body {}",
        held.status, held.body
    );

    for row in resource_type_table() {
        for (action, answer) in [
            ("PutResourcePolicy", put(&emulator, &row.resource_arn, &attached).await),
            ("GetResourcePolicy", get(&emulator, &row.resource_arn).await),
            ("DeleteResourcePolicy", delete(&emulator, &row.resource_arn).await),
        ] {
            let case = format!("{} through {action}", row.case);
            assert_refused(&case, &answer, 400, "InvalidParameterValue");
            assert_eq!(
                answer.text("Message").as_deref(),
                Some(row.expected_message().as_str()),
                "case {case}: expected the refusal to name the resource type {:?}, got {:?}",
                row.resource_type,
                answer.text("Message")
            );
        }
        assert_eq!(
            stored_policy(&emulator, &row.resource_arn),
            None,
            "case {}: expected the refused write to attach nothing, got {:?}",
            row.case,
            stored_policy(&emulator, &row.resource_arn)
        );
    }

    // The one policy this account holds is still the one the namespace was
    // written with: no refused call attached, replaced or detached anything.
    assert_eq!(
        stored_arns(&emulator),
        vec![namespace.clone()],
        "case \"the policies this account holds\": expected only the namespace's"
    );
    assert_eq!(
        stored_policy(&emulator, &namespace).as_deref(),
        Some(attached.as_str()),
        "case \"the namespace's own document\": expected it untouched, got {:?}",
        stored_policy(&emulator, &namespace)
    );
}

/// Resource-type segments the service's shared ARN resolver DOES resolve a
/// resource for, none of which carries a resource policy.
///
/// Source: the resource types a Redshift tag attaches to —
/// `smithy_query(query="doc", shape="TaggedResource", member="ResourceType")`
/// ("Cluster, CIDR/IP, EC2 security group, Snapshot, Cluster security group,
/// Subnet group, HSM connection, HSM certificate, Parameter group") plus the two
/// `smithy_query(query="doc", shape="DescribeTagsMessage", member="ResourceType")`
/// adds ("Snapshot copy grant", "Integration") — written as the ARN segments the
/// Amazon Redshift Cluster Management Guide's "Constructing an Amazon Redshift
/// Amazon Resource Name (ARN)" page (the page that doc links to) publishes for
/// them. Every entry here is a type the table above does not carry.
const RESOLVED_TYPES: [&str; 6] = [
    "securitygroup",
    "securitygroupingress-cidr",
    "securitygroupingress-ec2securitygroup",
    "hsmclientcertificate",
    "snapshotcopygrant",
    "integration",
];

/// Resource-type segments the shared ARN resolver resolves NOTHING for: resource
/// portions naming Redshift concepts that are absent from the catalog above — a
/// datashare, a usage limit, a scheduled action, an IAM Identity Center
/// application, a database user — beside a segment no published ARN form uses at
/// all and an empty one.
///
/// Source for the segments: the resource portions this service's own ARN
/// builders and the published ARN forms write for those concepts.
const UNRESOLVED_TYPES: [&str; 7] = [
    "datashare",
    "usagelimit",
    "scheduledaction",
    "redshiftidcapplication",
    "dbuser",
    "widget",
    "",
];

/// The three resource-policy operations' answers to one ARN, so one row of a
/// table is asserted across the whole surface.
async fn refusals(
    emulator: &Emulator,
    resource_arn: &str,
) -> Vec<(&'static str, Answer)> {
    let document = policy_with_sid("ArnResolution");
    vec![
        (
            "PutResourcePolicy",
            put(emulator, resource_arn, &document).await,
        ),
        ("GetResourcePolicy", get(emulator, resource_arn).await),
        ("DeleteResourcePolicy", delete(emulator, resource_arn).await),
    ]
}

/// A resource type the shared ARN resolver resolves, but which carries no
/// resource policy, is refused by the resource-policy rule — which names the
/// type, whatever the resource portion after it looks like.
#[tokio::test]
async fn a_resolvable_resource_type_is_refused_by_the_resource_policy_rule() {
    let emulator = Emulator::new();

    for resource_type in RESOLVED_TYPES {
        // Both separators a published ARN writes its resource portion with, and
        // a portion that is the type alone: the refusal is about the TYPE, not
        // about how the identifier after it is spelled.
        for (shape, resource) in [
            (
                "a colon before the identifier",
                format!("{resource_type}:policy-arn-probe"),
            ),
            (
                "a slash before the identifier",
                format!("{resource_type}/policy-arn-probe"),
            ),
            ("no identifier at all", resource_type.to_string()),
        ] {
            let resource_arn = redshift_arn(&resource);
            for (action, answer) in refusals(&emulator, &resource_arn).await {
                let case = format!("{resource_type} with {shape} through {action}");
                assert_refused(&case, &answer, 400, "InvalidParameterValue");
                assert_eq!(
                    answer.text("Message").as_deref(),
                    Some(unsupported_type_message(resource_type).as_str()),
                    "case {case}: expected the resource-policy rule's sentence naming {resource_type:?}, got {:?}",
                    answer.text("Message")
                );
            }
            assert_eq!(
                stored_policy(&emulator, &resource_arn),
                None,
                "case {resource_type} with {shape}: expected the refused write to attach nothing, got {:?}",
                stored_policy(&emulator, &resource_arn)
            );
        }
    }
}

/// A resource-type segment the shared ARN resolver resolves nothing for is
/// refused by the RESOLVER, with its two fixed halves around the segment the
/// request's own ARN carried.
#[tokio::test]
async fn an_unresolvable_resource_type_is_refused_by_the_arn_resolver() {
    let emulator = Emulator::new();

    for resource_type in UNRESOLVED_TYPES {
        // A resource portion written `<type>/<identifier>` is the one form every
        // segment takes, the empty one included.
        let mut shapes = vec![(
            "a slash before the identifier",
            format!("{resource_type}/probe"),
        )];
        // The other two forms need a NON-empty segment: with an empty one, a
        // colon after it leaves the ARN's sixth field empty and a portion that
        // is the segment alone leaves no resource portion at all — in both cases
        // the value is no ARN, which is a different refusal.
        if !resource_type.is_empty() {
            shapes.push((
                "a colon before the identifier",
                format!("{resource_type}:11111111-1111-1111-1111-111111111111/probe"),
            ));
            shapes.push(("no identifier at all", resource_type.to_string()));
        }
        for (shape, resource) in shapes {
            let resource_arn = redshift_arn(&resource);
            for (action, answer) in refusals(&emulator, &resource_arn).await {
                let case = format!("{resource_type:?} with {shape} through {action}");
                assert_refused(&case, &answer, 400, "InvalidParameterValue");
                let message = answer.text("Message").unwrap_or_default();
                assert_eq!(
                    message,
                    unresolvable_type_message(resource_type),
                    "case {case}: expected the resolver's sentence quoting {resource_type:?}, got {message:?}"
                );
                // The halves are asserted on their own too, so a sentence that
                // matched by carrying different fixed text cannot pass, and the
                // segment between them is read by stripping EXACTLY one of each.
                let quoted = message
                    .strip_prefix(RESOLVER_REFUSAL_PREFIX)
                    .and_then(|rest| rest.strip_suffix(RESOLVER_REFUSAL_SUFFIX));
                assert_eq!(
                    quoted,
                    Some(resource_type),
                    "case {case}: expected the resolver's fixed halves around the segment the ARN carried, got {message:?}"
                );
            }
            assert_eq!(
                stored_policy(&emulator, &resource_arn),
                None,
                "case {resource_type:?} with {shape}: expected the refused write to attach nothing, got {:?}",
                stored_policy(&emulator, &resource_arn)
            );
        }
    }

    // Every call above was refused, so this account holds no policy at all — and
    // a namespace ARN still takes one, so the refusals are about the type
    // segment rather than an emulator refusing everything.
    assert_eq!(
        stored_arns(&emulator),
        Vec::<String>::new(),
        "case \"the policies this account holds\": expected none, got {:?}",
        stored_arns(&emulator)
    );
    let namespace = serverless_namespace_arn("4f2b1d7e-9c3a-4f55-8c2b-7d1e0a6b3c48");
    let attached = put(&emulator, &namespace, &policy_with_sid("Control")).await;
    assert_eq!(
        attached.status, 200,
        "case \"a namespace ARN\": expected 200, got {} with body {}",
        attached.status, attached.body
    );
}

/// A body that is no JSON policy document is refused with the service's fixed
/// parse-failure sentence, whatever is wrong with it.
#[tokio::test]
async fn a_body_that_is_no_policy_document_is_refused_with_the_published_sentence() {
    let emulator = Emulator::new();
    let namespace = serverless_namespace_arn("5533bed2-5c89-4049-91c3-77252462b84d");

    // The sentence is entirely fixed: it names nothing of the request, so every
    // row expects the same text.
    let expected = "The resource policy can't be parsed.";
    let table = [
        ("a body that is not JSON at all", "not a policy".to_string()),
        ("an unterminated object", r#"{"Statement":"#.to_string()),
        ("an object missing its closing brace", r#"{"Version":"2012-10-17""#.to_string()),
        ("a JSON array", r#"[{"Effect":"Allow"}]"#.to_string()),
        ("a JSON string", r#""a policy""#.to_string()),
        ("a JSON number", "12".to_string()),
        ("JSON null", "null".to_string()),
        ("a trailing comma", r#"{"Version":"2012-10-17",}"#.to_string()),
        ("two documents in one value", "{}{}".to_string()),
        ("non-ASCII text that is no JSON document", "политика".to_string()),
    ];

    for (case, policy) in &table {
        let refused = put(&emulator, &namespace, policy).await;
        assert_refused(case, &refused, 400, "InvalidPolicyFault");
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(expected),
            "case {case}: expected the fixed parse-failure sentence, got {:?}",
            refused.text("Message")
        );
    }
    assert_eq!(
        stored_arns(&emulator),
        Vec::<String>::new(),
        "case \"a refused write\": expected the store to hold no policy at all"
    );
}

/// One row of the canonical-form table: the document a write sends and the
/// document every read of it must report.
struct CanonicalCase {
    case: &'static str,
    submitted: String,
    reported: String,
}

/// The canonical-form table: rows differing along which policy element carries
/// the one-element list, how many values the list holds, where the list sits,
/// and what else the document carries.
///
/// The members that take "a value or a list of values" are the IAM JSON policy
/// grammar's `Principal`, `Action` and `Resource` elements and their `Not…`
/// counterparts; a `Condition` block's keys and values are the caller's own
/// vocabulary and are reported as they were written.
fn canonical_table() -> Vec<CanonicalCase> {
    let root = format!("arn:aws:iam::{}:root", account());
    let namespace = state::namespace_arn("policy-canonical", REGION);
    vec![
        CanonicalCase {
            case: "a one-element Principal.AWS, Action and Resource",
            submitted: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"One","Effect":"Allow","Principal":{{"AWS":["{root}"]}},"Action":["redshift:GetClusterCredentials"],"Resource":["{namespace}"]}}]}}"#
            ),
            reported: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"One","Effect":"Allow","Principal":{{"AWS":"{root}"}},"Action":"redshift:GetClusterCredentials","Resource":"{namespace}"}}]}}"#
            ),
        },
        CanonicalCase {
            case: "lists of two values, which stay lists",
            submitted: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":["{root}","arn:aws:iam::111122223333:root"]}},"Action":["redshift:GetClusterCredentials","redshift:DescribeClusters"],"Resource":["{namespace}","*"]}}]}}"#
            ),
            reported: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":["{root}","arn:aws:iam::111122223333:root"]}},"Action":["redshift:GetClusterCredentials","redshift:DescribeClusters"],"Resource":["{namespace}","*"]}}]}}"#
            ),
        },
        CanonicalCase {
            case: "the Not… counterparts of the same three elements",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","NotPrincipal":{"Service":["glue.amazonaws.com"]},"NotAction":["redshift:DeleteCluster"],"NotResource":["*"]}]}"#.to_string(),
            reported: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","NotPrincipal":{"Service":"glue.amazonaws.com"},"NotAction":"redshift:DeleteCluster","NotResource":"*"}]}"#.to_string(),
        },
        CanonicalCase {
            case: "a condition block with non-ASCII keys and values, beside a collapsed Action",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":["redshift:DescribeClusters"],"Resource":"*","Condition":{"StringEquals":{"政策:キー":["値 ✓"],"aws:SourceAccount":"000000000000"}}}]}"#.to_string(),
            reported: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"redshift:DescribeClusters","Resource":"*","Condition":{"StringEquals":{"政策:キー":["値 ✓"],"aws:SourceAccount":"000000000000"}}}]}"#.to_string(),
        },
        CanonicalCase {
            case: "several statements, each canonicalized on its own",
            submitted: r#"{"Statement":[{"Effect":"Allow","Action":["redshift:DescribeClusters"],"Resource":["*"]},{"Effect":"Deny","Action":["redshift:DeleteCluster","redshift:ModifyCluster"],"Resource":["*"]}]}"#.to_string(),
            reported: r#"{"Statement":[{"Effect":"Allow","Action":"redshift:DescribeClusters","Resource":"*"},{"Effect":"Deny","Action":["redshift:DeleteCluster","redshift:ModifyCluster"],"Resource":"*"}]}"#.to_string(),
        },
        CanonicalCase {
            case: "one statement written as the object the grammar also admits",
            submitted: r#"{"Statement":{"Effect":"Allow","Action":["redshift:DescribeClusters"],"Resource":["*"]}}"#.to_string(),
            reported: r#"{"Statement":{"Effect":"Allow","Action":"redshift:DescribeClusters","Resource":"*"}}"#.to_string(),
        },
        CanonicalCase {
            case: "a Statement list of one, which is no policy element and stays a list",
            submitted: r#"{"Statement":[{"Effect":"Allow","Action":"redshift:DescribeClusters","Resource":"*"}]}"#.to_string(),
            reported: r#"{"Statement":[{"Effect":"Allow","Action":"redshift:DescribeClusters","Resource":"*"}]}"#.to_string(),
        },
        CanonicalCase {
            case: "a one-element list holding something that is no value",
            submitted: r#"{"Statement":[{"Effect":"Allow","Action":[["redshift:DescribeClusters"]],"Resource":"*"}]}"#.to_string(),
            reported: r#"{"Statement":[{"Effect":"Allow","Action":[["redshift:DescribeClusters"]],"Resource":"*"}]}"#.to_string(),
        },
    ]
}

/// The canonical form keeps the document's own member ORDER and renders it
/// compactly: what a client diffs against the document it submitted differs only
/// where a one-element list collapsed.
#[tokio::test]
async fn the_canonical_form_keeps_the_members_in_the_order_they_were_written() {
    let emulator = Emulator::new();
    seed_cluster(&emulator, "policy-order").await;
    let namespace = state::namespace_arn("policy-order", REGION);

    // Deliberately NOT in alphabetical order, in the document and in the
    // statement, so a rendering that sorted its members would read differently
    // here at the character level.
    let submitted = format!(
        r#"{{"Version":"2012-10-17","Id":"ordered","Statement":[{{"Sid":"Zeta","Effect":"Allow","Principal":{{"AWS":["arn:aws:iam::{account}:root"]}},"Action":["redshift:GetClusterCredentials"],"Resource":["{namespace}"],"Condition":{{"StringEquals":{{"aws:SourceAccount":"{account}"}}}}}}]}}"#,
        account = account()
    );
    let expected = format!(
        r#"{{"Version":"2012-10-17","Id":"ordered","Statement":[{{"Sid":"Zeta","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"redshift:GetClusterCredentials","Resource":"{namespace}","Condition":{{"StringEquals":{{"aws:SourceAccount":"{account}"}}}}}}]}}"#,
        account = account()
    );

    let written = put(&emulator, &namespace, &submitted).await;
    assert_eq!(
        written.text("Policy").as_deref(),
        Some(expected.as_str()),
        "case \"the write\": expected the canonical document to keep the member order, got {:?}",
        written.text("Policy")
    );
    let read = get(&emulator, &namespace).await;
    assert_eq!(
        read.text("Policy").as_deref(),
        Some(expected.as_str()),
        "case \"the read\": expected the canonical document to keep the member order, got {:?}",
        read.text("Policy")
    );
}

/// The document a response reports, parsed, so two documents are compared as the
/// documents they are rather than as two renderings of one.
fn reported_document(case: &str, answer: &Answer) -> serde_json::Value {
    let policy = answer
        .text("Policy")
        .unwrap_or_else(|| panic!("case {case}: expected a Policy in {}", answer.body));
    serde_json::from_str(&policy)
        .unwrap_or_else(|_| panic!("case {case}: expected a JSON document, got {policy}"))
}

/// A one-element `Principal`, `Action` or `Resource` list is reported as the
/// single value it holds, by the write and by every later read, and everything
/// else the document carries is reported as it was written.
#[tokio::test]
async fn a_policy_is_reported_in_the_services_canonical_form() {
    let emulator = Emulator::new();
    seed_cluster(&emulator, "policy-canonical").await;
    let namespace = state::namespace_arn("policy-canonical", REGION);

    for row in canonical_table() {
        let expected: serde_json::Value = serde_json::from_str(&row.reported).unwrap_or_else(|_| {
            panic!(
                "case {}: the test's own expected document must be JSON",
                row.case
            )
        });

        let written = put(&emulator, &namespace, &row.submitted).await;
        assert_eq!(
            written.status, 200,
            "case {}: expected 200, got {} with body {}",
            row.case, written.status, written.body
        );
        assert_eq!(
            reported_document(row.case, &written),
            expected,
            "case {}: expected the write to report the canonical document, got {:?}",
            row.case,
            written.text("Policy")
        );

        let read = get(&emulator, &namespace).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected the read to answer 200, got {} with body {}",
            row.case, read.status, read.body
        );
        assert_eq!(
            reported_document(row.case, &read),
            expected,
            "case {}: expected the read to report the canonical document, got {:?}",
            row.case,
            read.text("Policy")
        );
        // The write and the read report ONE document: the characters a client
        // diffs are the same on both.
        assert_eq!(
            read.text("Policy"),
            written.text("Policy"),
            "case {}: expected the write and the read to report the same characters, got {:?} and {:?}",
            row.case,
            written.text("Policy"),
            read.text("Policy")
        );

        // Canonicalizing twice changes nothing: the document the service
        // reported, written back, is reported as the very characters it was
        // written with.
        let canonical = read.text("Policy").unwrap_or_default();
        let again = put(&emulator, &namespace, &canonical).await;
        assert_eq!(
            again.text("Policy").as_deref(),
            Some(canonical.as_str()),
            "case {}: expected a canonical document to be reported as it arrived, got {:?}",
            row.case,
            again.text("Policy")
        );
        assert_eq!(
            stored_policy(&emulator, &namespace).as_deref(),
            Some(canonical.as_str()),
            "case {}: expected the store to hold the canonical document, got {:?}",
            row.case,
            stored_policy(&emulator, &namespace)
        );
    }
}
