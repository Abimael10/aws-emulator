// SPDX-License-Identifier: CC0-1.0

//! What a SUCCESSFUL reply of this service carries, end to end over the wire.
//!
//! RULES under test, each over a table that spans the class it generalises over:
//!
//! * A mutation whose modelled output is `smithy.api#Unit` has nothing to report, so
//!   it answers the status that says exactly that — 204 No Content with no body at
//!   all. The two tagging mutations are this service's operations of that shape
//!   (`smithy_query(query="operation", operation="TagResource,UntagResource")`:
//!   output `Unit`), and the reader beside them, which does carry a body, still
//!   answers 200.
//! * A read that reports a resource's TAG SET reports the set itself — the empty map
//!   for a resource holding no tags — rather than leaving the member out, so a client
//!   cannot mistake "no tags" for "tags not reported" and the two readers of one map
//!   never disagree.
//! * A read of a cluster's resource-based policy hands the document back in the
//!   canonical LAYOUT a service reports an attached policy in, which depends on the
//!   document and not on the bytes the caller submitted it in: the same policy written
//!   compactly or over lines comes back identically, so a client that diffs or hashes
//!   what it last read sees a change only when the policy changed.
//! * A `policyVersion` names ONE stored policy document: it is reported unchanged by
//!   every read of that policy, a write issues one no earlier write of any cluster
//!   issued, and a version issued for one cluster selects nothing on another.
//!
//! The client is the shared `Dsql` harness in `harness.rs`. Nothing here waits a
//! fixed time for the clock: a case that needs a settled cluster polls the cluster's
//! own status until it has moved, and the policy rows that need none attach their
//! document inline at create time.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::harness::{expected_arn, Answer, Dsql, TAG_MUTATION_SUCCESS_STATUS};
use crate::resource_tags::untag;

/// Far past the span this service owes a cluster its `CREATING -> ACTIVE`
/// transition in, so a loaded or slow host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often [`settled`] looks again while it waits for that transition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The status `ListTagsForResource` answers a served read with: it reports a tag map
/// in a BODY, so it carries the protocol's ordinary success status rather than the
/// one a reply with no body answers.
const TAG_READ_SUCCESS_STATUS: u16 = 200;

/// A cluster whose `CREATING -> ACTIVE` transition the clock has already made, found
/// by asking for the cluster's status until it has moved.
///
/// The wait is on the CONDITION — never on a fixed duration — and bounded by
/// [`SETTLE_DEADLINE`].
async fn settled(dsql: &Dsql, identifier: &str) {
    let deadline = SystemTime::now() + SETTLE_DEADLINE;
    loop {
        let read = dsql.get(identifier).await;
        if read.status == 200 && read.str("status") == "ACTIVE" {
            return;
        }
        assert!(
            SystemTime::now() < deadline,
            "cluster {identifier:?} must reach ACTIVE, last read {} {:?}",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `targetDefinition` naming a Kinesis destination, as `CreateStream` requires.
fn kinesis_target(region: &str) -> Value {
    let account = engine::EMULATOR_ACCOUNT_ID;
    json!({"kinesis": {
        "streamArn": format!("arn:aws:kinesis:{region}:{account}:stream/changes"),
        "roleArn": format!("arn:aws:iam::{account}:role/dsql-cdc"),
    }})
}

/// One tagging MUTATION, as a client calls it.
enum Mutation {
    /// `POST /tags/{resourceArn}` with the pairs to merge.
    Tag(Value),
    /// `DELETE /tags/{resourceArn}?tagKeys=…` with the keys to remove.
    Untag(Vec<String>),
}

impl Mutation {
    async fn call(&self, dsql: &Dsql, arn: &str) -> Answer {
        match self {
            Self::Tag(tags) => dsql.tag_resource(arn, json!({"tags": tags})).await,
            Self::Untag(keys) => untag(dsql, arn, keys).await,
        }
    }
}

/// One tagging mutation row: the call, and the resource kind it is addressed at.
struct MutationCase {
    name: &'static str,
    /// Whether the ARN names the cluster or the stream of it.
    on_stream: bool,
    mutation: Mutation,
}

fn mutation_cases() -> Vec<MutationCase> {
    vec![
        MutationCase {
            name: "one pair onto a cluster that already holds tags",
            on_stream: false,
            mutation: Mutation::Tag(json!({"owner": "team-a"})),
        },
        MutationCase {
            name: "a pair that overwrites a key the cluster holds",
            on_stream: false,
            mutation: Mutation::Tag(json!({"env": "staging"})),
        },
        MutationCase {
            name: "several pairs at once",
            on_stream: false,
            mutation: Mutation::Tag(json!({"owner": "team-a", "tier": "2", "scope": "cdc"})),
        },
        MutationCase {
            name: "an empty pair set, which TagMap's 0 minimum admits",
            on_stream: false,
            mutation: Mutation::Tag(json!({})),
        },
        MutationCase {
            name: "one pair onto a stream of the cluster",
            on_stream: true,
            mutation: Mutation::Tag(json!({"owner": "team-a"})),
        },
        MutationCase {
            name: "a key the cluster holds",
            on_stream: false,
            mutation: Mutation::Untag(vec!["env".to_string()]),
        },
        MutationCase {
            name: "a key the cluster never held",
            on_stream: false,
            mutation: Mutation::Untag(vec!["never-set".to_string()]),
        },
        MutationCase {
            name: "several keys at once, held and never held",
            on_stream: false,
            mutation: Mutation::Untag(vec![
                "env".to_string(),
                "never-set".to_string(),
                "cost-centre".to_string(),
            ]),
        },
        MutationCase {
            name: "a key the stream of the cluster holds",
            on_stream: true,
            mutation: Mutation::Untag(vec!["env".to_string()]),
        },
    ]
}

/// RULE: a tagging mutation reports the write in the STATUS alone — 204 No Content,
/// with no body — whatever it was asked to write, on whichever tagged resource, and
/// however many times it is repeated; the reader beside it still answers 200 with the
/// tag map in its body.
#[tokio::test]
async fn a_tagging_mutation_answers_no_content_with_no_body() {
    let region = "us-east-1";
    let dsql = Dsql::in_region(region);
    let created = dsql
        .create(json!({
            "deletionProtectionEnabled": false,
            "tags": {"env": "prod", "cost-centre": "42"},
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateCluster must answer 200, got {} {:?}",
        created.status, created.body
    );
    let cluster = created.str("identifier");
    let cluster_arn = created.str("arn");
    // A stream is a write of the cluster's record, so the cluster takes one once the
    // clock has settled the status it owes.
    settled(&dsql, &cluster).await;

    let stream = dsql
        .create_stream(
            &cluster,
            json!({
                "format": "JSON",
                "ordering": "UNORDERED",
                "targetDefinition": kinesis_target(region),
                "tags": {"env": "prod"},
            }),
        )
        .await;
    assert_eq!(
        stream.status, 200,
        "CreateStream must answer 200, got {} {:?}",
        stream.status, stream.body
    );
    let stream_arn = stream.str("arn");

    for case in mutation_cases() {
        let arn = match case.on_stream {
            true => &stream_arn,
            false => &cluster_arn,
        };
        // Twice: the status a mutation answers is the same whether or not the call
        // changed anything, because `tagKeys` and `tags` name an end state.
        for attempt in 1..=2 {
            let answer = case.mutation.call(&dsql, arn).await;
            assert_eq!(
                answer.status, TAG_MUTATION_SUCCESS_STATUS,
                "case {:?} (attempt {attempt}): expected HTTP {TAG_MUTATION_SUCCESS_STATUS}, \
                 got {} {:?}",
                case.name, answer.status, answer.body
            );
            assert_eq!(
                answer.body,
                Value::Null,
                "case {:?} (attempt {attempt}): a Unit output carries no body, got {:?}",
                case.name,
                answer.body
            );
            assert!(
                answer.error.is_none(),
                "case {:?} (attempt {attempt}): expected no error, got {:?}",
                case.name,
                answer.error
            );
        }
    }

    // The reader on the same path reports a map, so it answers with a body and the
    // status that carries one — the 204 above is about the Unit output and not about
    // the route.
    let listed = dsql.list_tags_for_resource(&cluster_arn).await;
    assert_eq!(
        (listed.status, listed.get("tags").is_object()),
        (TAG_READ_SUCCESS_STATUS, true),
        "case \"the reader on the same path\": expected HTTP {TAG_READ_SUCCESS_STATUS} with a \
         tag map, got {} {:?}",
        listed.status,
        listed.body
    );
}

/// One created resource's tag set and what every reader of it must report.
struct TagSetCase {
    name: &'static str,
    /// The `tags` member the create carries, or `Value::Null` to omit it.
    created_with: Value,
    /// The map every reader must then report.
    reported: Value,
}

fn tag_set_cases() -> Vec<TagSetCase> {
    vec![
        TagSetCase {
            name: "created with no tags member at all",
            created_with: Value::Null,
            reported: json!({}),
        },
        TagSetCase {
            name: "created with an explicitly empty map",
            created_with: json!({}),
            reported: json!({}),
        },
        TagSetCase {
            name: "created with one tag",
            created_with: json!({"env": "test"}),
            reported: json!({"env": "test"}),
        },
        TagSetCase {
            name: "created with several tags, one of them an empty value",
            created_with: json!({"env": "prod", "owner": "team-a", "note": ""}),
            reported: json!({"env": "prod", "owner": "team-a", "note": ""}),
        },
    ]
}

/// RULE: every reader of a resource's tags reports the SET — the empty map included —
/// so the resource's own reader and the shared tag reader agree for a resource that
/// holds tags and for one that holds none, both for a cluster and for a stream of
/// one, and still agree once every tag has been removed.
#[tokio::test]
async fn every_reader_reports_the_tag_set_itself() {
    let region = "eu-west-1";
    for case in tag_set_cases() {
        let dsql = Dsql::in_region(region);
        let mut request = json!({"deletionProtectionEnabled": false});
        if !case.created_with.is_null() {
            request["tags"] = case.created_with.clone();
        }
        let created = dsql.create(request).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreateCluster must answer 200, got {} {:?}",
            case.name, created.status, created.body
        );
        let cluster = created.str("identifier");
        let cluster_arn = created.str("arn");
        settled(&dsql, &cluster).await;

        let mut stream_request = json!({
            "format": "JSON",
            "ordering": "UNORDERED",
            "targetDefinition": kinesis_target(region),
        });
        if !case.created_with.is_null() {
            stream_request["tags"] = case.created_with.clone();
        }
        let stream = dsql.create_stream(&cluster, stream_request).await;
        assert_eq!(
            stream.status, 200,
            "case {:?}: CreateStream must answer 200, got {} {:?}",
            case.name, stream.status, stream.body
        );
        let stream_identifier = stream.str("streamIdentifier");
        let stream_arn = stream.str("arn");

        // (which resource, its own reader's answer, the shared reader's answer)
        let readers = [
            (
                "a cluster",
                dsql.get(&cluster).await,
                dsql.list_tags_for_resource(&cluster_arn).await,
            ),
            (
                "a stream of it",
                dsql.get_stream(&cluster, &stream_identifier).await,
                dsql.list_tags_for_resource(&stream_arn).await,
            ),
        ];
        for (resource, own, listed) in readers {
            assert_eq!(
                own.get("tags"),
                case.reported,
                "case {:?} on {resource}: the resource's own reader must report {:?}, got {:?}",
                case.name,
                case.reported,
                own.get("tags")
            );
            assert_eq!(
                listed.get("tags"),
                case.reported,
                "case {:?} on {resource}: ListTagsForResource must report {:?}, got {:?}",
                case.name,
                case.reported,
                listed.get("tags")
            );
        }

        // And once every key is removed, the readers report the EMPTY map rather than
        // nothing at all.
        let held: Vec<String> = case
            .reported
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default();
        if held.is_empty() {
            continue;
        }
        for (resource, arn, own) in [
            ("a cluster", &cluster_arn, cluster.clone()),
            ("a stream of it", &stream_arn, stream_identifier.clone()),
        ] {
            let removed = untag(&dsql, arn, &held).await;
            assert_eq!(
                removed.status, TAG_MUTATION_SUCCESS_STATUS,
                "case {:?} on {resource}: UntagResource must answer \
                 {TAG_MUTATION_SUCCESS_STATUS}, got {} {:?}",
                case.name, removed.status, removed.body
            );
            let read = match resource {
                "a cluster" => dsql.get(&own).await,
                _ => dsql.get_stream(&cluster, &own).await,
            };
            assert_eq!(
                read.get("tags"),
                json!({}),
                "case {:?} on {resource}: an emptied tag set reads as the empty map, got {:?}",
                case.name,
                read.get("tags")
            );
            let listed = dsql.list_tags_for_resource(arn).await;
            assert_eq!(
                listed.get("tags"),
                json!({}),
                "case {:?} on {resource}: ListTagsForResource must agree, got {:?}",
                case.name,
                listed.get("tags")
            );
        }
    }
}

/// One policy document, written in the several ways a caller can write it, and the
/// one rendering every read of it must answer.
struct RenderingCase {
    name: &'static str,
    /// The bytes the document is submitted in, one row per form.
    written: Vec<String>,
    /// The bytes every read of the attached document must answer.
    rendered: &'static str,
}

fn rendering_cases() -> Vec<RenderingCase> {
    let account = engine::EMULATOR_ACCOUNT_ID;
    vec![
        RenderingCase {
            name: "a statement granting one action to one principal",
            written: vec![
                format!(
                    "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
                     \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
                     \"Action\":[\"dsql:DbConnect\"],\"Resource\":\"*\"}}]}}"
                ),
                format!(
                    "{{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [\n    {{\n      \
                     \"Effect\": \"Allow\",\n      \"Principal\": {{\n        \"AWS\": \
                     \"arn:aws:iam::{account}:root\"\n      }},\n      \"Action\": [\n        \
                     \"dsql:DbConnect\"\n      ],\n      \"Resource\": \"*\"\n    }}\n  ]\n}}"
                ),
                format!(
                    "{{\t\"Version\" :\"2012-10-17\" ,   \"Statement\":[ {{ \"Effect\" : \
                     \"Allow\", \"Principal\" : {{ \"AWS\" : \"arn:aws:iam::{account}:root\" }}, \
                     \"Action\" : [ \"dsql:DbConnect\" ] , \"Resource\":\"*\" }} ] }}"
                ),
            ],
            rendered: concat!(
                "{\n",
                "  \"Version\" : \"2012-10-17\",\n",
                "  \"Statement\" : [ {\n",
                "    \"Effect\" : \"Allow\",\n",
                "    \"Principal\" : {\n",
                "      \"AWS\" : \"arn:aws:iam::000000000000:root\"\n",
                "    },\n",
                "    \"Action\" : [ \"dsql:DbConnect\" ],\n",
                "    \"Resource\" : \"*\"\n",
                "  } ]\n",
                "}"
            ),
        },
        RenderingCase {
            name: "a document whose members are in no sorted order, with an empty \
                   statement list",
            written: vec![
                "{\"Statement\":[],\"Version\":\"2012-10-17\"}".to_string(),
                "{\n \"Statement\" : [\n ],\n \"Version\" : \"2012-10-17\"\n}".to_string(),
            ],
            rendered: "{\n  \"Statement\" : [ ],\n  \"Version\" : \"2012-10-17\"\n}",
        },
        RenderingCase {
            name: "two statements, a list of actions and a nested condition",
            written: vec![format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"AllowConnect\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
                 \"Action\":[\"dsql:DbConnect\",\"dsql:DbConnectAdmin\"],\"Resource\":\"*\",\
                 \"Condition\":{{\"Bool\":{{\"aws:SecureTransport\":\"true\"}}}}}},\
                 {{\"Sid\":\"DenyOther\",\"Effect\":\"Deny\",\"Principal\":\"*\",\
                 \"NotAction\":\"dsql:DbConnect\",\"Resource\":\"*\"}}]}}"
            )],
            rendered: concat!(
                "{\n",
                "  \"Version\" : \"2012-10-17\",\n",
                "  \"Statement\" : [ {\n",
                "    \"Sid\" : \"AllowConnect\",\n",
                "    \"Effect\" : \"Allow\",\n",
                "    \"Principal\" : {\n",
                "      \"AWS\" : \"arn:aws:iam::000000000000:root\"\n",
                "    },\n",
                "    \"Action\" : [ \"dsql:DbConnect\", \"dsql:DbConnectAdmin\" ],\n",
                "    \"Resource\" : \"*\",\n",
                "    \"Condition\" : {\n",
                "      \"Bool\" : {\n",
                "        \"aws:SecureTransport\" : \"true\"\n",
                "      }\n",
                "    }\n",
                "  }, {\n",
                "    \"Sid\" : \"DenyOther\",\n",
                "    \"Effect\" : \"Deny\",\n",
                "    \"Principal\" : \"*\",\n",
                "    \"NotAction\" : \"dsql:DbConnect\",\n",
                "    \"Resource\" : \"*\"\n",
                "  } ]\n",
                "}"
            ),
        },
    ]
}

/// RULE: a read of a cluster's policy answers the DOCUMENT in the canonical layout a
/// service reports an attached policy in — one member per line, two spaces of
/// indentation per enclosing object, `" : "` between a member and its value and a
/// sequence written inline — so the same document submitted in any form reads back
/// identically, and two reads of one attached policy answer the same bytes.
///
/// The rows attach their document inline at create time, which is one of the two
/// entry points that store one, so no case waits for the clock.
#[tokio::test]
async fn a_policy_read_renders_the_stored_document_in_the_services_own_form() {
    let dsql = Dsql::new();
    for case in rendering_cases() {
        for (index, written) in case.written.iter().enumerate() {
            let created = dsql
                .create(json!({
                    "deletionProtectionEnabled": false,
                    "policy": written,
                    // The rows are about the RENDERING of a document, not about which
                    // documents the lockout safety check admits, so every row accepts
                    // the risk explicitly and that check is out of the way.
                    "bypassPolicyLockoutSafetyCheck": true,
                }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {:?} (form {index}): CreateCluster must attach the document, got {} {:?}",
                case.name, created.status, created.body
            );
            let identifier = created.str("identifier");

            let read = dsql.get_policy(&identifier).await;
            assert_eq!(
                read.status, 200,
                "case {:?} (form {index}): GetClusterPolicy must answer 200, got {} {:?}",
                case.name, read.status, read.body
            );
            assert_eq!(
                read.str("policy"),
                case.rendered,
                "case {:?} (form {index}): expected {:?}, got {:?}",
                case.name,
                case.rendered,
                read.str("policy")
            );

            // A read changes nothing, so reading again answers the same bytes.
            let again = dsql.get_policy(&identifier).await;
            assert_eq!(
                again.str("policy"),
                read.str("policy"),
                "case {:?} (form {index}): a second read must answer the same document, got {:?}",
                case.name,
                again.str("policy")
            );
        }
    }
}

/// RULE: a `policyVersion` names ONE stored policy document — every read of that
/// policy reports it unchanged, a write issues a version no earlier write of any
/// cluster issued, and a version issued for one cluster selects nothing on another,
/// so a caller holding one cannot condition a write on another cluster's policy with
/// it.
#[tokio::test]
async fn a_policy_version_names_one_stored_policy_document() {
    let region = "us-west-2";
    let dsql = Dsql::in_region(region);
    let account = engine::EMULATOR_ACCOUNT_ID;
    let document = |action: &str| {
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
             \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
             \"Action\":\"{action}\",\"Resource\":\"*\"}}]}}"
        )
    };

    // Two clusters, so "a version of one cluster is not a version of the other" is
    // asserted rather than assumed, and several writes on each.
    let clusters = [
        dsql.create(json!({"deletionProtectionEnabled": false})).await,
        dsql.create(json!({"deletionProtectionEnabled": false})).await,
    ];
    let mut identifiers = Vec::new();
    for created in &clusters {
        assert_eq!(
            created.status, 200,
            "CreateCluster must answer 200, got {} {:?}",
            created.status, created.body
        );
        identifiers.push(created.str("identifier"));
    }
    for identifier in &identifiers {
        settled(&dsql, identifier).await;
    }

    let mut issued: BTreeSet<String> = BTreeSet::new();
    let mut current: Vec<String> = Vec::new();
    for identifier in &identifiers {
        for round in 0..3 {
            let action = match round % 2 {
                0 => "dsql:DbConnect",
                _ => "dsql:DbConnectAdmin",
            };
            let put = dsql
                .put_policy(identifier, json!({"policy": document(action)}))
                .await;
            assert_eq!(
                put.status, 200,
                "case cluster {identifier:?} round {round}: PutClusterPolicy must answer 200, \
                 got {} {:?}",
                put.status, put.body
            );
            let version = put.str("policyVersion");
            assert!(
                !version.is_empty(),
                "case cluster {identifier:?} round {round}: policyVersion is @required, got {:?}",
                put.body
            );
            assert!(
                issued.insert(version.clone()),
                "case cluster {identifier:?} round {round}: version {version:?} was already \
                 issued, seen {issued:?}"
            );

            // The read reports the version the write answered, and keeps reporting it.
            for read_round in 1..=2 {
                let read = dsql.get_policy(identifier).await;
                assert_eq!(
                    read.str("policyVersion"),
                    version,
                    "case cluster {identifier:?} round {round} (read {read_round}): the read must \
                     report {version:?}, got {:?}",
                    read.str("policyVersion")
                );
            }
            if round == 2 {
                current.push(version);
            }
        }
    }

    // A version is the version of ONE cluster's policy: the other cluster's current
    // version is no claim about this one, so a write conditioned on it is refused and
    // changes nothing.
    for (index, identifier) in identifiers.iter().enumerate() {
        let foreign = current[(index + 1) % current.len()].clone();
        let before = dsql.get_policy(identifier).await;
        let refused = dsql
            .put_policy(
                identifier,
                json!({
                    "policy": document("dsql:DbConnect"),
                    "expectedPolicyVersion": foreign.clone(),
                }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (409, Some("ConflictException")),
            "case cluster {identifier:?}: a write conditioned on {foreign:?} must be refused, \
             got {} {:?}",
            refused.status,
            refused.body
        );
        let after = dsql.get_policy(identifier).await;
        assert_eq!(
            (after.str("policy"), after.str("policyVersion")),
            (before.str("policy"), before.str("policyVersion")),
            "case cluster {identifier:?}: the refused write must change nothing, got {:?}",
            after.body
        );
    }

    // The ARNs the two clusters were minted with are distinct, so the rows above are
    // about two stored policies rather than one read twice.
    assert_ne!(
        expected_arn(region, &identifiers[0]),
        expected_arn(region, &identifiers[1]),
        "the two clusters must be distinct, got {identifiers:?}"
    );
}
