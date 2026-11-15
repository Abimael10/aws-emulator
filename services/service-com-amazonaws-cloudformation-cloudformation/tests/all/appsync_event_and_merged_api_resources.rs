// SPDX-License-Identifier: CC0-1.0

//! The AppSync resources a stack declares beyond a single GraphQL API's estate, end to end:
//! an Event API, the channel namespaces events travel over on it, and the link of a source API
//! into a Merged API.
//!
//! RULE: a template may declare every resource type CloudFormation publishes for a service
//! this emulator mounts, and a declared resource of such a type is created in the service that
//! OWNS it through that service's own create — so the identifiers, endpoints and states the
//! stack publishes for an `AWS::AppSync::Api`, an `AWS::AppSync::ChannelNamespace` and an
//! `AWS::AppSync::SourceApiAssociation` are the ones `GetApi`, `GetChannelNamespace` and
//! `GetSourceApiAssociation` report, and deleting the stack takes each of them away again. A
//! template declaring one of these types is therefore never refused as an unrecognized type,
//! and a declaration the owning service REFUSES fails its resource rather than being reported
//! as created.
//!
//! The rows differ along what the rule generalises over: which of the three types the template
//! declares, how many channel namespaces hang off one Event API, which authorization providers
//! and modes the API's `EventConfig` states, whether a namespace carries handler code or only
//! authorization modes, which merge type a link is made under (`AUTO_MERGE` and a declaration
//! stating none, which is the modelled default), the language the template is written in (JSON
//! or YAML), and which intrinsic each output reads a resource with (`Ref`, `Fn::GetAtt` in both
//! spellings, including the dotted path into an Event API's `Dns`).
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource. Everything goes over
//! the wire each service speaks through the one router `harness.rs` builds — the CloudFormation
//! calls over its awsQuery wire, the AppSync reads over that service's own `restJson1` wire.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack, xml_text, xml_texts, REGION,
};
use crate::stack_resources::{resources_of, wire_resources};

/// How long one poll waits before the next, and how many are made, for the wait on a resource
/// AppSync has been asked to delete.
///
/// A stack's own status transitions and the deletion a disassociate SCHEDULES are owed on their
/// clocks, so the suite waits for the STATE rather than for a span, with a deadline generous
/// enough for a loaded host running every suite of the workspace beside this one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 9_600;

/// One AWS AppSync READ over the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding names, signed for `appsync` — that model's own `sigv4Name` —
/// in the Region the stack's resources live in.
async fn appsync(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
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
fn outputs_of(case: &str, described: &str) -> BTreeMap<String, String> {
    let keys = xml_texts(described, "OutputKey");
    let values = xml_texts(described, "OutputValue");
    assert_eq!(
        keys.len(),
        values.len(),
        "case {case:?}: expected one OutputValue per OutputKey {keys:?}, got {values:?} in \
         {described}"
    );
    keys.into_iter().zip(values).collect()
}

/// One output's value, or a panic naming the case and what was there.
fn output(outputs: &BTreeMap<String, String>, key: &str, case: &str) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the output {key:?}, got {outputs:?}"))
}

/// The physical resource ids `DescribeStackResources` reports, keyed by logical id.
async fn physical_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) = resources_of(router, stack).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources({stack}) answered {status} with {body}"
    );
    wire_resources(&body)
        .into_iter()
        .filter_map(|resource| Some((resource.logical_id?, resource.physical_id?)))
        .collect()
}

/// Wait until AppSync answers `uri` with HTTP 404, asking the stack about itself in between: a
/// reporting call is what advances a deletion the stack still owes.
async fn wait_until_gone(router: &MultiServiceRouter, stack: &str, uri: &str, case: &str) {
    let mut answered = 0;
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (status, read) = appsync(router, uri).await;
        answered = status;
        last = read;
        if status == 404 {
            return;
        }
        let _ = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: expected HTTP 404 from {uri:?} once the stack had been deleted, got \
         {answered} with body {last}"
    );
}

/// The handler code one channel namespace of the table carries, which is what
/// `GetChannelNamespace` must report for it.
const CODE_HANDLERS: &str =
    "export function onPublish(ctx) { return ctx.events }\nexport function onSubscribe() { }";

/// The Event API estate a row declares: the API's name, and each namespace as
/// `(name, the handler code it carries or "")`.
struct EventApi {
    name: &'static str,
    namespaces: &'static [(&'static str, &'static str)],
}

/// The link a row declares: the `MergeType` the association must report, which is the one the
/// declaration states or the modelled default for a declaration stating none.
struct Link {
    merge_type: &'static str,
}

/// One row of the table: a template, and what the rule says the resources it declares must then
/// be in AWS AppSync.
struct Row {
    case: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The Event API estate it declares, or `None` for a row that declares none.
    event_api: Option<EventApi>,
    /// The source-API link it declares, or `None` for a row that declares none.
    link: Option<Link>,
    /// The capabilities the row's `CreateStack` acknowledges: a template declaring an IAM
    /// resource is refused without `CAPABILITY_IAM` ("Requires capabilities"), which is the
    /// client's acknowledgement rather than anything about the AppSync resources.
    capabilities: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "an Event API with one namespace carrying handler code, in YAML",
        stack: "appsync-event-api",
        template: concat!(
            "Resources:\n",
            "  Events:\n",
            "    Type: AWS::AppSync::Api\n",
            "    Properties:\n",
            "      Name: orders-events\n",
            "      OwnerContact: platform\n",
            "      EventConfig:\n",
            "        AuthProviders:\n",
            "          - AuthType: API_KEY\n",
            "        ConnectionAuthModes:\n",
            "          - AuthType: API_KEY\n",
            "        DefaultPublishAuthModes:\n",
            "          - AuthType: API_KEY\n",
            "        DefaultSubscribeAuthModes:\n",
            "          - AuthType: API_KEY\n",
            "  Default:\n",
            "    Type: AWS::AppSync::ChannelNamespace\n",
            "    Properties:\n",
            "      ApiId: !GetAtt Events.ApiId\n",
            "      Name: default\n",
            "      CodeHandlers: \"export function onPublish(ctx) { return ctx.events }\\nexport function onSubscribe() { }\"\n",
            "Outputs:\n",
            "  ApiIdOut:\n",
            "    Value: !GetAtt Events.ApiId\n",
            "  ApiArnOut:\n",
            "    Value: !GetAtt Events.ApiArn\n",
            "  ApiRef:\n",
            "    Value: !Ref Events\n",
            "  HttpDns:\n",
            "    Value: !GetAtt Events.Dns.Http\n",
            "  RealtimeDns:\n",
            "    Value: !GetAtt Events.Dns.Realtime\n",
            "  NamespaceRef:\n",
            "    Value: !Ref Default\n",
            "  NamespaceArn0:\n",
            "    Value: !GetAtt Default.ChannelNamespaceArn\n",
        ),
        event_api: Some(EventApi {
            name: "orders-events",
            namespaces: &[("default", CODE_HANDLERS)],
        }),
        link: None,
        capabilities: &[],
    },
    Row {
        case: "an Event API with two namespaces, Cognito and API-key providers and tags, in JSON",
        stack: "appsync-event-api-namespaces",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Events":{"Type":"AWS::AppSync::Api","Properties":{"#,
            r#""Name":"audit-events","#,
            r#""Tags":[{"Key":"team","Value":"audit"}],"#,
            r#""EventConfig":{"#,
            r#""AuthProviders":[{"AuthType":"API_KEY"},"#,
            r#"{"AuthType":"AMAZON_COGNITO_USER_POOLS","CognitoConfig":{"#,
            r#""UserPoolId":"us-east-1_abcdefghi","AwsRegion":"us-east-1"}}],"#,
            r#""ConnectionAuthModes":[{"AuthType":"API_KEY"},"#,
            r#"{"AuthType":"AMAZON_COGNITO_USER_POOLS"}],"#,
            r#""DefaultPublishAuthModes":[{"AuthType":"API_KEY"}],"#,
            r#""DefaultSubscribeAuthModes":[{"AuthType":"AMAZON_COGNITO_USER_POOLS"}]}}},"#,
            r#""Orders":{"Type":"AWS::AppSync::ChannelNamespace","Properties":{"#,
            r#""ApiId":{"Fn::GetAtt":["Events","ApiId"]},"Name":"orders","#,
            r#""PublishAuthModes":[{"AuthType":"API_KEY"}],"#,
            r#""SubscribeAuthModes":[{"AuthType":"AMAZON_COGNITO_USER_POOLS"}]}},"#,
            r#""Shipments":{"Type":"AWS::AppSync::ChannelNamespace","Properties":{"#,
            r#""ApiId":{"Fn::GetAtt":["Events","ApiId"]},"Name":"shipments"}}},"#,
            r#""Outputs":{"#,
            r#""ApiIdOut":{"Value":{"Fn::GetAtt":["Events","ApiId"]}},"#,
            r#""ApiArnOut":{"Value":{"Fn::GetAtt":["Events","ApiArn"]}},"#,
            r#""ApiRef":{"Value":{"Ref":"Events"}},"#,
            r#""HttpDns":{"Value":{"Fn::GetAtt":["Events","Dns","Http"]}},"#,
            r#""RealtimeDns":{"Value":{"Fn::GetAtt":["Events","Dns","Realtime"]}},"#,
            r#""NamespaceRef":{"Value":{"Ref":"Orders"}},"#,
            r#""NamespaceArn0":{"Value":{"Fn::GetAtt":["Orders","ChannelNamespaceArn"]}},"#,
            r#""NamespaceArn1":{"Value":{"Fn::GetAtt":["Shipments","ChannelNamespaceArn"]}}}}"#,
        ),
        event_api: Some(EventApi {
            name: "audit-events",
            namespaces: &[("orders", ""), ("shipments", "")],
        }),
        link: None,
        capabilities: &[],
    },
    Row {
        case: "a Merged API auto-merging a source API, in JSON",
        stack: "appsync-merged-auto",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Role":{"Type":"AWS::IAM::Role","Properties":{"#,
            r#""AssumeRolePolicyDocument":{"Version":"2012-10-17","Statement":[{"#,
            r#""Effect":"Allow","Principal":{"Service":"appsync.amazonaws.com"},"#,
            r#""Action":"sts:AssumeRole"}]}}},"#,
            r#""Source":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
            r#""Name":"orders-source","AuthenticationType":"API_KEY"}},"#,
            r#""SourceSchema":{"Type":"AWS::AppSync::GraphQLSchema","Properties":{"#,
            r#""ApiId":{"Fn::GetAtt":["Source","ApiId"]},"#,
            r#""Definition":"type Order { id: ID! } type Query { order(id: ID!): Order } schema { query: Query }"}},"#,
            r#""Merged":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
            r#""Name":"orders-merged","AuthenticationType":"API_KEY","ApiType":"MERGED","#,
            r#""MergedApiExecutionRoleArn":{"Fn::GetAtt":["Role","Arn"]}}},"#,
            r#""Link":{"Type":"AWS::AppSync::SourceApiAssociation","DependsOn":["SourceSchema"],"#,
            r#""Properties":{"#,
            r#""MergedApiIdentifier":{"Fn::GetAtt":["Merged","ApiId"]},"#,
            r#""SourceApiIdentifier":{"Fn::GetAtt":["Source","ApiId"]},"#,
            r#""Description":"the orders link","#,
            r#""SourceApiAssociationConfig":{"MergeType":"AUTO_MERGE"}}}},"#,
            r#""Outputs":{"#,
            r#""AssociationRef":{"Value":{"Ref":"Link"}},"#,
            r#""AssociationArnOut":{"Value":{"Fn::GetAtt":["Link","AssociationArn"]}},"#,
            r#""AssociationIdOut":{"Value":{"Fn::GetAtt":["Link","AssociationId"]}},"#,
            r#""MergedApiIdOut":{"Value":{"Fn::GetAtt":["Link","MergedApiId"]}},"#,
            r#""MergedApiArnOut":{"Value":{"Fn::GetAtt":["Link","MergedApiArn"]}},"#,
            r#""SourceApiIdOut":{"Value":{"Fn::GetAtt":["Link","SourceApiId"]}},"#,
            r#""SourceApiArnOut":{"Value":{"Fn::GetAtt":["Link","SourceApiArn"]}},"#,
            r#""StatusOut":{"Value":{"Fn::GetAtt":["Link","SourceApiAssociationStatus"]}}}}"#,
        ),
        event_api: None,
        link: Some(Link {
            merge_type: "AUTO_MERGE",
        }),
        capabilities: &["CAPABILITY_IAM"],
    },
    Row {
        case: "a Merged API linked to a source API with no merge type stated, in YAML",
        stack: "appsync-merged-default",
        template: concat!(
            "Resources:\n",
            "  Role:\n",
            "    Type: AWS::IAM::Role\n",
            "    Properties:\n",
            "      AssumeRolePolicyDocument:\n",
            "        Version: \"2012-10-17\"\n",
            "        Statement:\n",
            "          - Effect: Allow\n",
            "            Principal:\n",
            "              Service: appsync.amazonaws.com\n",
            "            Action: sts:AssumeRole\n",
            "  Source:\n",
            "    Type: AWS::AppSync::GraphQLApi\n",
            "    Properties:\n",
            "      Name: notes-source\n",
            "      AuthenticationType: API_KEY\n",
            "  Merged:\n",
            "    Type: AWS::AppSync::GraphQLApi\n",
            "    Properties:\n",
            "      Name: notes-merged\n",
            "      AuthenticationType: API_KEY\n",
            "      ApiType: MERGED\n",
            "      MergedApiExecutionRoleArn: !GetAtt Role.Arn\n",
            "  Link:\n",
            "    Type: AWS::AppSync::SourceApiAssociation\n",
            "    Properties:\n",
            "      MergedApiIdentifier: !GetAtt Merged.ApiId\n",
            "      SourceApiIdentifier: !GetAtt Source.ApiId\n",
            "Outputs:\n",
            "  AssociationRef:\n",
            "    Value: !Ref Link\n",
            "  AssociationArnOut:\n",
            "    Value: !GetAtt Link.AssociationArn\n",
            "  AssociationIdOut:\n",
            "    Value: !GetAtt Link.AssociationId\n",
            "  MergedApiIdOut:\n",
            "    Value: !GetAtt Link.MergedApiId\n",
            "  MergedApiArnOut:\n",
            "    Value: !GetAtt Link.MergedApiArn\n",
            "  SourceApiIdOut:\n",
            "    Value: !GetAtt Link.SourceApiId\n",
            "  SourceApiArnOut:\n",
            "    Value: !GetAtt Link.SourceApiArn\n",
            "  StatusOut:\n",
            "    Value: !GetAtt Link.SourceApiAssociationStatus\n",
        ),
        event_api: None,
        // "Valid merge types are MANUAL_MERGE (default) and AUTO_MERGE"
        // (`SourceApiAssociationConfig$mergeType`).
        link: Some(Link {
            merge_type: "MANUAL_MERGE",
        }),
        capabilities: &["CAPABILITY_IAM"],
    },
];

/// Every row: the stack reaches `CREATE_COMPLETE`, the values it publishes are the ones AppSync
/// answered, every resource the template declared is read back through that service's own reads,
/// and deleting the stack takes each of them away again.
#[tokio::test]
async fn a_stack_provisions_the_event_and_merged_api_resources_it_declares() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let acknowledged: Vec<(String, String)> = row
            .capabilities
            .iter()
            .enumerate()
            .map(|(position, capability)| {
                member(&format!("Capabilities.member.{}", position + 1), capability)
            })
            .collect();
        let (status, body) = create_stack(&router, row.stack, row.template, &acknowledged).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to be created, got {described}"
        );
        let outputs = outputs_of(case, &described);
        let physical = physical_ids(&router, case, row.stack).await;

        // The one resource whose disappearance the deletion half of the row waits on.
        let mut deleted_uri = String::new();

        if let Some(event_api) = &row.event_api {
            let api_id = output(&outputs, "ApiIdOut", case);
            let uri = format!("/v2/apis/{api_id}");
            let (status, read) = appsync(&router, &uri).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetApi on the id the stack published \
                 ({api_id:?}), got {status} with body {read}"
            );
            let api = read.get("api").cloned().unwrap_or(Value::Null);
            assert_eq!(
                api.get("name").and_then(Value::as_str),
                Some(event_api.name),
                "case {case:?}: expected the Event API to report the declared name, got {api}"
            );
            assert_eq!(
                api.get("apiId").and_then(Value::as_str),
                Some(api_id.as_str()),
                "case {case:?}: expected the API to answer to the published id, got {api}"
            );
            // `Ref` returns the `Api` ID, and the published ARN is the one AppSync built.
            assert_eq!(
                output(&outputs, "ApiRef", case),
                api_id,
                "case {case:?}: expected the Ref output to be the Api ID, got {api}"
            );
            assert_eq!(
                output(&outputs, "ApiArnOut", case),
                api.get("apiArn")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "case {case:?}: expected the ARN output to be the one AppSync built, got {api}"
            );
            assert_eq!(
                physical.get("Events").map(String::as_str),
                Some(api_id.as_str()),
                "case {case:?}: expected the API's physical id to be the Api ID, got {physical:?}"
            );
            // Each endpoint output is the DNS record AppSync serves that endpoint at — never a
            // value CloudFormation invented.
            for (key, record) in [("HttpDns", "HTTP"), ("RealtimeDns", "REALTIME")] {
                let published = output(&outputs, key, case);
                let served = api
                    .get("dns")
                    .and_then(|dns| dns.get(record))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_eq!(
                    published, served,
                    "case {case:?}: expected output {key:?} to be the {record} DNS record AppSync \
                     serves ({served:?}), got {published:?}"
                );
                assert!(
                    !published.is_empty(),
                    "case {case:?}: expected output {key:?} to be an address a client can dial, \
                     got {published:?}"
                );
            }
            // ...and every namespace the row declares is one AppSync holds on that API, under
            // the ARN the stack published for it.
            for (position, (name, handlers)) in event_api.namespaces.iter().enumerate() {
                let (status, read) =
                    appsync(&router, &format!("/v2/apis/{api_id}/channelNamespaces/{name}")).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from GetChannelNamespace {name:?}, got \
                     {status} with body {read}"
                );
                let namespace = read.get("channelNamespace").cloned().unwrap_or(Value::Null);
                assert_eq!(
                    namespace.get("name").and_then(Value::as_str),
                    Some(*name),
                    "case {case:?}: expected the namespace to report the declared name, got \
                     {namespace}"
                );
                assert_eq!(
                    namespace.get("apiId").and_then(Value::as_str),
                    Some(api_id.as_str()),
                    "case {case:?}: expected the namespace to sit on the API the stack created, \
                     got {namespace}"
                );
                let declared_handlers = (!handlers.is_empty()).then_some(*handlers);
                assert_eq!(
                    namespace.get("codeHandlers").and_then(Value::as_str),
                    declared_handlers,
                    "case {case:?}: expected the namespace to carry the declared handlers, got \
                     {namespace}"
                );
                let published = output(&outputs, &format!("NamespaceArn{position}"), case);
                assert_eq!(
                    namespace.get("channelNamespaceArn").and_then(Value::as_str),
                    Some(published.as_str()),
                    "case {case:?}: expected the published ARN to be the one AppSync built, got \
                     {namespace}"
                );
            }
            // `Ref` on a namespace returns its ARN, which is the resource's physical id.
            assert_eq!(
                output(&outputs, "NamespaceRef", case),
                output(&outputs, "NamespaceArn0", case),
                "case {case:?}: expected the namespace's Ref to be its ARN, got {outputs:?}"
            );
            deleted_uri = uri;
        }

        if let Some(link) = &row.link {
            let merged_api_id = output(&outputs, "MergedApiIdOut", case);
            let association_id = output(&outputs, "AssociationIdOut", case);
            let uri =
                format!("/v1/mergedApis/{merged_api_id}/sourceApiAssociations/{association_id}");
            let (status, read) = appsync(&router, &uri).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetSourceApiAssociation on the pair the \
                 stack published ({uri:?}), got {status} with body {read}"
            );
            let association = read
                .get("sourceApiAssociation")
                .cloned()
                .unwrap_or(Value::Null);
            // Each side of the link is the API the same template declared, as the stack's own
            // resource records name them.
            assert_eq!(
                physical.get("Merged").map(String::as_str),
                association.get("mergedApiArn").and_then(Value::as_str),
                "case {case:?}: expected the Merged API's physical id to be the ARN the link \
                 names, got {association}"
            );
            assert_eq!(
                physical.get("Source").map(String::as_str),
                association.get("sourceApiArn").and_then(Value::as_str),
                "case {case:?}: expected the source API's physical id to be the ARN the link \
                 names, got {association}"
            );
            for (key, member_name) in [
                ("AssociationArnOut", "associationArn"),
                ("AssociationIdOut", "associationId"),
                ("MergedApiIdOut", "mergedApiId"),
                ("MergedApiArnOut", "mergedApiArn"),
                ("SourceApiIdOut", "sourceApiId"),
                ("SourceApiArnOut", "sourceApiArn"),
                ("StatusOut", "sourceApiAssociationStatus"),
            ] {
                let published = output(&outputs, key, case);
                assert_eq!(
                    association.get(member_name).and_then(Value::as_str),
                    Some(published.as_str()),
                    "case {case:?}: expected output {key:?} to be the {member_name} AppSync \
                     reports, got {association}"
                );
            }
            // `Ref` returns the association's ARN, which is also the resource's physical id.
            assert_eq!(
                output(&outputs, "AssociationRef", case),
                output(&outputs, "AssociationArnOut", case),
                "case {case:?}: expected the link's Ref to be its ARN, got {outputs:?}"
            );
            assert_eq!(
                physical.get("Link").map(String::as_str),
                Some(output(&outputs, "AssociationRef", case).as_str()),
                "case {case:?}: expected the link's physical id to be its ARN, got {physical:?}"
            );
            // The link was made under the merge type the declaration states, and the merge it
            // scheduled is one AppSync carried out — which is what the stack waited for.
            assert_eq!(
                association
                    .get("sourceApiAssociationConfig")
                    .and_then(|config| config.get("mergeType"))
                    .and_then(Value::as_str),
                Some(link.merge_type),
                "case {case:?}: expected the declared merge type, got {association}"
            );
            assert_eq!(
                association
                    .get("sourceApiAssociationStatus")
                    .and_then(Value::as_str),
                Some("MERGE_SUCCESS"),
                "case {case:?}: expected the scheduled merge to have been carried out, got \
                 {association}"
            );
            deleted_uri = uri;
        }

        // Deleting the stack takes the resources it created away again — the association's
        // deletion being one AppSync SCHEDULES, which the teardown waits out.
        let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        wait_until_gone(&router, row.stack, &deleted_uri, case).await;
    }
}

/// A declaration AppSync REFUSES is not reported as created: a source API linked into an API
/// that is not a Merged API is a request that service refuses, so the stack fails carrying that
/// refusal instead of publishing a link nothing made.
#[tokio::test]
async fn a_link_into_an_api_that_is_not_merged_fails_the_stack() {
    let router = router();
    let stack = "appsync-link-to-plain-api";
    let case = "a source API linked into a plain GraphQL API";
    let template = concat!(
        r#"{"Resources":{"#,
        r#""Source":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
        r#""Name":"link-source","AuthenticationType":"API_KEY"}},"#,
        r#""Target":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
        r#""Name":"link-target","AuthenticationType":"API_KEY"}},"#,
        r#""Link":{"Type":"AWS::AppSync::SourceApiAssociation","Properties":{"#,
        r#""MergedApiIdentifier":{"Fn::GetAtt":["Target","ApiId"]},"#,
        r#""SourceApiIdentifier":{"Fn::GetAtt":["Source","ApiId"]}}}}}"#,
    );
    let (status, body) = create_stack(&router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    // The stack's creation does not succeed, and the link's own event carries its failure.
    let described = settled_stack(case, &router, REGION, stack).await;
    let reported = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_ne!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the stack's creation to fail, got {described}"
    );
    let (status, events) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {events}"
    );
    let failed: Vec<String> = xml_texts(&events, "LogicalResourceId")
        .into_iter()
        .zip(xml_texts(&events, "ResourceStatus"))
        .filter(|(_, status)| status == "CREATE_FAILED")
        .map(|(logical_id, _)| logical_id)
        .collect();
    assert!(
        failed.contains(&"Link".to_string()),
        "case {case:?}: expected the link's own resource to have failed, got {failed:?} in \
         {events}"
    );
}
