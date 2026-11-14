// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of a hosted zone's DNS query logging configuration:
//! `CreateQueryLoggingConfig`, `GetQueryLoggingConfig`, `ListQueryLoggingConfigs` and
//! `DeleteQueryLoggingConfig`.
//!
//! The rule under test: a query logging configuration is the PAIR of one public hosted
//! zone of the account and one CloudWatch Logs log group the account holds — "Creates a
//! configuration for DNS query logging. After you create a query logging configuration,
//! Amazon Route 53 begins to publish log data to an Amazon CloudWatch Logs log group"
//! — so a create persists exactly that pair under a minted id and every later read
//! reports it, while a request whose pair is not one Route 53 can log is refused and
//! persists nothing: a hosted zone the account does not hold (`NoSuchHostedZone`, 404),
//! a private hosted zone ("You can log queries only for public hosted zones",
//! `InvalidInput`, 400), an ARN that names no log group of the account
//! (`NoSuchCloudWatchLogsLogGroup`, 404) and a zone that already has a configuration
//! ("You can create only one query logging configuration for a hosted zone",
//! `QueryLoggingConfigAlreadyExists`, 409). An id the store holds no configuration
//! under is `NoSuchQueryLoggingConfig` (404) for both the read and the delete, and a
//! delete frees its zone to be configured again.
//!
//! So the tables below drive that rule over a SET of inputs rather than one request:
//! the log group's name form (a flat name, the `/aws/route53/<zone>` prefix the
//! operation's own documentation recommends, a name carrying dots), the ARN form the
//! request names it by (with and without the trailing `:*` a log group's `arn` member
//! carries), the spelling of the hosted zone id (bare and `/hostedzone/`-prefixed), the
//! page size a listing is asked for, and each way a pair can fail to be loggable.
//!
//! The log group belongs to CloudWatch Logs, so this suite mounts BOTH services on one
//! engine and creates every log group through that service's own `CreateLogGroup` —
//! never by writing into a store by hand — and takes each ARN from the group
//! `DescribeLogGroups` reports, so nothing here hardcodes an account id.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;

use crate::hosted_zone_records::{call, create_zone_body, escape_xml, Reply};

/// The Region the CloudWatch Logs calls below are addressed to, which is also the
/// Region the log group ARNs they report carry.
const REGION: &str = "us-east-1";

/// The CloudWatch Logs operation prefix the `x-amz-target` header names, from that
/// service's own model (`Logs_20140328`).
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// The IPv4 block the VPC this suite registers carries. Immaterial to query logging —
/// nothing here reads it — but a VPC of the account has one, so the record is stored as
/// Amazon EC2 would report it.
const VPC_CIDR_BLOCK: &str = "10.0.0.0/16";

/// The VPC the one PRIVATE hosted zone in the refusal table is created with.
const PRIVATE_ZONE_VPC: &str = "vpc-0a1b2c3d4e5f60718";

/// The sentence a query logging configuration over a private hosted zone is refused
/// with, which is the rule `CreateQueryLoggingConfigRequest$HostedZoneId` states.
const PUBLIC_ONLY_MESSAGE: &str = "You can log queries only for public hosted zones.";

/// The sentence an id that names no query logging configuration is refused with.
const NO_SUCH_CONFIG_MESSAGE: &str = "The query logging configuration does not exist.";

/// A router with Route 53 and CloudWatch Logs mounted on ONE shared engine, whose
/// account already holds [`PRIVATE_ZONE_VPC`] in its Amazon EC2 inventory.
///
/// The log group a configuration publishes to belongs to CloudWatch Logs, so the rule
/// under test is only observable with both services reachable through the same engine —
/// which is how the router mounts them. The VPC is registered the way the sibling zone
/// suites register one: a private hosted zone is created WITH its first association, and
/// which VPCs the account holds is a fact of its Amazon EC2 inventory.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    {
        let state = ec2::state::service_state(&*engine)
            .expect("the shared engine must carry the Amazon EC2 store");
        let mut store = state.lock();
        store.vpcs.insert(
            PRIVATE_ZONE_VPC.to_string(),
            ec2::ops::types::Vpc {
                vpc_id: Some(PRIVATE_ZONE_VPC.to_string()),
                cidr_block: Some(VPC_CIDR_BLOCK.to_string()),
                state: Some(ec2::state::VPC_STATE_AVAILABLE.to_string()),
                ..Default::default()
            },
        );
        store
            .vpc_regions
            .insert(PRIVATE_ZONE_VPC.to_string(), REGION.to_string());
    }
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One CloudWatch Logs call, the way a client makes it.
async fn logs_call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(http::header::HOST, format!("logs.{REGION}.amazonaws.com"))
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        // The SigV4 credential scope is what routes the request to the `logs`
        // signing name.
        .header(
            http::header::AUTHORIZATION,
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/logs/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the log request serializes"),
        ))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.body().clone();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Both ARN forms of one log group: `arn` (with the trailing `:*`) and `logGroupArn`
/// (without it), as `DescribeLogGroups` reports them.
struct GroupArns {
    with_wildcard: String,
    plain: String,
}

/// Create the log group `name` through CloudWatch Logs' own API and report both ARN
/// forms the service holds it under.
async fn create_log_group(router: &MultiServiceRouter, name: &str) -> GroupArns {
    let (status, body) = logs_call(router, "CreateLogGroup", &json!({"logGroupName": name})).await;
    assert_eq!(
        status, 200,
        "creating log group {name:?}: expected 200, got {status} with body {body}"
    );
    let (status, body) = logs_call(
        router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "describing log group {name:?}: expected 200, got {status} with body {body}"
    );
    let group = body["logGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .unwrap_or_else(|| panic!("CloudWatch Logs must report the group {name:?}; body {body}"));
    let member = |key: &str| {
        group[key]
            .as_str()
            .unwrap_or_else(|| panic!("the reported group must carry {key}; body {body}"))
            .to_string()
    };
    GroupArns {
        with_wildcard: member("arn"),
        plain: member("logGroupArn"),
    }
}

/// A `CreateQueryLoggingConfig` body.
fn create_config_body(hosted_zone_id: &str, log_group_arn: &str) -> String {
    format!(
        "<CreateQueryLoggingConfigRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
         <HostedZoneId>{}</HostedZoneId>\
         <CloudWatchLogsLogGroupArn>{}</CloudWatchLogsLogGroupArn>\
         </CreateQueryLoggingConfigRequest>",
        escape_xml(hosted_zone_id),
        escape_xml(log_group_arn)
    )
}

/// `CreateQueryLoggingConfig`.
async fn create_config(
    router: &MultiServiceRouter,
    hosted_zone_id: &str,
    log_group_arn: &str,
) -> Reply {
    call(
        router,
        "POST",
        "/2013-04-01/queryloggingconfig",
        &create_config_body(hosted_zone_id, log_group_arn),
    )
    .await
}

/// `GetQueryLoggingConfig`.
async fn get_config(router: &MultiServiceRouter, id: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/2013-04-01/queryloggingconfig/{id}"),
        "",
    )
    .await
}

/// `DeleteQueryLoggingConfig`.
async fn delete_config(router: &MultiServiceRouter, id: &str) -> Reply {
    call(
        router,
        "DELETE",
        &format!("/2013-04-01/queryloggingconfig/{id}"),
        "",
    )
    .await
}

/// `ListQueryLoggingConfigs`, with `query` appended to its path.
async fn list_configs(router: &MultiServiceRouter, query: &str) -> Reply {
    call(
        router,
        "GET",
        &format!("/2013-04-01/queryloggingconfig{query}"),
        "",
    )
    .await
}

/// Create one PUBLIC hosted zone and report its bare (`Z…`) id.
async fn create_public_zone(router: &MultiServiceRouter, name: &str, caller_reference: &str) -> String {
    let reply = call(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &create_zone_body(name, caller_reference, ""),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "creating zone {name}: expected 201, got {} with body {}",
        reply.status, reply.body
    );
    reply
        .first("HostedZone/Id")
        .unwrap_or_else(|| panic!("the reply must carry HostedZone/Id; body {}", reply.body))
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// Create one PRIVATE hosted zone (associated with [`PRIVATE_ZONE_VPC`]) and report its
/// bare id.
async fn create_private_zone(
    router: &MultiServiceRouter,
    name: &str,
    caller_reference: &str,
) -> String {
    let config = format!(
        "<HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
         <VPC><VPCId>{PRIVATE_ZONE_VPC}</VPCId><VPCRegion>{REGION}</VPCRegion></VPC>"
    );
    let reply = call(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &create_zone_body(name, caller_reference, &config),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "creating private zone {name}: expected 201, got {} with body {}",
        reply.status, reply.body
    );
    reply
        .first("HostedZone/Id")
        .unwrap_or_else(|| panic!("the reply must carry HostedZone/Id; body {}", reply.body))
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// The `(Id, HostedZoneId, CloudWatchLogsLogGroupArn)` of every `QueryLoggingConfig` a
/// reply reports, in document order.
fn reported_configs(reply: &Reply, case: &str) -> Vec<(String, String, String)> {
    let ids = reply.all("QueryLoggingConfig/Id");
    let zones = reply.all("QueryLoggingConfig/HostedZoneId");
    let arns = reply.all("QueryLoggingConfig/CloudWatchLogsLogGroupArn");
    assert_eq!(
        (ids.len(), zones.len()),
        (arns.len(), arns.len()),
        "case {case:?}: every member of QueryLoggingConfig is required, so each reported \
         configuration must carry all three; body {}",
        reply.body
    );
    ids.into_iter()
        .zip(zones)
        .zip(arns)
        .map(|((id, zone), arn)| (id.to_string(), zone.to_string(), arn.to_string()))
        .collect()
}

/// The single `QueryLoggingConfig` a reply reports.
fn reported_config(reply: &Reply, case: &str) -> (String, String, String) {
    let reported = reported_configs(reply, case);
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected exactly one reported configuration, got {reported:?}; body {}",
        reply.body
    );
    reported.into_iter().next().expect("one configuration")
}

// ---------------------------------------------------------------------------
// The pair a create persists
// ---------------------------------------------------------------------------

/// One row of the create table: the zone, the log group, and the ARN form the request
/// names that group by.
struct CreateRow {
    case: &'static str,
    zone_name: &'static str,
    /// The CloudWatch Logs log group name, in one of the forms a caller uses. The
    /// `/aws/route53/<zone name>` form is the prefix `CreateQueryLoggingConfig`'s own
    /// documentation recommends ("we recommend that you use a consistent prefix, for
    /// example: /aws/route53/hosted zone name").
    log_group: &'static str,
    /// Whether the request names the group by the ARN form carrying the trailing `:*`
    /// (the `LogGroup$arn` member) rather than the plain one (`LogGroup$logGroupArn`).
    wildcard_arn: bool,
    /// Whether the request spells the hosted zone id with the `/hostedzone/` prefix a
    /// response hands back, rather than bare.
    prefixed_zone_id: bool,
}

#[tokio::test]
async fn creating_a_configuration_ties_a_public_zone_to_its_log_group() {
    let rows = [
        CreateRow {
            case: "the recommended /aws/route53 prefix, named by the plain ARN, bare zone id",
            zone_name: "logged.example.com",
            log_group: "/aws/route53/logged.example.com",
            wildcard_arn: false,
            prefixed_zone_id: false,
        },
        CreateRow {
            case: "a flat log group name, named by the ARN carrying the trailing :*",
            zone_name: "flat.example.net",
            log_group: "route53-queries",
            wildcard_arn: true,
            prefixed_zone_id: false,
        },
        CreateRow {
            case: "a log group name carrying dots, with the /hostedzone/-prefixed zone id",
            zone_name: "dotted.example.org",
            log_group: "aws.route53.queries.dotted",
            wildcard_arn: false,
            prefixed_zone_id: true,
        },
        CreateRow {
            case: "a nested log group path, the :* ARN and the prefixed zone id together",
            zone_name: "nested.example.co.uk",
            log_group: "/aws/route53/nested/queries",
            wildcard_arn: true,
            prefixed_zone_id: true,
        },
    ];

    let router = emulator();
    let mut created: Vec<(String, String, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let zone_id = create_public_zone(&router, row.zone_name, &format!("ref-{index}")).await;
        let arns = create_log_group(&router, row.log_group).await;
        let arn = if row.wildcard_arn {
            arns.with_wildcard
        } else {
            arns.plain
        };
        let requested_zone_id = if row.prefixed_zone_id {
            format!("/hostedzone/{zone_id}")
        } else {
            zone_id.clone()
        };

        let reply = create_config(&router, &requested_zone_id, &arn).await;
        assert_eq!(
            reply.status, 201,
            "case {case:?}: expected 201 from CreateQueryLoggingConfig, got {} with body {}",
            reply.status, reply.body
        );
        let (id, reported_zone, reported_arn) = reported_config(&reply, case);
        assert!(
            !id.is_empty(),
            "case {case:?}: the created configuration must carry an Id, got {id:?}"
        );
        assert_eq!(
            reported_zone, zone_id,
            "case {case:?}: expected HostedZoneId {zone_id}, got {reported_zone}"
        );
        assert_eq!(
            reported_arn, arn,
            "case {case:?}: expected CloudWatchLogsLogGroupArn {arn}, got {reported_arn}"
        );
        assert_eq!(
            reply.location.as_deref(),
            Some(
                format!(
                    "https://{}/2013-04-01/queryloggingconfig/{id}",
                    crate::hosted_zone_records::ENDPOINT
                )
                .as_str()
            ),
            "case {case:?}: the Location must be the URL the read is served at, got {:?}",
            reply.location
        );

        // The create is only a claim until a read answers from the store: the
        // configuration round-trips through the reader the Location names.
        let read = get_config(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetQueryLoggingConfig, got {} with body {}",
            read.status, read.body
        );
        assert_eq!(
            reported_config(&read, case),
            (id.clone(), zone_id.clone(), arn.clone()),
            "case {case:?}: the read must report the persisted pair; body {}",
            read.body
        );
        // A read is a report: reading it again says the same thing.
        let again = get_config(&router, &id).await;
        assert_eq!(
            reported_config(&again, case),
            reported_config(&read, case),
            "case {case:?}: two reads of one configuration must agree; bodies {} and {}",
            read.body,
            again.body
        );

        // "If you want to list the query logging configuration that is associated with
        // a hosted zone, specify the ID in HostedZoneId."
        let filtered = list_configs(&router, &format!("?hostedzoneid={zone_id}")).await;
        assert_eq!(
            filtered.status, 200,
            "case {case:?}: expected 200 from ListQueryLoggingConfigs, got {} with body {}",
            filtered.status, filtered.body
        );
        assert_eq!(
            reported_configs(&filtered, case),
            vec![(id.clone(), zone_id.clone(), arn.clone())],
            "case {case:?}: the zone's listing must report just its own configuration; body {}",
            filtered.body
        );

        created.push((id, zone_id, arn));
    }

    // Every configuration the account holds, in configuration-id order: one listing
    // reports all of them, so no create was lost or overwritten by a later one.
    let all = list_configs(&router, "").await;
    let mut expected = created.clone();
    expected.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        reported_configs(&all, "every configuration"),
        expected,
        "the account's listing must report every created configuration in id order; body {}",
        all.body
    );
}

// ---------------------------------------------------------------------------
// The pairs a create refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: what the request names, and how Route 53 answers.
struct RefusalRow {
    case: &'static str,
    /// Which stored zone (or which literal id) the request names.
    zone: ZoneUnder,
    /// Which log group ARN (or which literal value) the request names.
    log_group: GroupUnder,
    status: u16,
    code: &'static str,
    /// The message the refusal carries, when it is fixed text.
    message: Option<&'static str>,
}

/// The hosted zone a refusal row's request names.
enum ZoneUnder {
    /// The public zone this suite created.
    Public,
    /// The private zone this suite created.
    Private,
    /// A literal id, as the request spells it.
    Literal(&'static str),
}

/// The log group a refusal row's request names.
enum GroupUnder {
    /// The log group this suite created, by its plain ARN.
    Created,
    /// The log group this suite created, with `:log-stream:<name>` appended — a log
    /// STREAM of it, which is not a log group.
    StreamOfCreated(&'static str),
    /// A literal value, as the request spells it.
    Literal(&'static str),
}

#[tokio::test]
async fn a_create_is_refused_when_the_pair_is_not_one_route_53_can_log() {
    let router = emulator();
    let public_zone = create_public_zone(&router, "refusals.example.com", "refusals-public").await;
    let private_zone =
        create_private_zone(&router, "private.example.com", "refusals-private").await;
    let group = create_log_group(&router, "/aws/route53/refusals.example.com").await;

    let rows = [
        RefusalRow {
            case: "a well-formed hosted zone id the account does not hold",
            zone: ZoneUnder::Literal("Z1D633PJN98FT9"),
            log_group: GroupUnder::Created,
            status: 404,
            code: "NoSuchHostedZone",
            message: Some("No hosted zone found with ID: Z1D633PJN98FT9"),
        },
        RefusalRow {
            case: "the same missing zone, spelled with the /hostedzone/ prefix",
            zone: ZoneUnder::Literal("/hostedzone/Z1D633PJN98FT9"),
            log_group: GroupUnder::Created,
            status: 404,
            code: "NoSuchHostedZone",
            message: Some("No hosted zone found with ID: Z1D633PJN98FT9"),
        },
        RefusalRow {
            case: "a private hosted zone",
            zone: ZoneUnder::Private,
            log_group: GroupUnder::Created,
            status: 400,
            code: "InvalidInput",
            message: Some(PUBLIC_ONLY_MESSAGE),
        },
        RefusalRow {
            case: "a well-formed log group ARN for a group nobody created",
            zone: ZoneUnder::Public,
            log_group: GroupUnder::Literal(
                "arn:aws:logs:us-east-1:000000000000:log-group:/aws/route53/never-created",
            ),
            status: 404,
            code: "NoSuchCloudWatchLogsLogGroup",
            message: Some("The specified CloudWatch Logs log group doesn't exist."),
        },
        RefusalRow {
            case: "the ARN of a log STREAM of a group the account does hold",
            zone: ZoneUnder::Public,
            log_group: GroupUnder::StreamOfCreated("queries"),
            status: 404,
            code: "NoSuchCloudWatchLogsLogGroup",
            message: Some("The specified CloudWatch Logs log group doesn't exist."),
        },
        RefusalRow {
            case: "an ARN of another service",
            zone: ZoneUnder::Public,
            log_group: GroupUnder::Literal("arn:aws:sns:us-east-1:000000000000:route53-queries"),
            status: 404,
            code: "NoSuchCloudWatchLogsLogGroup",
            message: Some("The specified CloudWatch Logs log group doesn't exist."),
        },
        RefusalRow {
            case: "the log group's NAME rather than its ARN",
            zone: ZoneUnder::Public,
            log_group: GroupUnder::Literal("/aws/route53/refusals.example.com"),
            status: 404,
            code: "NoSuchCloudWatchLogsLogGroup",
            message: Some("The specified CloudWatch Logs log group doesn't exist."),
        },
    ];

    for row in &rows {
        let case = row.case;
        let zone_id = match row.zone {
            ZoneUnder::Public => public_zone.clone(),
            ZoneUnder::Private => private_zone.clone(),
            ZoneUnder::Literal(literal) => literal.to_string(),
        };
        let arn = match row.log_group {
            GroupUnder::Created => group.plain.clone(),
            GroupUnder::StreamOfCreated(stream) => format!("{}:log-stream:{stream}", group.plain),
            GroupUnder::Literal(literal) => literal.to_string(),
        };

        let reply = create_config(&router, &zone_id, &arn).await;
        assert_eq!(
            reply.status, row.status,
            "case {case:?}: expected HTTP {}, got {} with body {}",
            row.status, reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            row.code,
            "case {case:?}: expected error code {}, got {:?}; body {}",
            row.code,
            reply.error_code(),
            reply.body
        );
        if let Some(expected) = row.message {
            assert_eq!(
                reply.first("Error/Message"),
                Some(expected),
                "case {case:?}: expected message {expected:?}, got {:?}; body {}",
                reply.first("Error/Message"),
                reply.body
            );
        }
    }

    // Nothing in the table was persisted: a refused create leaves the account with no
    // configuration at all.
    let listed = list_configs(&router, "").await;
    assert_eq!(
        reported_configs(&listed, "after the refusals"),
        Vec::new(),
        "a refused create must persist nothing; body {}",
        listed.body
    );

    // One create that IS loggable, and then the second one for the same zone, which is
    // the conflict the model carries its own error for.
    let created = create_config(&router, &public_zone, &group.plain).await;
    assert_eq!(
        created.status, 201,
        "the loggable pair must be created, got {} with body {}",
        created.status, created.body
    );
    let (id, _, _) = reported_config(&created, "the loggable pair");
    let repeated = create_config(&router, &public_zone, &group.with_wildcard).await;
    assert_eq!(
        repeated.status, 409,
        "a second configuration for one zone must be refused with 409, got {} with body {}",
        repeated.status, repeated.body
    );
    assert_eq!(
        repeated.error_code(),
        "QueryLoggingConfigAlreadyExists",
        "a second configuration for one zone must be refused as QueryLoggingConfigAlreadyExists, \
         got {:?}; body {}",
        repeated.error_code(),
        repeated.body
    );
    let listed = list_configs(&router, &format!("?hostedzoneid={public_zone}")).await;
    assert_eq!(
        reported_configs(&listed, "after the repeated create"),
        vec![(id, public_zone.clone(), group.plain.clone())],
        "the refused second create must leave the first configuration as it was; body {}",
        listed.body
    );
}

// ---------------------------------------------------------------------------
// The id a read and a delete resolve
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_configuration_id_is_refused_by_the_read_and_the_delete() {
    let router = emulator();
    let zone = create_public_zone(&router, "deleted.example.com", "unknown-id").await;
    let group = create_log_group(&router, "/aws/route53/deleted.example.com").await;
    let created = create_config(&router, &zone, &group.plain).await;
    assert_eq!(
        created.status, 201,
        "the configuration under test must be created, got {} with body {}",
        created.status, created.body
    );
    let (held, _, _) = reported_config(&created, "the held configuration");
    // A configuration that was created and then deleted: its id is as unknown as one
    // that was never minted.
    let dropped = create_config(
        &router,
        &create_public_zone(&router, "dropped.example.com", "dropped").await,
        &create_log_group(&router, "/aws/route53/dropped.example.com")
            .await
            .plain,
    )
    .await;
    let (dropped_id, _, _) = reported_config(&dropped, "the dropped configuration");
    let deleted = delete_config(&router, &dropped_id).await;
    assert_eq!(
        deleted.status, 200,
        "deleting a held configuration must answer 200, got {} with body {}",
        deleted.status, deleted.body
    );

    // Ids that name no configuration: the minted form nobody minted, the id of a
    // deleted configuration, an id in another resource's form, and a bare word.
    let unknown = [
        ("a well-formed configuration id nobody minted", "11111111-2222-3333-4444-555555555555".to_string()),
        ("the id of a deleted configuration", dropped_id.clone()),
        ("an id in a hosted zone's form", "Z1D633PJN98FT9".to_string()),
        ("a bare word", "not-a-configuration".to_string()),
    ];
    for (case, id) in &unknown {
        for (operation, reply) in [
            ("GetQueryLoggingConfig", get_config(&router, id).await),
            ("DeleteQueryLoggingConfig", delete_config(&router, id).await),
        ] {
            assert_eq!(
                reply.status, 404,
                "case {case:?}: expected 404 from {operation}, got {} with body {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error_code(),
                "NoSuchQueryLoggingConfig",
                "case {case:?}: expected NoSuchQueryLoggingConfig from {operation}, got {:?}; \
                 body {}",
                reply.error_code(),
                reply.body
            );
            assert_eq!(
                reply.first("Error/Message"),
                Some(NO_SUCH_CONFIG_MESSAGE),
                "case {case:?}: expected message {NO_SUCH_CONFIG_MESSAGE:?} from {operation}, got \
                 {:?}; body {}",
                reply.first("Error/Message"),
                reply.body
            );
        }
    }

    // None of those refusals touched the configuration the account does hold.
    let read = get_config(&router, &held).await;
    assert_eq!(
        read.status, 200,
        "the held configuration must still be readable, got {} with body {}",
        read.status, read.body
    );
    assert_eq!(
        reported_config(&read, "the held configuration"),
        (held, zone, group.plain),
        "the held configuration must be unchanged; body {}",
        read.body
    );
}

#[tokio::test]
async fn deleting_a_configuration_frees_its_hosted_zone_to_be_configured_again() {
    let router = emulator();
    let zone = create_public_zone(&router, "recycled.example.com", "recycled").await;
    let first_group = create_log_group(&router, "/aws/route53/recycled.example.com").await;
    let second_group = create_log_group(&router, "route53-recycled-queries").await;

    let created = create_config(&router, &zone, &first_group.plain).await;
    assert_eq!(
        created.status, 201,
        "the first configuration must be created, got {} with body {}",
        created.status, created.body
    );
    let (first_id, _, _) = reported_config(&created, "the first configuration");

    let deleted = delete_config(&router, &first_id).await;
    assert_eq!(
        deleted.status, 200,
        "the delete must answer 200, got {} with body {}",
        deleted.status, deleted.body
    );
    let read = get_config(&router, &first_id).await;
    assert_eq!(
        (read.status, read.error_code()),
        (404, "NoSuchQueryLoggingConfig"),
        "a deleted configuration must stop resolving; body {}",
        read.body
    );
    let listed = list_configs(&router, &format!("?hostedzoneid={zone}")).await;
    assert_eq!(
        reported_configs(&listed, "after the delete"),
        Vec::new(),
        "a deleted configuration must leave its zone with none; body {}",
        listed.body
    );

    // The zone can be configured again, against a DIFFERENT log group, and the new
    // configuration is its own resource.
    let recreated = create_config(&router, &zone, &second_group.with_wildcard).await;
    assert_eq!(
        recreated.status, 201,
        "the zone must take a new configuration, got {} with body {}",
        recreated.status, recreated.body
    );
    let (second_id, second_zone, second_arn) = reported_config(&recreated, "the new configuration");
    assert_ne!(
        second_id, first_id,
        "the new configuration must carry an id of its own, got {second_id} twice"
    );
    assert_eq!(
        (second_zone.as_str(), second_arn.as_str()),
        (zone.as_str(), second_group.with_wildcard.as_str()),
        "the new configuration must report the pair it was created from; body {}",
        recreated.body
    );
}

// ---------------------------------------------------------------------------
// How the listing pages
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_pages_over_the_configurations_in_id_order() {
    let router = emulator();
    // Five zones, so every page size in the table below leaves a remainder to resume
    // at. The zone with no configuration is the sixth.
    let mut expected: Vec<(String, String, String)> = Vec::new();
    for index in 0..5u32 {
        let zone_name = format!("paged-{index}.example.com");
        let zone = create_public_zone(&router, &zone_name, &format!("paged-{index}")).await;
        let group = create_log_group(&router, &format!("/aws/route53/{zone_name}")).await;
        let reply = create_config(&router, &zone, &group.plain).await;
        assert_eq!(
            reply.status, 201,
            "creating the configuration of {zone_name}: expected 201, got {} with body {}",
            reply.status, reply.body
        );
        let config = reported_config(&reply, &zone_name);
        expected.push(config);
    }
    expected.sort_by(|left, right| left.0.cmp(&right.0));

    // Each row is a page size; paging through at that size must report the same total
    // order as one unpaged listing.
    for page_size in [1usize, 2, 3, 5, 100] {
        let case = format!("maxresults={page_size}");
        let mut seen: Vec<(String, String, String)> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let query = match &token {
                Some(token) => format!("?maxresults={page_size}&nexttoken={token}"),
                None => format!("?maxresults={page_size}"),
            };
            let reply = list_configs(&router, &query).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected 200 from ListQueryLoggingConfigs, got {} with body {}",
                reply.status, reply.body
            );
            let page = reported_configs(&reply, &case);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page must carry at most {page_size} configurations, got {}",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: paging must terminate, got {pages} pages"
            );
            token = reply.first("NextToken").map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, expected,
            "case {case:?}: paging must report every configuration in id order"
        );
    }

    // A zone of the account that has no configuration reports an empty listing, not a
    // refusal; a zone the account does not hold is a missing hosted zone.
    let unconfigured = create_public_zone(&router, "unconfigured.example.com", "unconfigured").await;
    let empty = list_configs(&router, &format!("?hostedzoneid={unconfigured}")).await;
    assert_eq!(
        (empty.status, reported_configs(&empty, "an unconfigured zone")),
        (200, Vec::new()),
        "a zone with no configuration must report an empty listing; body {}",
        empty.body
    );
    let missing = list_configs(&router, "?hostedzoneid=Z1D633PJN98FT9").await;
    assert_eq!(
        (missing.status, missing.error_code()),
        (404, "NoSuchHostedZone"),
        "a listing filtered on a zone the account does not hold must be refused; body {}",
        missing.body
    );

    // A token this operation never minted identifies no page.
    for token in ["not-a-token", "Z1D633PJN98FT9", "11111111-2222-3333"] {
        let reply = list_configs(&router, &format!("?nexttoken={token}")).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "InvalidPaginationToken"),
            "the token {token:?} identifies no page, so the listing must refuse it; body {}",
            reply.body
        );
    }
}
