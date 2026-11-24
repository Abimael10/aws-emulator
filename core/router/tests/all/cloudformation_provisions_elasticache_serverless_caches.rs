// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ElastiCache::ServerlessCache` has really
//! created that cache — in Amazon ElastiCache, placed in the account's own subnets — and a
//! declaration that service REFUSES fails the resource instead of being reported created.
//!
//! The rule under test: **a stack reaches its completed status only once the service that
//! owns each declared resource has created it, through that service's own create, so every
//! rule the create applies to the declared properties decides the resource's fate; a create
//! that service refuses fails the resource, and a stack created with `OnFailure=ROLLBACK`
//! unwinds, leaving nothing behind**. Three crates have to agree for that to hold
//! (CloudFormation makes the call, ElastiCache serves the cache, Amazon EC2 owns the subnets
//! and the security groups it is placed in), so the suite lives here.
//!
//! The ACCEPTED rows differ along what the rule generalises over: which engine the template
//! declares, how many subnets and security groups it places the cache in, whether it states a
//! description, a usage limit, a snapshot posture, a major engine version or tags, and how
//! the declared name is cased.
//!
//! The REFUSED rows differ along WHICH rule of the owning service the declaration breaks —
//! a major engine version the declared engine does not offer, a subnet identifier EC2 holds
//! no subnet for, and a usage-limit bound outside the published range — so what is asserted
//! is that the create is the owning service's own, not that one recorded template fails.
//!
//! Nothing here reaches into either service's state: a cache exists only because
//! `DescribeServerlessCaches` reported it, and a subnet only because EC2 minted it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The API versions each of the three wires carries beside `Action`.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
const EC2_API_VERSION: &str = "2016-11-15";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack's own clock-owed transition and the created cache's
/// provisioning span both have to come due, and the suite waits for the CONDITION (the
/// stack's status, the cache being gone) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The logical id every template below declares its cache under.
const CACHE_LOGICAL_ID: &str = "Cache";

/// An identifier of the EC2 subnet form that no `CreateSubnet` below minted, so it reaches
/// ElastiCache's subnet resolution and resolves to nothing.
const ABSENT_SUBNET: &str = "subnet-00000000deadbeef";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticache_amazonelasticachev9::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one query-protocol operation of `service`, at `version`, with these form members.
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
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        action,
        members,
    )
    .await
}

/// Call one Amazon ElastiCache operation over its awsQuery wire.
async fn elasticache(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "elasticache", ELASTICACHE_API_VERSION, action, members).await
}

/// One Amazon EC2 action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The account's network for this suite, built through Amazon EC2's own operations: a VPC,
/// one subnet per zone and one security group, so every identifier a template below hands
/// ElastiCache is one the owner of those resources handed out.
struct Network {
    subnets: Vec<String>,
    security_group: String,
}

/// Build [`Network`] with `zones.len()` subnets.
async fn network(router: &MultiServiceRouter, zones: &[&str]) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.7.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.7.{}.0/24", index + 1);
        let body = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr.as_str()),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&body, "subnetId"));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "cfn-serverless-cache"),
                ("GroupDescription", "caches a stack declares"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnets,
        security_group,
    }
}

/// The `"SubnetIds":[…]` / `"SecurityGroupIds":[…]` template members for these identifiers.
fn id_list(property: &str, ids: &[&str]) -> String {
    let quoted: Vec<String> = ids.iter().map(|id| format!("\"{id}\"")).collect();
    format!(r#","{property}":[{}]"#, quoted.join(","))
}

/// The template one row deploys: the cache alone, with its `Ref` and the addresses of both
/// endpoints published as outputs.
fn template_of(cache_name: &str, placement: &str, extra_properties: &str) -> String {
    let mut template = String::new();
    template.push_str(r#"{"Resources":{""#);
    template.push_str(CACHE_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::ElastiCache::ServerlessCache","Properties":{"#);
    template.push_str(&format!(r#""ServerlessCacheName":"{cache_name}""#));
    template.push_str(placement);
    template.push_str(extra_properties);
    template.push_str(r#"}}},"Outputs":{"#);
    for (output, attribute) in [
        ("Endpoint", "Endpoint.Address"),
        ("Reader", "ReaderEndpoint.Address"),
    ] {
        template.push_str(&format!(
            r#""{output}":{{"Value":{{"Fn::GetAtt":["{CACHE_LOGICAL_ID}","{attribute}"]}}}},"#
        ));
    }
    template.push_str(&format!(
        r#""CacheRef":{{"Value":{{"Ref":"{CACHE_LOGICAL_ID}"}}}}}}}}"#
    ));
    template
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events:
/// the reasons the owning service's refusal is quoted in.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack `stack_name` from `template`, with the rollback a failed resource unwinds
/// through, and answer its settled `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            ("OnFailure", "ROLLBACK"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// `DescribeServerlessCaches` for one name, as the status and body a client reads.
async fn describe_cache(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    elasticache(
        router,
        "DescribeServerlessCaches",
        &[("ServerlessCacheName", name)],
    )
    .await
}

/// One ACCEPTED row: a stack whose only resource is a serverless cache the owning service
/// takes.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The `ServerlessCacheName` the template states, which is this resource's `Ref`.
    cache_name: &'static str,
    engine: &'static str,
    /// How many of the account's subnets, and how many of its security groups, the template
    /// places the cache in. `0` subnets is a declaration that names none at all, which leaves
    /// the placement to the owning service.
    subnets: usize,
    security_groups: usize,
    /// Further properties the row declares, as template JSON members (each with its leading
    /// comma), and what ElastiCache must then report for them.
    extra_properties: &'static str,
    reported: &'static [(&'static str, &'static str)],
}

/// The accepted rows. The engines are the ones a serverless cache runs
/// (`CreateServerlessCache$Engine`: "The name of the cache engine to be used for creating the
/// serverless cache"), and each row declares a different part of the type's surface.
const ROWS: &[Row] = &[
    Row {
        case: "a Valkey cache in two subnets, with a description and tags",
        stack_name: "esc-valkey-two-subnets",
        cache_name: "cfn-esc-valkey",
        engine: "valkey",
        subnets: 2,
        security_groups: 1,
        extra_properties: r#","Description":"the valkey cache a stack declares","MajorEngineVersion":"8","Tags":[{"Key":"team","Value":"platform"}]"#,
        reported: &[
            ("Engine", "valkey"),
            ("Description", "the valkey cache a stack declares"),
            ("MajorEngineVersion", "8"),
        ],
    },
    Row {
        case: "a Redis OSS cache in one subnet, with a usage limit and a snapshot posture",
        stack_name: "esc-redis-one-subnet",
        cache_name: "cfn-esc-redis",
        engine: "redis",
        subnets: 1,
        security_groups: 1,
        extra_properties: r#","SnapshotRetentionLimit":3,"DailySnapshotTime":"04:00","CacheUsageLimits":{"DataStorage":{"Maximum":10,"Unit":"GB"},"ECPUPerSecond":{"Maximum":5000}}"#,
        reported: &[("Engine", "redis"), ("SnapshotRetentionLimit", "3")],
    },
    Row {
        case: "a Memcached cache whose declared name is mixed case and whose placement is \
               left to the service",
        stack_name: "esc-memcached-default-placement",
        cache_name: "Cfn-ESC-Memcached",
        engine: "memcached",
        subnets: 0,
        security_groups: 0,
        extra_properties: "",
        reported: &[("Engine", "memcached")],
    },
];

#[tokio::test]
async fn a_completed_stack_has_created_its_serverless_cache_in_elasticache() {
    let router = build_router();
    let network = network(&router, &["us-east-1a", "us-east-1b"]).await;

    for row in ROWS {
        let case = row.case;
        let subnets: Vec<&str> = network
            .subnets
            .iter()
            .take(row.subnets)
            .map(String::as_str)
            .collect();
        let groups: Vec<&str> = std::iter::once(network.security_group.as_str())
            .take(row.security_groups)
            .collect();
        let mut placement = format!(r#","Engine":"{}""#, row.engine);
        if !subnets.is_empty() {
            placement.push_str(&id_list("SubnetIds", &subnets));
        }
        if !groups.is_empty() {
            placement.push_str(&id_list("SecurityGroupIds", &groups));
        }
        let template = template_of(row.cache_name, &placement, row.extra_properties);

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body} with the failure \
             reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );

        // The handle the stack publishes for the cache: "Ref returns the serverless cache
        // name", which is the identifier every ElastiCache call addresses it by.
        let outputs = outputs_of(&body);
        let cache_ref = outputs
            .get("CacheRef")
            .unwrap_or_else(|| panic!("case {case:?}: no CacheRef output in {body}"))
            .clone();
        assert_eq!(
            cache_ref, row.cache_name,
            "case {case:?}: expected the declared ServerlessCacheName as the Ref, got \
             {cache_ref:?}"
        );

        // THE RULE: the cache the stack claims to have created is one ElastiCache holds,
        // under exactly that identifier, with the configuration the template declared.
        let (status, reported) = describe_cache(&router, &cache_ref).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ElastiCache to report the cache the stack created, got \
             {status} with body {reported}"
        );
        assert_eq!(
            opt_element(&reported, "ServerlessCacheName").as_deref(),
            Some(cache_ref.to_ascii_lowercase().as_str()),
            "case {case:?}: expected the cache to be held under the published handle, got \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "Status").as_deref(),
            Some("available"),
            "case {case:?}: expected a completed stack to leave the cache available, got \
             {reported}"
        );
        for (member, value) in row.reported {
            assert_eq!(
                opt_element(&reported, member).as_deref(),
                Some(*value),
                "case {case:?}: expected {member} to be {value:?}, got {reported}"
            );
        }
        // The placement is the account's own: every subnet the template named is one EC2
        // minted, and the cache reports it back.
        for subnet in &subnets {
            assert!(
                elements(&reported, "SubnetId").iter().any(|held| held == subnet),
                "case {case:?}: expected the cache to sit in {subnet:?}, got {reported}"
            );
        }

        // The endpoints the stack published for the cache are the ones that service reports
        // for it, not values CloudFormation invented.
        for (output, published) in [("Endpoint", "Endpoint"), ("Reader", "ReaderEndpoint")] {
            let address = outputs
                .get(output)
                .unwrap_or_else(|| panic!("case {case:?}: no {output} output in {body}"))
                .clone();
            let block = block_of(&reported, published)
                .unwrap_or_else(|| panic!("case {case:?}: no <{published}> in {reported}"));
            assert_eq!(
                Some(address.as_str()),
                opt_element(block, "Address").as_deref(),
                "case {case:?}: expected the published {output} to be ElastiCache's own, got \
                 {block}"
            );
        }

        // And the cache the stack created takes a live call, which is what makes it the
        // account's cache rather than a record CloudFormation holds alone.
        let (status, modified) = elasticache(
            &router,
            "ModifyServerlessCache",
            &[
                ("ServerlessCacheName", cache_ref.as_str()),
                ("Description", "modified through the service"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stack's cache to take a modify, got {status} with \
             body {modified}"
        );
        assert_eq!(
            opt_element(&modified, "Description").as_deref(),
            Some("modified through the service"),
            "case {case:?}: expected the modify to be applied, got {modified}"
        );
    }
}

/// One REFUSED row: a declaration the owning service does not take, and the sentence its
/// refusal carries — which CloudFormation records as the failed resource's status reason.
struct RefusedRow {
    case: &'static str,
    stack_name: &'static str,
    cache_name: &'static str,
    /// The properties the row declares beside the name, built from the account's network.
    properties: fn(&Network) -> String,
    /// The fixed opening of ElastiCache's own refusal sentence for that rule.
    sentence: fn(&Network) -> String,
}

/// The refused rows: one per RULE of the owning service a declaration can break, so the
/// assertion is that the stack's create is that service's own rather than that one template
/// fails.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a major engine version the declared engine does not offer",
        stack_name: "esc-bad-major-version",
        cache_name: "cfn-esc-bad-version",
        properties: |network| {
            format!(
                r#","Engine":"redis","MajorEngineVersion":"999.999"{}"#,
                id_list(
                    "SubnetIds",
                    &network.subnets.iter().map(String::as_str).collect::<Vec<_>>()
                )
            )
        },
        sentence: |_| {
            "Specified engine does not support major engine version (999.999).".to_string()
        },
    },
    RefusedRow {
        case: "a subnet identifier Amazon EC2 holds no subnet for",
        stack_name: "esc-absent-subnet",
        cache_name: "cfn-esc-absent-subnet",
        properties: |network| {
            format!(
                r#","Engine":"valkey"{}"#,
                id_list("SubnetIds", &[network.subnets[0].as_str(), ABSENT_SUBNET])
            )
        },
        sentence: |network| {
            format!(
                "Some input subnets in :[{}, {ABSENT_SUBNET}] are invalid.",
                network.subnets[0]
            )
        },
    },
    RefusedRow {
        case: "a usage-limit bound outside the published range",
        stack_name: "esc-usage-limit-out-of-range",
        cache_name: "cfn-esc-usage-limit",
        properties: |network| {
            let limits = r#","CacheUsageLimits":{"DataStorage":{"Maximum":100000,"Unit":"GB"}}"#;
            format!(
                r#","Engine":"redis"{limits}{}"#,
                id_list(
                    "SubnetIds",
                    &network.subnets.iter().map(String::as_str).collect::<Vec<_>>()
                )
            )
        },
        sentence: |_| "Invalid value for DataStorage limit.".to_string(),
    },
];

#[tokio::test]
async fn a_declaration_elasticache_refuses_rolls_the_stack_back_with_no_cache_left_behind() {
    let router = build_router();
    let network = network(&router, &["us-east-1a", "us-east-1b"]).await;

    for row in REFUSED_ROWS {
        let case = row.case;
        let template = template_of(row.cache_name, &(row.properties)(&network), "");
        let body = create_and_settle(&router, case, row.stack_name, &template).await;

        // THE RULE: the create is the owning service's own, so its refusal fails the
        // resource and the stack unwinds.
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the stack to roll back, got {body} with the failure \
             reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let expected = (row.sentence)(&network);
        let reasons = failure_reasons(&router, row.stack_name).await;
        assert!(
            reasons.iter().any(|reason| reason.contains(&expected)),
            "case {case:?}: expected a failure reason quoting {expected:?}, got {reasons:?}"
        );

        // And nothing is left behind: the cache the template asked for was never created,
        // so the owning service answers its own not-found for that name.
        let (status, reported) = describe_cache(&router, row.cache_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected no cache to be left behind, got {status} with body \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "Code").as_deref(),
            Some("ServerlessCacheNotFoundFault"),
            "case {case:?}: expected the modelled not-found code, got {reported}"
        );
        let not_found = format!(
            "Serverless cache with name {} not found.",
            row.cache_name
        );
        assert_eq!(
            opt_element(&reported, "Message").as_deref(),
            Some(not_found.as_str()),
            "case {case:?}: expected the not-found sentence {not_found:?}, got {reported}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_serverless_cache_it_created() {
    let case = "delete the stack that created the serverless cache";
    let stack_name = "esc-deleted";
    let cache_name = "cfn-esc-deleted";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;

    let placement = format!(
        r#","Engine":"valkey"{}"#,
        id_list("SubnetIds", &[network.subnets[0].as_str()])
    );
    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(cache_name, &placement, ""),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons \
         {:?}",
        failure_reasons(&router, stack_name).await
    );
    let (status, reported) = describe_cache(&router, cache_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the cache to exist before the delete, got {status} with body \
         {reported}"
    );

    let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );

    // The cache the stack created goes with the stack. ElastiCache leaves a deleted cache
    // `deleting` until the removal its clock owes comes due, so the suite waits for the
    // CONDITION — the cache being gone — rather than for a fixed time.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, reported) = describe_cache(&router, cache_name).await;
        if status == 404 {
            assert_eq!(
                opt_element(&reported, "Code").as_deref(),
                Some("ServerlessCacheNotFoundFault"),
                "case {case:?}: expected the modelled not-found code, got {reported}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the cache to be gone with the stack, last read {status} \
             with body {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}
