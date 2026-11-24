// SPDX-License-Identifier: CC0-1.0

//! What a stack PUBLISHES for a resource whose declaration reads an identifier the owning
//! service minted, after a deployment that REPLACED that resource.
//!
//! RULE: a deployment resolves its declarations against the identities IN FORCE — the
//! identifier the owning service really minted for a resource the stack already has, never the
//! stand-in CloudFormation's naming rules compute before any create has answered — and an
//! attribute a record publishes straight from one of those properties carries the value that
//! property resolves to. So every call the stack later makes ABOUT such a resource (its own
//! update, the read a drift detection makes, the delete its teardown asks for) is addressed at
//! the resource the owning service holds, and a stack whose update REPLACED a resource —
//! whose record is therefore recomputed from the template rather than carried forward — is
//! still torn down completely.
//!
//! The instance this suite is written over is an AWS AppSync GraphQL API's estate: every child
//! of the API names it by an `ApiId` its declaration reads off the API with `Fn::GetAtt`, and
//! AppSync ASSIGNS that id (`CreateGraphqlApi` takes a name and answers the `apiId` it minted),
//! so a child's record that published the stand-in instead would address an API that does not
//! exist — the delete of such a child answers "not found", the resource is left behind in
//! AppSync, and the data source it resolves against can then no longer be deleted ("Resolver
//! for type <type> and field <field> associated with data source").
//!
//! The rows differ along what the rule generalises over: WHICH child the revision replaces (a
//! resolver moved to another field, a data source renamed, and both at once), how many
//! resources one revision replaces, and whether a sibling's declaration follows the replaced
//! resource through an `Fn::GetAtt` (the resolver's `DataSourceName` reads the data source's
//! name, so renaming it changes the resolver in place).
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares or what the OWNING service reports. The CloudFormation calls go over its
//! awsQuery wire and the AppSync reads over that service's own `restJson1` wire, through the
//! one router `harness.rs` builds.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack, stack_event_reasons, xml_text, xml_texts,
    REGION,
};

/// One AWS AppSync call over the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding states, signed for `appsync` — that model's `sigv4Name` — in
/// the Region the stack's resources live in.
pub(crate) async fn appsync(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://appsync.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/appsync/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed AWS AppSync request");
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

/// The stack's outputs, keyed by `OutputKey`: the answer carries one `OutputValue` per key, so
/// the two parallel lists zip.
pub(crate) fn outputs_of(case: &str, described: &str) -> BTreeMap<String, String> {
    let keys = xml_texts(described, "OutputKey");
    let values = xml_texts(described, "OutputValue");
    assert_eq!(
        keys.len(),
        values.len(),
        "case {case:?}: expected one OutputValue per OutputKey {keys:?}, got {values:?}"
    );
    keys.into_iter().zip(values).collect()
}

/// The `PhysicalResourceId` `DescribeStackResources` reports for each logical id of a stack.
async fn physical_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources({stack}) answered {status} with {body}"
    );
    xml_texts(&body, "LogicalResourceId")
        .into_iter()
        .zip(xml_texts(&body, "PhysicalResourceId"))
        .collect()
}

/// One revision of the row's template: the schema document the API serves, the field the
/// resolver is attached to, the name the data source is created under and the description both
/// it and the API key carry.
pub(crate) struct Revision {
    pub(crate) definition: &'static str,
    pub(crate) field: &'static str,
    pub(crate) source: &'static str,
    pub(crate) description: &'static str,
}

/// The whole estate one revision declares: the API, its schema, a data source, a resolver
/// attached to one field of `Query` and an API key — each child naming the API through
/// `Fn::GetAtt`, which is the only way a template can name an id AppSync mints.
pub(crate) fn template(revision: &Revision) -> String {
    let Revision {
        definition,
        field,
        source,
        description,
    } = revision;
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::AppSync::GraphQLApi",
                "Properties": {"Name": "estate-api", "AuthenticationType": "API_KEY"},
            },
            "Schema": {
                "Type": "AWS::AppSync::GraphQLSchema",
                "Properties": {
                    "ApiId": {"Fn::GetAtt": ["Api", "ApiId"]},
                    "Definition": definition,
                },
            },
            "Source": {
                "Type": "AWS::AppSync::DataSource",
                "Properties": {
                    "ApiId": {"Fn::GetAtt": ["Api", "ApiId"]},
                    "Name": source,
                    "Type": "NONE",
                    "Description": description,
                },
            },
            "Resolver": {
                "Type": "AWS::AppSync::Resolver",
                "DependsOn": ["Schema"],
                "Properties": {
                    "ApiId": {"Fn::GetAtt": ["Api", "ApiId"]},
                    "TypeName": "Query",
                    "FieldName": field,
                    "DataSourceName": {"Fn::GetAtt": ["Source", "Name"]},
                    "Runtime": {"Name": "APPSYNC_JS", "RuntimeVersion": "1.0.0"},
                    "Code": format!(
                        "export function request(ctx) {{ return {{}}; }}\n\
                         export function response(ctx) {{ return \"{description}:\" + \
                         ctx.args.msg; }}\n"
                    ),
                },
            },
            "Key": {
                "Type": "AWS::AppSync::ApiKey",
                "Properties": {
                    "ApiId": {"Fn::GetAtt": ["Api", "ApiId"]},
                    "Description": description,
                },
            },
        },
        "Outputs": {
            "ApiIdOut": {"Value": {"Fn::GetAtt": ["Api", "ApiId"]}},
        },
    })
    .to_string()
}

/// The schema document of a revision that serves one field, and of one that serves two: the
/// resolver of a row that moves to another field needs that field to exist on `Query`.
pub(crate) const ONE_FIELD: &str = "type Query { first: String } schema { query: Query }";
const TWO_FIELDS: &str = "type Query { first: String second: String } schema { query: Query }";

/// One row of the table: the revision the stack is created with, the one it is then updated to,
/// and which of the stack's resources that update REPLACES.
struct Row {
    case: &'static str,
    stack: &'static str,
    first: Revision,
    second: Revision,
    /// The logical ids whose physical resource the second revision replaces — a declaration
    /// that restates one of the type's create-only properties (a resolver's `FieldName`, a data
    /// source's `Name`), so its record is recomputed rather than carried forward.
    replaced: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a resolver moved to another field of the same type",
        stack: "appsync-replaced-resolver",
        first: Revision {
            definition: ONE_FIELD,
            field: "first",
            source: "NoneSource",
            description: "v1",
        },
        second: Revision {
            definition: TWO_FIELDS,
            field: "second",
            source: "NoneSource",
            description: "v2",
        },
        replaced: &["Resolver"],
    },
    Row {
        case: "a data source renamed, which the resolver's DataSourceName follows",
        stack: "appsync-replaced-data-source",
        first: Revision {
            definition: ONE_FIELD,
            field: "first",
            source: "NoneSource",
            description: "v1",
        },
        second: Revision {
            definition: ONE_FIELD,
            field: "first",
            source: "OtherSource",
            description: "v2",
        },
        replaced: &["Source"],
    },
    Row {
        case: "both children replaced by one revision",
        stack: "appsync-replaced-pair",
        first: Revision {
            definition: ONE_FIELD,
            field: "first",
            source: "NoneSource",
            description: "v1",
        },
        second: Revision {
            definition: TWO_FIELDS,
            field: "second",
            source: "OtherSource",
            description: "v3",
        },
        replaced: &["Resolver", "Source"],
    },
];

/// Every row: the revision that replaces a child is deployed in full, every record the stack
/// publishes afterwards is addressed by the id AppSync minted, the owning service holds exactly
/// the children the new revision declares, the stack's own READ of a replaced resource finds it,
/// and deleting the stack takes the whole estate away.
#[tokio::test]
async fn a_replacement_publishes_the_identifiers_the_owning_service_minted() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let (status, body) = create_stack(&router, row.stack, &template(&row.first), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to create, the events said {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let api_id = outputs_of(case, &described)
            .get("ApiIdOut")
            .cloned()
            .unwrap_or_default();
        assert!(
            !api_id.is_empty(),
            "case {case:?}: expected the stack to publish the API id AppSync minted"
        );

        // The revision that REPLACES a child goes through: the replacement is created beside
        // the resource the stack had and the old one is taken down in the cleanup phase, both
        // addressed at the API AppSync holds.
        let (status, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &template(&row.second)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
        );
        let updated = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&updated, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, the events said {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        assert_eq!(
            outputs_of(case, &updated).get("ApiIdOut"),
            Some(&api_id),
            "case {case:?}: expected the API id to be the one AppSync minted, got {updated}"
        );

        // Every record the stack publishes for a child is built on that minted id — the
        // replacement's as much as the records that were carried forward.
        let physical = physical_ids(&router, case, row.stack).await;
        for logical_id in ["Schema", "Source", "Resolver", "Key"] {
            let published = physical.get(logical_id).cloned().unwrap_or_default();
            assert!(
                published.contains(&api_id),
                "case {case:?}: expected {logical_id}'s physical id to be built on the minted \
                 API id {api_id:?}, got {published:?}"
            );
        }

        // ...and AppSync holds exactly the children the new revision declares: the resolver on
        // the field it now names, resolving against the data source it now names, and that one
        // data source alone — so the replacement was created and the resource it replaced was
        // really removed.
        let (status, resolvers) =
            appsync(&router, &format!("/v1/apis/{api_id}/types/Query/resolvers")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListResolvers, got {status} with {resolvers}"
        );
        let held: Vec<&Value> = resolvers
            .get("resolvers")
            .and_then(Value::as_array)
            .map(|held| held.iter().collect())
            .unwrap_or_default();
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected the API to hold exactly the resolver the revision declares, \
             got {resolvers}"
        );
        assert_eq!(
            held[0].get("fieldName").and_then(Value::as_str),
            Some(row.second.field),
            "case {case:?}: expected the resolver on the declared field, got {resolvers}"
        );
        assert_eq!(
            held[0].get("dataSourceName").and_then(Value::as_str),
            Some(row.second.source),
            "case {case:?}: expected the resolver to resolve against the declared data source, \
             got {resolvers}"
        );
        let (status, sources) = appsync(&router, &format!("/v1/apis/{api_id}/datasources")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListDataSources, got {status} with {sources}"
        );
        let names: Vec<&str> = sources
            .get("dataSources")
            .and_then(Value::as_array)
            .map(|held| {
                held.iter()
                    .filter_map(|source| source.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec![row.second.source],
            "case {case:?}: expected the API to hold exactly the data source the revision \
             declares, got {sources}"
        );

        // The stack's own READ of a replaced resource reaches it: a drift detection asks the
        // owning service about the resource the record names, so a record addressed by a
        // stand-in would report the resource as one nobody holds.
        for logical_id in row.replaced {
            let (status, drift) = call(
                &router,
                "DetectStackResourceDrift",
                &[
                    member("StackName", row.stack),
                    member("LogicalResourceId", logical_id),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DetectStackResourceDrift({logical_id}), \
                 got {status} with {drift}"
            );
            let reported = xml_text(&drift, "StackResourceDriftStatus").unwrap_or_default();
            assert_ne!(
                reported, "DELETED",
                "case {case:?}: expected the stack's read of {logical_id} to find the resource \
                 AppSync holds, got {drift}"
            );
        }

        // ...and the teardown deletes every one of them, in the reverse of the order they were
        // created in: the resolver before the data source it resolves against, each addressed at
        // the API AppSync holds, so nothing is left behind to refuse another resource's delete.
        let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
        );
        let deleted = settled_stack(case, &router, REGION, row.stack).await;
        let reported = xml_text(&deleted, "StackStatus").unwrap_or_default();
        assert!(
            deleted.is_empty() || reported == "DELETE_COMPLETE",
            "case {case:?}: expected the stack to delete, got {reported:?}; the events said {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let (status, read) = appsync(&router, &format!("/v1/apis/{api_id}")).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected AppSync to hold no API once the stack was deleted, got \
             {status} with {read}"
        );
    }
}
