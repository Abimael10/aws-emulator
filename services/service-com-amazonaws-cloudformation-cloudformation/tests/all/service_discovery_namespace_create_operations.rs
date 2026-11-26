// SPDX-License-Identifier: CC0-1.0

//! A stack's AWS Cloud Map NAMESPACE resource against the asynchronous operation its create
//! files.
//!
//! RULE: a resource whose owning service carries the create out ASYNCHRONOUSLY — answering an
//! operation id and reporting the outcome only through that operation record — is reported
//! created only once that operation reaches its terminal success, and a create whose operation
//! lands `FAIL` fails the resource with that operation's own reason, so the stack rolls back
//! instead of publishing identifiers for a resource that does not exist.
//!
//! Each of the three namespace types is in that position: `CreateHttpNamespace`,
//! `CreatePublicDnsNamespace` and `CreatePrivateDnsNamespace` each answer an `OperationId` and
//! nothing else, and `Operation$Status` is one of `SUBMITTED`, `PENDING`, `SUCCESS` and `FAIL`
//! (`smithy_query(query="shape",shape="OperationStatus")`), with `ErrorCode`/`ErrorMessage`
//! carrying why a failed one failed.
//!
//! The rows differ along what the rule generalises over: which namespace kind the template
//! declares, whether its creation is backed by a Route 53 hosted zone, what the create's
//! asynchronous step makes of it (it settles, or it cannot be carried out — a VPC the account
//! holds none of, a public zone name Route 53 reserves), whether the template declares resources
//! BEHIND the namespace, and the language the template is written in.
//!
//! No expected value is read out of the answer being checked: each is either what the row's own
//! template declares or what AWS Cloud Map reports for the resource over its own wire.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    create_stack, router_with_engine, settled_stack, stack_event_reasons, xml_text, xml_texts,
    REGION,
};

/// The service shape an awsJson `X-Amz-Target` names a Cloud Map operation under (that model's
/// own service shape, `smithy_query(query="service_info")`).
const CLOUD_MAP_TARGET_PREFIX: &str = "Route53AutoNaming_v20170314";

/// What a row's template writes where the VPC a private DNS namespace is created for goes. The
/// id substituted for it is read out of Amazon EC2's own store, so the declaration names a VPC
/// that is really there.
const VPC_ID_PLACEHOLDER: &str = "VPC_ID_OF_THIS_ACCOUNT";

/// A well-formed Amazon VPC id the account holds NO VPC for: a private namespace named this has
/// nothing for its hosted zone to be associated with, so its create's asynchronous step cannot
/// be carried out.
const UNHELD_VPC: &str = "vpc-0f1e2d3c4b5a69788";

/// The `OperationType` every namespace create files, whichever of the three calls made it
/// (`smithy_query(query="shape",shape="OperationType")`).
const CREATE_NAMESPACE: &str = "CREATE_NAMESPACE";

/// The terminal `OperationStatus` of an operation Cloud Map carried out.
const SUCCESS: &str = "SUCCESS";

/// One AWS Cloud Map call over the `awsJson1_1` wire its model declares, signed for
/// `servicediscovery` — that model's own `sigv4Name` — in the Region the stack's resources live
/// in.
async fn cloud_map(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
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

/// One member of a reported record as text.
fn text(reported: &Value, member: &str) -> String {
    reported
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One stack output's value, read off the `DescribeStacks` body by key: the answer carries one
/// `OutputValue` per `OutputKey`, so the two parallel lists zip.
fn output(described: &str, key: &str) -> Option<String> {
    xml_texts(described, "OutputKey")
        .into_iter()
        .zip(xml_texts(described, "OutputValue"))
        .find(|(reported, _)| reported == key)
        .map(|(_, value)| value)
}

/// The `CREATE_NAMESPACE` operation Cloud Map filed for the namespace `namespace_id`, read
/// through that service's own wire: `ListOperations`' `NAMESPACE_ID` and `TYPE` filters name
/// exactly that one operation, and `GetOperation` reports it whole.
async fn create_operation(router: &MultiServiceRouter, case: &str, namespace_id: &str) -> Value {
    let listed = read(
        router,
        case,
        "ListOperations",
        &serde_json::json!({"Filters": [
            {"Name": "NAMESPACE_ID", "Values": [namespace_id]},
            {"Name": "TYPE", "Values": [CREATE_NAMESPACE]},
        ]}),
    )
    .await;
    let operations = listed
        .get("Operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        operations.len(),
        1,
        "case {case:?}: expected one {CREATE_NAMESPACE} operation for {namespace_id}, got \
         {operations:?}"
    );
    read(
        router,
        case,
        "GetOperation",
        &serde_json::json!({"OperationId": text(&operations[0], "Id")}),
    )
    .await
    .get("Operation")
    .cloned()
    .unwrap_or(Value::Null)
}

/// One row whose namespace create Cloud Map CARRIES OUT: the template, and the namespace the
/// stack must then publish.
struct SettledRow {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    namespace_name: &'static str,
}

/// The rows differ along the namespace kind, whether a hosted zone backs the namespace, how many
/// labels its name spells and the language the template is written in.
const SETTLED_ROWS: &[SettledRow] = &[
    SettledRow {
        case: "an HTTP namespace, which has no hosted zone, in JSON",
        stack: "cloudmap-op-http",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Namespace":{"Type":"AWS::ServiceDiscovery::HttpNamespace","Properties":{"#,
            r#""Name":"orders-http"}}},"#,
            r#""Outputs":{"NamespaceId":{"Value":{"Fn::GetAtt":["Namespace","Id"]}}}}"#,
        ),
        namespace_name: "orders-http",
    },
    SettledRow {
        case: "a public DNS namespace, whose public hosted zone needs a registrable name, in JSON",
        stack: "cloudmap-op-public",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Namespace":{"Type":"AWS::ServiceDiscovery::PublicDnsNamespace","Properties":{"#,
            r#""Name":"shop.example.com"}}},"#,
            r#""Outputs":{"NamespaceId":{"Value":{"Fn::GetAtt":["Namespace","Id"]}}}}"#,
        ),
        namespace_name: "shop.example.com",
    },
    SettledRow {
        case: "a private DNS namespace named by a single label, with a VPC of the account, in \
               YAML",
        stack: "cloudmap-op-private",
        template: concat!(
            "Resources:\n",
            "  Namespace:\n",
            "    Type: AWS::ServiceDiscovery::PrivateDnsNamespace\n",
            "    Properties:\n",
            "      Name: internal\n",
            "      Vpc: VPC_ID_OF_THIS_ACCOUNT\n",
            "  Service:\n",
            "    Type: AWS::ServiceDiscovery::Service\n",
            "    Properties:\n",
            "      Name: ledger\n",
            "      NamespaceId: !GetAtt Namespace.Id\n",
            "      DnsConfig:\n",
            "        DnsRecords:\n",
            "          - Type: A\n",
            "            TTL: 60\n",
            "Outputs:\n",
            "  NamespaceId:\n",
            "    Value: !GetAtt Namespace.Id\n",
        ),
        namespace_name: "internal",
    },
];

/// Every row: the stack reaches `CREATE_COMPLETE`, and the namespace it published is one whose
/// `CREATE_NAMESPACE` operation Cloud Map reports `SUCCESS` — the terminal status that says the
/// namespace was really created — naming that namespace as its `NAMESPACE` target, and
/// `GetNamespace` answers it under the declared name.
#[tokio::test]
async fn a_namespace_resource_is_published_only_once_its_create_operation_succeeded() {
    let (router, engine) = router_with_engine();
    let held_vpc = {
        let ec2 = engine
            .state::<service_com_amazonaws_ec2_amazonec2::state::ComAmazonawsEc2Amazonec2State>();
        let held = ec2
            .lock()
            .ensure_default_network(REGION)
            .expect("the account holds a default VPC in this Region");
        held
    };

    for row in SETTLED_ROWS {
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
        let namespace_id = output(&described, "NamespaceId").unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish NamespaceId, got {described}")
        });

        // The moment the stack reports the resource created, the operation that created it is
        // through: a namespace published while its create is still SUBMITTED or PENDING is one
        // nothing has finished creating.
        let operation = create_operation(&router, case, &namespace_id).await;
        assert_eq!(
            text(&operation, "Status"),
            SUCCESS,
            "case {case:?}: expected the namespace's create operation to have succeeded by the \
             time the stack reported CREATE_COMPLETE, got {operation}"
        );
        assert_eq!(
            text(&operation, "Type"),
            CREATE_NAMESPACE,
            "case {case:?}: expected the operation Cloud Map filed for the namespace, got \
             {operation}"
        );
        assert_eq!(
            operation
                .get("Targets")
                .map(|targets| text(targets, "NAMESPACE"))
                .unwrap_or_default(),
            namespace_id,
            "case {case:?}: expected the operation to name the published namespace as its \
             target, got {operation}"
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
            "case {case:?}: expected the published id to resolve to the declared namespace, got \
             {namespace}"
        );
    }
}

/// One row whose namespace create Cloud Map ACCEPTS and then cannot carry out: the template, and
/// the sentence the failed operation reports it with.
struct FailedRow {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    namespace_name: &'static str,
    /// Part of the `Operation$ErrorMessage` the failed `CREATE_NAMESPACE` carries, which is what
    /// the failed resource must be reported with.
    reported: &'static str,
}

/// The rows differ along the namespace kind, what the asynchronous step cannot do (a VPC the
/// account holds none of; a one-label name no PUBLIC hosted zone may carry), whether the
/// template declares resources behind the namespace, and the language it is written in.
const FAILED_ROWS: &[FailedRow] = &[
    FailedRow {
        case: "a private DNS namespace naming a VPC the account holds none of, with a service \
               and an instance behind it, in YAML",
        stack: "cloudmap-op-absent-vpc",
        template: concat!(
            "Resources:\n",
            "  Namespace:\n",
            "    Type: AWS::ServiceDiscovery::PrivateDnsNamespace\n",
            "    Properties:\n",
            "      Name: unreachable.internal\n",
            "      Vpc: vpc-0f1e2d3c4b5a69788\n",
            "  Service:\n",
            "    Type: AWS::ServiceDiscovery::Service\n",
            "    Properties:\n",
            "      Name: ledger\n",
            "      NamespaceId: !GetAtt Namespace.Id\n",
            "      DnsConfig:\n",
            "        DnsRecords:\n",
            "          - Type: A\n",
            "            TTL: 60\n",
            "  Instance:\n",
            "    Type: AWS::ServiceDiscovery::Instance\n",
            "    Properties:\n",
            "      InstanceId: ledger-1\n",
            "      ServiceId: !GetAtt Service.Id\n",
            "      InstanceAttributes:\n",
            "        AWS_INSTANCE_IPV4: 10.0.0.5\n",
            "Outputs:\n",
            "  NamespaceId:\n",
            "    Value: !GetAtt Namespace.Id\n",
            "  ServiceId:\n",
            "    Value: !GetAtt Service.Id\n",
        ),
        namespace_name: "unreachable.internal",
        reported: UNHELD_VPC,
    },
    FailedRow {
        case: "a private DNS namespace whose name has an empty label, which no hosted zone can \
               carry, with a VPC of the account, in JSON",
        stack: "cloudmap-op-unusable-name",
        template: concat!(
            r#"{"Resources":{"#,
            r#""Namespace":{"Type":"AWS::ServiceDiscovery::PrivateDnsNamespace","Properties":{"#,
            r#""Name":"empty..label.example.com","Vpc":"VPC_ID_OF_THIS_ACCOUNT"}}},"#,
            r#""Outputs":{"NamespaceId":{"Value":{"Fn::GetAtt":["Namespace","Id"]}}}}"#,
        ),
        namespace_name: "empty..label.example.com",
        // The clause Route 53 states a name with a label of no octets with, quoting the name
        // asked for (`service-com-amazonaws-route53-awsdnsv20130401`'s `dns_name_fault_clause`).
        reported: "DomainLabelEmpty",
    },
];

/// Every row: the stack does NOT reach `CREATE_COMPLETE` and publishes no identifier for the
/// namespace — it rolls back, reporting the failed resource with the sentence the failed
/// `CREATE_NAMESPACE` operation carries — and Cloud Map holds no namespace of that name, so
/// nothing names a namespace that does not exist.
#[tokio::test]
async fn a_namespace_whose_create_operation_failed_rolls_the_stack_back() {
    let (router, engine) = router_with_engine();
    let held_vpc = {
        let ec2 = engine
            .state::<service_com_amazonaws_ec2_amazonec2::state::ComAmazonawsEc2Amazonec2State>();
        let held = ec2
            .lock()
            .ensure_default_network(REGION)
            .expect("the account holds a default VPC in this Region");
        held
    };

    for row in FAILED_ROWS {
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
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed namespace create to roll the stack back, got \
             {described}"
        );
        assert_eq!(
            output(&described, "NamespaceId"),
            None,
            "case {case:?}: expected no namespace identifier to be published, got {described}"
        );

        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert!(
            reasons.iter().any(|reason| reason.contains(row.reported)),
            "case {case:?}: expected the failed resource to report what the operation failed \
             with ({:?}), got {reasons:?}",
            row.reported
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
                .all(|summary| text(summary, "Name") != row.namespace_name),
            "case {case:?}: expected Cloud Map to hold no namespace of the declared name, got \
             {listed:?}"
        );
        let services = read(&router, case, "ListServices", &serde_json::json!({}))
            .await
            .get("Services")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            services.is_empty(),
            "case {case:?}: expected no service to have been created behind the failed \
             namespace, got {services:?}"
        );
    }
}
