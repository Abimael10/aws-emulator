// SPDX-License-Identifier: CC0-1.0

//! The AWS Cloud Map resources a stack declares, end to end: a namespace of each of the three
//! kinds, a service of one, and an instance registered in that service.
//!
//! RULE: a template may declare every resource type CloudFormation publishes for a service this
//! emulator mounts, and a declared resource of such a type is created in the service that OWNS
//! it through that service's own create — so the identifiers a stack publishes for an
//! `AWS::ServiceDiscovery::HttpNamespace`, `::PublicDnsNamespace`, `::PrivateDnsNamespace`,
//! `::Service` and `::Instance` are the ones `ListNamespaces`, `GetNamespace`, `GetService` and
//! `GetInstance` report, the resources are usable through the API that owns them
//! (`DiscoverInstances`), deleting the stack takes each of them away, and a declaration the
//! owning service REFUSES fails its resource rather than being reported as created.
//!
//! The rows differ along what the rule generalises over: which of the three namespace kinds the
//! template declares, whether the namespace is backed by a Route 53 hosted zone, the language
//! the template is written in (JSON or YAML), which intrinsic each output reads a resource with
//! (`Ref`, `Fn::GetAtt` in both spellings), how a record's time to live is spelled (as a number
//! and as the quoted text a template may write any scalar as), how many DNS records a service
//! declares, and which attributes an instance answers discovery with.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares, or what the OWNING service reports for the resource. Everything goes over
//! the wire each service speaks through the one router `harness.rs` builds — the CloudFormation
//! calls over its awsQuery wire, the Cloud Map reads over that service's own `awsJson1_1` wire.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, router_with_engine, settled_stack, stack_event_reasons,
    xml_text, xml_texts, REGION,
};

/// How long one poll waits before the next, and how many are made, for the wait on a resource
/// the stack's deletion has been asked to take away.
///
/// A stack's own status transitions are owed on its clock, so the suite waits for the STATE
/// rather than for a span, with a deadline generous enough for a loaded host running every
/// suite of the workspace beside this one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 2_400;

/// The service shape an awsJson `X-Amz-Target` names a Cloud Map operation under (that model's
/// own service shape, `smithy_query(query="service_info")`).
const CLOUD_MAP_TARGET_PREFIX: &str = "Route53AutoNaming_v20170314";

/// The error codes Cloud Map answers for a namespace and for a service it does not hold (the
/// modelled 404s of its reads), which is what says the stack's deletion has taken one away.
const NAMESPACE_NOT_FOUND: &str = "NamespaceNotFound";
/// See [`NAMESPACE_NOT_FOUND`].
const SERVICE_NOT_FOUND: &str = "ServiceNotFound";

/// What a row's template writes where the VPC a private DNS namespace is created for goes.
///
/// A private namespace IS an association between a Route 53 private hosted zone and a VPC of
/// the account, so Cloud Map resolves that VPC in Amazon EC2 when it creates the namespace.
/// The id substituted here is read from EC2's own store rather than invented, so the
/// declaration names a VPC that is really there.
const VPC_ID_PLACEHOLDER: &str = "VPC_ID_OF_THIS_ACCOUNT";

/// One AWS Cloud Map call over the `awsJson1_1` wire its model declares, signed for
/// `servicediscovery` — that model's own `sigv4Name` — in the Region the stack's resources live
/// in.
async fn cloud_map(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let body = serde_json::to_vec(request).expect("a serialisable Cloud Map request");
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://servicediscovery.{REGION}.amazonaws.com/"))
        .header("host", format!("servicediscovery.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{CLOUD_MAP_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/servicediscovery/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed AWS Cloud Map request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Cloud Map read that must succeed, answered with its document.
async fn read(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    let (status, answered) = cloud_map(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation} with {request}, got {status} with \
         body {answered}"
    );
    answered
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

/// One member of a reported record as text.
fn text(reported: &Value, member: &str) -> String {
    reported
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code one Cloud Map refusal carries, as the `__type` an awsJson service renders it
/// under: the class a caller branches on, which is what says a resource is no longer held.
fn error_code(answered: &Value) -> String {
    answered
        .get("__type")
        .and_then(Value::as_str)
        .map(|code| code.rsplit('#').next().unwrap_or(code).to_string())
        .unwrap_or_default()
}

/// Wait until Cloud Map reports no namespace under `namespace_id`, asking the stack about itself
/// in between: a reporting call is what advances a deletion the stack still owes.
async fn wait_until_the_namespace_is_gone(
    router: &MultiServiceRouter,
    stack: &str,
    namespace_id: &str,
    case: &str,
) {
    let mut answered = 0;
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (status, read) =
            cloud_map(router, "GetNamespace", &serde_json::json!({"Id": namespace_id})).await;
        answered = status;
        last = read;
        if status != 200 && error_code(&last) == NAMESPACE_NOT_FOUND {
            return;
        }
        let _ = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: expected Cloud Map to hold no namespace {namespace_id} once the stack \
         had been deleted, got HTTP {answered} with body {last}"
    );
}

/// One row of the table: a template, and what the rule says the resources it declares must then
/// be in AWS Cloud Map.
struct Row {
    case: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The namespace name the template declares, and the `Namespace$Type` Cloud Map must report
    /// for a namespace of that kind.
    namespace_name: &'static str,
    namespace_type: &'static str,
    /// The time to live the namespace's SOA record must carry, or `None` for an HTTP namespace,
    /// which has no hosted zone and therefore no SOA record.
    soa_ttl: Option<i64>,
    /// The service name the template declares, the `Service$Type` Cloud Map must report for it,
    /// and the DNS records its `DnsConfig` declares as `(RecordType, TTL)`.
    service_name: &'static str,
    service_type: &'static str,
    dns_records: &'static [(&'static str, i64)],
    /// The instance the template registers: the `InstanceId` it declares and the attributes it
    /// must answer discovery with.
    instance_id: &'static str,
    instance_attributes: &'static [(&'static str, &'static str)],
    /// The tag the namespace's declaration asks for, which `ListTagsForResource` must report
    /// beside the ones CloudFormation propagates.
    tag: (&'static str, &'static str),
}

const ROWS: &[Row] = &[
    Row {
        case: "an HTTP namespace with an API-only service and one instance, in YAML",
        stack: "cloudmap-http-namespace",
        template: concat!(
            "Resources:\n",
            "  Namespace:\n",
            "    Type: AWS::ServiceDiscovery::HttpNamespace\n",
            "    Properties:\n",
            "      Name: orders-http\n",
            "      Description: the orders namespace\n",
            "      Tags:\n",
            "        - Key: team\n",
            "          Value: platform\n",
            "  Service:\n",
            "    Type: AWS::ServiceDiscovery::Service\n",
            "    Properties:\n",
            "      Name: orders\n",
            "      NamespaceId: !GetAtt Namespace.Id\n",
            "      Type: HTTP\n",
            "  Instance:\n",
            "    Type: AWS::ServiceDiscovery::Instance\n",
            "    Properties:\n",
            "      InstanceId: orders-1\n",
            "      ServiceId: !Ref Service\n",
            "      InstanceAttributes:\n",
            "        AWS_INSTANCE_IPV4: 10.0.0.7\n",
            "        stage: canary\n",
            "Outputs:\n",
            "  NamespaceRef:\n",
            "    Value: !Ref Namespace\n",
            "  NamespaceId:\n",
            "    Value: !GetAtt Namespace.Id\n",
            "  NamespaceArn:\n",
            "    Value: !GetAtt Namespace.Arn\n",
            "  ServiceRef:\n",
            "    Value: !Ref Service\n",
            "  ServiceId:\n",
            "    Value: !GetAtt Service.Id\n",
            "  ServiceArn:\n",
            "    Value: !GetAtt Service.Arn\n",
            "  ServiceNameOut:\n",
            "    Value: !GetAtt Service.Name\n",
            "  InstanceRef:\n",
            "    Value: !Ref Instance\n",
        ),
        namespace_name: "orders-http",
        namespace_type: "HTTP",
        soa_ttl: None,
        service_name: "orders",
        service_type: "HTTP",
        dns_records: &[],
        instance_id: "orders-1",
        instance_attributes: &[("AWS_INSTANCE_IPV4", "10.0.0.7"), ("stage", "canary")],
        tag: ("team", "platform"),
    },
    Row {
        case: "a public DNS namespace with a two-record service and a quoted TTL, in JSON",
        stack: "cloudmap-public-dns-namespace",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Namespace":{"Type":"AWS::ServiceDiscovery::PublicDnsNamespace","Properties":{"#,
            r#""Name":"shop.example.com","Description":"the shop namespace","#,
            r#""Properties":{"DnsProperties":{"SOA":{"TTL":120}}},"#,
            r#""Tags":[{"Key":"tier","Value":"edge"}]}},"#,
            r#""Service":{"Type":"AWS::ServiceDiscovery::Service","Properties":{"#,
            r#""Name":"checkout","NamespaceId":{"Fn::GetAtt":["Namespace","Id"]},"#,
            r#""DnsConfig":{"RoutingPolicy":"MULTIVALUE","DnsRecords":["#,
            r#"{"Type":"A","TTL":"60"},{"Type":"AAAA","TTL":90}]}}},"#,
            r#""Instance":{"Type":"AWS::ServiceDiscovery::Instance","Properties":{"#,
            r#""InstanceId":"checkout-1","ServiceId":{"Fn::GetAtt":["Service","Id"]},"#,
            r#""InstanceAttributes":{"AWS_INSTANCE_IPV4":"10.1.2.3","#,
            r#""AWS_INSTANCE_IPV6":"2001:db8::1"}}}},"#,
            r#""Outputs":{"#,
            r#""NamespaceRef":{"Value":{"Ref":"Namespace"}},"#,
            r#""NamespaceId":{"Value":{"Fn::GetAtt":["Namespace","Id"]}},"#,
            r#""NamespaceArn":{"Value":{"Fn::GetAtt":["Namespace","Arn"]}},"#,
            r#""HostedZoneIdOut":{"Value":{"Fn::GetAtt":["Namespace","HostedZoneId"]}},"#,
            r#""ServiceRef":{"Value":{"Ref":"Service"}},"#,
            r#""ServiceId":{"Value":{"Fn::GetAtt":["Service","Id"]}},"#,
            r#""ServiceArn":{"Value":{"Fn::GetAtt":["Service","Arn"]}},"#,
            r#""ServiceNameOut":{"Value":{"Fn::GetAtt":["Service","Name"]}},"#,
            r#""InstanceRef":{"Value":{"Ref":"Instance"}}}}"#,
        ),
        namespace_name: "shop.example.com",
        namespace_type: "DNS_PUBLIC",
        soa_ttl: Some(120),
        service_name: "checkout",
        service_type: "DNS_HTTP",
        dns_records: &[("A", 60), ("AAAA", 90)],
        instance_id: "checkout-1",
        instance_attributes: &[
            ("AWS_INSTANCE_IPV4", "10.1.2.3"),
            ("AWS_INSTANCE_IPV6", "2001:db8::1"),
        ],
        tag: ("tier", "edge"),
    },
    Row {
        case: "a private DNS namespace with an SRV service, in YAML",
        stack: "cloudmap-private-dns-namespace",
        template: concat!(
            "Resources:\n",
            "  Namespace:\n",
            "    Type: AWS::ServiceDiscovery::PrivateDnsNamespace\n",
            "    Properties:\n",
            "      Name: billing.internal\n",
            "      Vpc: VPC_ID_OF_THIS_ACCOUNT\n",
            "      Properties:\n",
            "        DnsProperties:\n",
            "          SOA:\n",
            "            TTL: \"30\"\n",
            "      Tags:\n",
            "        - Key: owner\n",
            "          Value: billing\n",
            "  Service:\n",
            "    Type: AWS::ServiceDiscovery::Service\n",
            "    Properties:\n",
            "      Name: ledger\n",
            "      NamespaceId: !GetAtt Namespace.Id\n",
            "      DnsConfig:\n",
            "        DnsRecords:\n",
            "          - Type: SRV\n",
            "            TTL: 15\n",
            "  Instance:\n",
            "    Type: AWS::ServiceDiscovery::Instance\n",
            "    Properties:\n",
            "      InstanceId: ledger-1\n",
            "      ServiceId: !GetAtt Service.Id\n",
            "      InstanceAttributes:\n",
            "        AWS_INSTANCE_IPV4: 10.2.3.4\n",
            "        AWS_INSTANCE_PORT: \"8080\"\n",
            "Outputs:\n",
            "  NamespaceRef:\n",
            "    Value: !Ref Namespace\n",
            "  NamespaceId:\n",
            "    Value: !GetAtt Namespace.Id\n",
            "  NamespaceArn:\n",
            "    Value: !GetAtt Namespace.Arn\n",
            "  HostedZoneIdOut:\n",
            "    Value: !GetAtt Namespace.HostedZoneId\n",
            "  ServiceRef:\n",
            "    Value: !Ref Service\n",
            "  ServiceId:\n",
            "    Value: !GetAtt Service.Id\n",
            "  ServiceArn:\n",
            "    Value: !GetAtt Service.Arn\n",
            "  ServiceNameOut:\n",
            "    Value: !GetAtt Service.Name\n",
            "  InstanceRef:\n",
            "    Value: !Ref Instance\n",
        ),
        namespace_name: "billing.internal",
        namespace_type: "DNS_PRIVATE",
        soa_ttl: Some(30),
        service_name: "ledger",
        service_type: "DNS_HTTP",
        dns_records: &[("SRV", 15)],
        instance_id: "ledger-1",
        instance_attributes: &[("AWS_INSTANCE_IPV4", "10.2.3.4"), ("AWS_INSTANCE_PORT", "8080")],
        tag: ("owner", "billing"),
    },
];

/// Every row: the stack reaches `CREATE_COMPLETE`, the values it publishes are the ones Cloud
/// Map answered, every resource the template declared is read back through that service's own
/// reads — the namespace by the id the stack's `Ref` reports, and through `ListNamespaces` —
/// and deleting the stack takes each of them away again.
#[tokio::test]
async fn a_stack_provisions_the_cloud_map_resources_it_declares() {
    let (router, engine) = router_with_engine();
    // The account's own VPC in the Region these stacks are deployed in, read from the Amazon
    // EC2 store Cloud Map resolves a private namespace's `Vpc` against.
    let held_vpc = {
        let ec2 = engine
            .state::<service_com_amazonaws_ec2_amazonec2::state::ComAmazonawsEc2Amazonec2State>();
        let held = ec2
            .lock()
            .ensure_default_network(REGION)
            .expect("the account holds a default VPC in this Region");
        held
    };

    for row in ROWS {
        let case = row.case;
        let template = row.template.replace(VPC_ID_PLACEHOLDER, &held_vpc);
        let (status, body) = create_stack(&router, row.stack, &template, &[]).await;
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

        // `Ref` returns the namespace ID, which is the id Cloud Map minted and the handle every
        // call on the namespace takes — so it is the same value the type's `Id` attribute
        // publishes, and it answers to `GetNamespace`.
        let namespace_id = output(&outputs, "NamespaceRef", case);
        assert_eq!(
            output(&outputs, "NamespaceId", case),
            namespace_id,
            "case {case:?}: expected the namespace's Ref and its Id attribute to be one value, \
             got {outputs:?}"
        );
        let namespace = read(
            &router,
            case,
            "GetNamespace",
            &serde_json::json!({"Id": namespace_id}),
        )
        .await
        .get("Namespace")
        .cloned()
        .unwrap_or(Value::Null);
        assert_eq!(
            text(&namespace, "Name"),
            row.namespace_name,
            "case {case:?}: expected the namespace to report the declared name, got {namespace}"
        );
        assert_eq!(
            text(&namespace, "Type"),
            row.namespace_type,
            "case {case:?}: expected the namespace kind the declaration asked for, got \
             {namespace}"
        );
        let namespace_arn = output(&outputs, "NamespaceArn", case);
        assert_eq!(
            text(&namespace, "Arn"),
            namespace_arn,
            "case {case:?}: expected the published ARN to be the one Cloud Map assigned, got \
             {namespace}"
        );

        // Cause 1 of the issue this suite pins: after CREATE_COMPLETE the namespace the stack
        // created is LISTED, findable by the id its `Ref` reports, with the ARN and the kind
        // Cloud Map holds for it.
        let listed = read(&router, case, "ListNamespaces", &serde_json::json!({}))
            .await
            .get("Namespaces")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let found: Vec<(String, String, String)> = listed
            .iter()
            .filter(|summary| text(summary, "Id") == namespace_id)
            .map(|summary| {
                (
                    text(summary, "Id"),
                    text(summary, "Arn"),
                    text(summary, "Type"),
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![(
                namespace_id.clone(),
                namespace_arn.clone(),
                row.namespace_type.to_string()
            )],
            "case {case:?}: expected ListNamespaces to report the namespace the stack created, \
             got {listed:?}"
        );

        // A DNS namespace is backed by a Route 53 hosted zone, whose id only Cloud Map can
        // report; an HTTP namespace has none, and publishes no such attribute.
        match row.soa_ttl {
            Some(ttl) => {
                let dns = namespace
                    .get("Properties")
                    .and_then(|properties| properties.get("DnsProperties"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let hosted_zone_id = output(&outputs, "HostedZoneIdOut", case);
                assert_eq!(
                    text(&dns, "HostedZoneId"),
                    hosted_zone_id,
                    "case {case:?}: expected the published HostedZoneId to be the zone Cloud Map \
                     created, got {namespace}"
                );
                assert!(
                    !hosted_zone_id.is_empty(),
                    "case {case:?}: expected the namespace to publish the id of a hosted zone \
                     Route 53 holds, got {hosted_zone_id:?}"
                );
                assert_eq!(
                    dns.get("SOA").and_then(|soa| soa.get("TTL")).and_then(Value::as_i64),
                    Some(ttl),
                    "case {case:?}: expected the declared SOA TTL, got {namespace}"
                );
            }
            None => assert!(
                !outputs.contains_key("HostedZoneIdOut"),
                "case {case:?}: expected an HTTP namespace to publish no hosted zone, got \
                 {outputs:?}"
            ),
        }

        // The service: `Ref` returns the service ID Cloud Map minted, it lives IN the namespace
        // the declaration named, and the records its `DnsConfig` asked for are the ones that
        // service holds.
        let service_id = output(&outputs, "ServiceRef", case);
        assert_eq!(
            output(&outputs, "ServiceId", case),
            service_id,
            "case {case:?}: expected the service's Ref and its Id attribute to be one value, got \
             {outputs:?}"
        );
        let service = read(
            &router,
            case,
            "GetService",
            &serde_json::json!({"Id": service_id}),
        )
        .await
        .get("Service")
        .cloned()
        .unwrap_or(Value::Null);
        assert_eq!(
            text(&service, "Name"),
            row.service_name,
            "case {case:?}: expected the service to report the declared name, got {service}"
        );
        assert_eq!(
            output(&outputs, "ServiceNameOut", case),
            row.service_name,
            "case {case:?}: expected the published service name to be the declared one, got \
             {outputs:?}"
        );
        assert_eq!(
            text(&service, "NamespaceId"),
            namespace_id,
            "case {case:?}: expected the service to live in the namespace the stack created, got \
             {service}"
        );
        assert_eq!(
            text(&service, "Arn"),
            output(&outputs, "ServiceArn", case),
            "case {case:?}: expected the published service ARN to be the one Cloud Map assigned, \
             got {service}"
        );
        assert_eq!(
            text(&service, "Type"),
            row.service_type,
            "case {case:?}: expected the service kind the declaration implies, got {service}"
        );
        let records: Vec<(String, i64)> = service
            .get("DnsConfig")
            .and_then(|config| config.get("DnsRecords"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|record| {
                (
                    text(record, "Type"),
                    record.get("TTL").and_then(Value::as_i64).unwrap_or_default(),
                )
            })
            .collect();
        let expected_records: Vec<(String, i64)> = row
            .dns_records
            .iter()
            .map(|(record_type, ttl)| ((*record_type).to_string(), *ttl))
            .collect();
        assert_eq!(
            records, expected_records,
            "case {case:?}: expected the declared DNS records, got {service}"
        );

        // The instance: `Ref` returns the `InstanceId` the declaration asked for, and the
        // attributes it answers with are the declared ones.
        let instance_id = output(&outputs, "InstanceRef", case);
        assert_eq!(
            instance_id, row.instance_id,
            "case {case:?}: expected the instance's Ref to be the declared InstanceId, got \
             {outputs:?}"
        );
        let instance = read(
            &router,
            case,
            "GetInstance",
            &serde_json::json!({"ServiceId": service_id, "InstanceId": instance_id}),
        )
        .await
        .get("Instance")
        .cloned()
        .unwrap_or(Value::Null);
        let attributes: BTreeMap<String, String> = instance
            .get("Attributes")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let expected_attributes: BTreeMap<String, String> = row
            .instance_attributes
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            attributes, expected_attributes,
            "case {case:?}: expected the declared instance attributes, got {instance}"
        );

        // ...and the instance is DISCOVERABLE through the namespace and the service the stack
        // created, which is what a client registers an instance for.
        let discovered = read(
            &router,
            case,
            "DiscoverInstances",
            &serde_json::json!({
                "NamespaceName": row.namespace_name,
                "ServiceName": row.service_name,
            }),
        )
        .await;
        let discovered_ids: Vec<String> = discovered
            .get("Instances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|found| text(found, "InstanceId"))
            .collect();
        assert_eq!(
            discovered_ids,
            vec![row.instance_id.to_string()],
            "case {case:?}: expected DiscoverInstances to report the registered instance, got \
             {discovered}"
        );

        // The namespace carries the tags its declaration asked for and the three records
        // CloudFormation keeps of its own accord about the resource it provisioned.
        let tags = read(
            &router,
            case,
            "ListTagsForResource",
            &serde_json::json!({"ResourceARN": namespace_arn}),
        )
        .await
        .get("Tags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| (text(tag, "Key"), text(tag, "Value")))
        .collect::<BTreeMap<String, String>>();
        assert_eq!(
            tags.get(row.tag.0).map(String::as_str),
            Some(row.tag.1),
            "case {case:?}: expected the declared tag on the namespace, got {tags:?}"
        );
        assert_eq!(
            tags.get("aws:cloudformation:stack-name").map(String::as_str),
            Some(row.stack),
            "case {case:?}: expected CloudFormation's own stack-name tag on the namespace, got \
             {tags:?}"
        );
        assert_eq!(
            tags.get("aws:cloudformation:logical-id").map(String::as_str),
            Some("Namespace"),
            "case {case:?}: expected CloudFormation's own logical-id tag on the namespace, got \
             {tags:?}"
        );

        // Deleting the stack takes the estate away: the instance is deregistered, the service
        // deleted and the namespace deleted, in the order those deletes are allowed in.
        let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        wait_until_the_namespace_is_gone(&router, row.stack, &namespace_id, case).await;
        let (status, answered) = cloud_map(
            &router,
            "GetService",
            &serde_json::json!({"Id": service_id}),
        )
        .await;
        assert_eq!(
            error_code(&answered),
            SERVICE_NOT_FOUND,
            "case {case:?}: expected Cloud Map to hold no service once the stack had been \
             deleted, got {status} with body {answered}"
        );
    }
}

/// A service that declares a `DnsConfig` inside an HTTP namespace is one Cloud Map REFUSES
/// ("when you create a service using a namespace that has a type of HTTP, you can't include a
/// DnsConfig element"), so the resource fails and the stack rolls back — and nothing is left
/// behind claiming to be a DNS-discoverable service in a namespace that has no DNS.
#[tokio::test]
async fn a_service_declaring_dns_records_in_an_http_namespace_rolls_the_stack_back() {
    let router = router();
    let case = "a DnsConfig in an HTTP namespace";
    let stack = "cloudmap-http-namespace-dnsconfig";
    let template = concat!(
        "Resources:\n",
        "  Namespace:\n",
        "    Type: AWS::ServiceDiscovery::HttpNamespace\n",
        "    Properties:\n",
        "      Name: refused-http\n",
        "  Service:\n",
        "    Type: AWS::ServiceDiscovery::Service\n",
        "    Properties:\n",
        "      Name: refused\n",
        "      NamespaceId: !GetAtt Namespace.Id\n",
        "      DnsConfig:\n",
        "        DnsRecords:\n",
        "          - Type: A\n",
        "            TTL: 60\n",
    );

    let (status, body) = create_stack(&router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the refused service to roll the stack back, got {described}"
    );
    let reasons = stack_event_reasons(&router, REGION, stack).await;
    assert!(
        reasons.iter().any(|reason| reason.contains("DnsConfig")),
        "case {case:?}: expected the refused resource to report Cloud Map's own refusal, got \
         {reasons:?}"
    );
    // ...worded as the SDK of the called service raises it: that service's own sentence, then
    // the SDK's diagnostic of the refused call naming the service and the status it answered.
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("(Service: ServiceDiscovery, Status Code: 400,")),
        "case {case:?}: expected the refusal to carry the called service's SDK diagnostic, got \
         {reasons:?}"
    );
    // The rollback took the namespace away with the failed deployment, so nothing of the
    // refused estate is left: no namespace of that name is listed, and no service was created.
    let listed = read(&router, case, "ListNamespaces", &serde_json::json!({}))
        .await
        .get("Namespaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        listed
            .iter()
            .all(|summary| text(summary, "Name") != "refused-http"),
        "case {case:?}: expected the rolled-back namespace to be gone, got {listed:?}"
    );
    let services = read(&router, case, "ListServices", &serde_json::json!({}))
        .await
        .get("Services")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        services
            .iter()
            .all(|summary| text(summary, "Name") != "refused"),
        "case {case:?}: expected no service to have been created, got {services:?}"
    );
}

/// A declaration that states none of a property the owning call REQUIRES describes no resource
/// that could be created, so the resource fails with the provider's own sentence naming that
/// property and the stack rolls back — nothing is sent to Cloud Map without it, and no
/// namespace of that name is left behind.
///
/// `AWS::ServiceDiscovery::PrivateDnsNamespace` is the type in that position here: `Vpc` is
/// required by `CreatePrivateDnsNamespace` and by the type, and it is the VPC the private
/// hosted zone a private namespace IS gets associated with.
#[tokio::test]
async fn a_private_dns_namespace_declaring_no_vpc_rolls_the_stack_back() {
    let router = router();
    let case = "a private DNS namespace naming no VPC";
    let stack = "cloudmap-private-dns-namespace-no-vpc";
    let template = concat!(
        r#"{"Resources":{"#,
        r#""Namespace":{"Type":"AWS::ServiceDiscovery::PrivateDnsNamespace","Properties":{"#,
        r#""Name":"refused.internal","Description":"the refused namespace"}}}}"#,
    );

    let (status, body) = create_stack(&router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the incomplete declaration to roll the stack back, got \
         {described}"
    );
    let reasons = stack_event_reasons(&router, REGION, stack).await;
    assert!(
        reasons.iter().any(|reason| reason.contains("Vpc")),
        "case {case:?}: expected the failed resource to report the property it declares none \
         of, got {reasons:?}"
    );
    let listed = read(&router, case, "ListNamespaces", &serde_json::json!({}))
        .await
        .get("Namespaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        listed
            .iter()
            .all(|summary| text(summary, "Name") != "refused.internal"),
        "case {case:?}: expected no namespace to have been created, got {listed:?}"
    );
}
