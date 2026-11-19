// SPDX-License-Identifier: CC0-1.0

//! The hosted zone a DNS namespace IS, and what becomes of a create whose zone Route 53
//! will not create.
//!
//! RULE: a DNS namespace is the Route 53 hosted zone of the same name that Cloud Map
//! creates for it, so a create whose zone Route 53 refuses is a namespace that cannot
//! exist: the synchronous call still answers an `OperationId`, and THAT operation lands
//! terminal `Status` `FAIL` carrying `ErrorCode` `CANNOT_CREATE_HOSTED_ZONE`, the refusal
//! Route 53 answered with as its `ErrorMessage` and the owning account as `OwnerAccount`,
//! while the namespace the create staged is rolled back with it — no `GetNamespace`, no
//! listing entry, and the name free again. A create whose zone Route 53 DOES hold reports
//! that zone's id and reads back.
//!
//! The rows below differ along WHICH rule of a hosted zone's own creation the request
//! breaks — a name that is a single label, a name with an empty label, a name with an
//! over-long label, a `Vpc` that is not a VPC id — because `NamespaceNamePrivate`
//! (`^(?!arn:)[!-~]{1,253}$`, from
//! `smithy_query(query="operation",operation="CreatePrivateDnsNamespace")`) admits values
//! that a hosted zone's name cannot be, where `NamespaceNamePublic` is itself a
//! domain-name pattern and refuses them at the boundary. Each refusal's text is Route 53's
//! own, so the assertions check the fragment the refused value puts in it rather than
//! inventing a sentence.
//!
//! Every Cloud Map call goes through `MultiServiceRouter::dispatch` (`awsJson1_1`, the same
//! front door a client uses) and every zone assertion is read back through Route 53's own
//! API over the emulator's in-process transport. Amazon EC2 is deliberately NOT mounted
//! here, so the VPC pre-flight resolves nothing and the hosted zone's own creation is the
//! only thing deciding each outcome.

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::async_namespace_operations::{set_operation_due, settled_operation};
use crate::cloud_map_core::{emulator_with_engine, ok, refusal};

/// A VPC id of the shape `CreatePrivateDnsNamespace$Vpc` takes (`ResourceId`, 0..64), in
/// the long form Amazon EC2 issues today.
const WELL_FORMED_VPC: &str = "vpc-0123456789abcdef0";

/// The longest a DNS label may be, in octets (RFC 1035 §2.3.4, the limit Route 53 measures
/// a hosted zone's name against); one octet more is a label no zone can carry.
const MAX_DNS_LABEL_OCTETS: usize = 63;

/// The Route 53 client the zone assertions read through: the emulator's own Route 53,
/// reached in process, addressed as the global service it is.
fn route53(router: &MultiServiceRouter) -> ::aws_sdk_route53::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_route53::Config::builder()
        .behavior_version(::aws_sdk_route53::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_route53::config::Region::new("us-east-1".to_string()))
        .endpoint_url("https://route53.amazonaws.com")
        .retry_config(::aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_route53::Client::from_conf(config)
}

/// One create of a DNS namespace: the operation it is made with and the request body.
struct Create {
    operation: &'static str,
    body: Value,
}

/// A `CreatePublicDnsNamespace` of `name`.
fn public(name: &str) -> Create {
    Create {
        operation: "CreatePublicDnsNamespace",
        body: json!({ "Name": name }),
    }
}

/// A `CreatePrivateDnsNamespace` of `name`, visible inside `vpc`.
fn private(name: &str, vpc: &str) -> Create {
    Create {
        operation: "CreatePrivateDnsNamespace",
        body: json!({ "Name": name, "Vpc": vpc }),
    }
}

/// Make the create of `create` and answer `(operation id, namespace id)`: the id the
/// synchronous call reports, and the namespace that operation targets.
async fn created(router: &MultiServiceRouter, case: &str, create: &Create) -> (String, String) {
    let answered = ok(router, create.operation, create.body.clone()).await;
    let operation_id = answered["OperationId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the synchronous call answers an OperationId, got {answered}")
        })
        .to_string();
    let filed = ok(
        router,
        "GetOperation",
        json!({ "OperationId": operation_id.clone() }),
    )
    .await;
    let namespace_id = filed["Operation"]["Targets"]["NAMESPACE"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the CREATE_NAMESPACE operation targets its namespace, got {filed}")
        })
        .to_string();
    (operation_id, namespace_id)
}

/// The names one `ListNamespaces` page carries.
async fn listed_names(router: &MultiServiceRouter) -> Vec<String> {
    let page = ok(router, "ListNamespaces", json!({})).await;
    page["Namespaces"]
        .as_array()
        .unwrap_or_else(|| panic!("ListNamespaces answers a Namespaces list, got {page}"))
        .iter()
        .filter_map(|summary| summary["Name"].as_str())
        .map(str::to_string)
        .collect()
}

/// A create whose hosted zone Route 53 refuses lands its operation `FAIL` with the
/// documented code and Route 53's own reason, and leaves no namespace behind.
#[tokio::test]
async fn a_dns_namespace_whose_hosted_zone_is_refused_fails_its_operation() {
    let (router, engine) = emulator_with_engine();
    let over_long_label = "l".repeat(MAX_DNS_LABEL_OCTETS + 1);

    // (case, the create, the fragment of Route 53's refusal the request put in it)
    let cases: Vec<(&str, Create, String)> = vec![
        (
            "a private namespace named by a single label",
            private("privatezonetld", WELL_FORMED_VPC),
            "privatezonetld".to_string(),
        ),
        (
            "a private namespace whose name has an empty label",
            private("empty..label.example.com", WELL_FORMED_VPC),
            "empty..label.example.com".to_string(),
        ),
        (
            "a private namespace whose name has an over-long label",
            private(&format!("{over_long_label}.example.com"), WELL_FORMED_VPC),
            over_long_label.clone(),
        ),
        (
            "a private namespace whose Vpc is not a VPC id",
            private("private-bad-vpc.example.com", "not-a-vpc"),
            "not-a-vpc".to_string(),
        ),
        (
            "a private namespace whose Vpc is a short form no account issues",
            private("private-short-vpc.example.com", "vpc-0a1b2c3"),
            "vpc-0a1b2c3".to_string(),
        ),
    ];

    for (case, create, fragment) in cases {
        let name = create.body["Name"]
            .as_str()
            .expect("every row names its namespace")
            .to_string();
        let (operation_id, namespace_id) = created(&router, case, &create).await;

        // The operation is settled by the clock, not by waiting for it: the service stores
        // when the transition is due and materialises it on read.
        set_operation_due(&engine, &operation_id, true);
        let settled = settled_operation(&router, &operation_id).await;
        let operation = &settled["Operation"];
        assert_eq!(
            (&operation["Status"], &operation["ErrorCode"]),
            (&json!("FAIL"), &json!("CANNOT_CREATE_HOSTED_ZONE")),
            "case {case:?}: a namespace whose hosted zone cannot be created must fail with \
             the documented code, got {settled}"
        );
        let message = operation["ErrorMessage"].as_str().unwrap_or_default();
        assert!(
            message.contains(&fragment),
            "case {case:?}: the failure reason must be the refusal naming {fragment:?}, got \
             {message:?}"
        );
        assert!(
            operation["OwnerAccount"]
                .as_str()
                .is_some_and(|account| !account.is_empty()),
            "case {case:?}: a failed operation still reports its owning account, got {settled}"
        );

        // Nothing of the namespace is left: no read, no listing entry, and the name is free.
        let (status, code, _) =
            refusal(&router, "GetNamespace", json!({ "Id": namespace_id.clone() })).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "NamespaceNotFound"),
            "case {case:?}: a create that failed leaves no namespace to read"
        );
        assert!(
            !listed_names(&router).await.contains(&name),
            "case {case:?}: a create that failed leaves nothing to list"
        );
        let retried = ok(&router, create.operation, create.body.clone()).await;
        assert!(
            retried["OperationId"].is_string(),
            "case {case:?}: the name a rolled-back create staged is free again, got {retried}"
        );
    }
}

/// The other side of the rule: a create whose zone Route 53 DOES hold reports that zone's
/// id, and the namespace reads back through it.
#[tokio::test]
async fn a_dns_namespace_whose_hosted_zone_is_created_reports_that_zone() {
    let (router, engine) = emulator_with_engine();

    // Rows differ along the namespace kind, i.e. along whether the zone is private (bound
    // to the requested VPC) or public.
    let cases: Vec<(&str, Create, bool)> = vec![
        (
            "a public namespace named by a domain name",
            public("zone-ok-public.example.com"),
            false,
        ),
        (
            "a private namespace named by a domain name",
            private("zone-ok-private.example.com", WELL_FORMED_VPC),
            true,
        ),
    ];

    for (case, create, private_zone) in cases {
        let name = create.body["Name"]
            .as_str()
            .expect("every row names its namespace")
            .to_string();
        let (operation_id, namespace_id) = created(&router, case, &create).await;
        set_operation_due(&engine, &operation_id, true);
        let settled = settled_operation(&router, &operation_id).await;
        assert_eq!(
            settled["Operation"]["Status"],
            json!("SUCCESS"),
            "case {case:?}: the zone was created, so the operation succeeds, got {settled}"
        );
        assert!(
            settled["Operation"]["ErrorCode"].is_null()
                && settled["Operation"]["ErrorMessage"].is_null(),
            "case {case:?}: a successful operation carries no error, got {settled}"
        );

        let read = ok(&router, "GetNamespace", json!({ "Id": namespace_id })).await;
        assert_eq!(
            read["Namespace"]["Name"],
            json!(name),
            "case {case:?}: the created namespace reads back, got {read}"
        );
        let zone_id = read["Namespace"]["Properties"]["DnsProperties"]["HostedZoneId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a DNS namespace carries its zone, got {read}"))
            .to_string();
        let zone = route53(&router)
            .get_hosted_zone()
            .id(zone_id.clone())
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: Route 53 must hold the zone {zone_id}: {error}")
            });
        assert_eq!(
            zone.hosted_zone().map(|zone| zone.name().to_string()),
            Some(format!("{name}.")),
            "case {case:?}: the zone Route 53 holds is the namespace's own name"
        );
        assert_eq!(
            zone.hosted_zone()
                .and_then(|zone| zone.config())
                .map(|config| config.private_zone()),
            Some(private_zone),
            "case {case:?}: a private namespace's zone is private and a public one's is not"
        );
    }
}
