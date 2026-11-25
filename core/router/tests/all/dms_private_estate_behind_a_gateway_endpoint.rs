// SPDX-License-Identifier: CC0-1.0

//! The private Database Migration Service estate a client builds behind a GATEWAY VPC
//! endpoint, over the two services that own its halves: Amazon EC2 holds the VPC, its route
//! tables, its subnets and the endpoint, and DMS holds the replication subnet group and the
//! replication instance placed in that network.
//!
//! RULE: `RouteTableIds` is an ordinary modelled input member of `CreateVpcEndpoint`, which
//! models no error at all, so a gateway endpoint create naming route tables of its own VPC —
//! the implicit main table a VPC comes with included — is SERVED: Amazon EC2 answers HTTP 200
//! with the endpoint in the state its type is created in, associated with exactly the tables
//! the request named and reached from each of them through a route to the endpoint service's
//! prefix list; and what is placed in that VPC afterwards resolves against the same network,
//! so a replication subnet group reports the VPC and the zones EC2 placed its subnets in, and
//! a replication instance created in that group is carried from its transient status to
//! `available` by the clock and reports the group it was placed in. No refusal is permissible
//! for this request shape, and least of all a 5xx, which no SDK models for an operation whose
//! `errors` list is empty.
//!
//! Only a cross-service suite can pin this: the network, the tables and the endpoint are
//! EC2's, while the subnet group and the instance are DMS's, so the estate is observable only
//! with both services mounted on ONE engine. The data-plane run of a task on such an instance
//! is the sibling suite `dms_s3_full_load_task_run.rs`.
//!
//! The rows differ along what the rule generalises over: which gateway endpoint service of
//! the Region the endpoint is created to, WHICH of the VPC's route tables the create
//! associates (the implicit main table alone, the main table together with tables created
//! beside it, a created table while the main one is left out, and no table at all), how many
//! Availability Zones the estate's subnets spread over, and whether the replication instance
//! is a Multi-AZ deployment.
//!
//! Sources: `smithy_query(query="operation", operation="CreateVpcEndpoint")` on the Amazon
//! EC2 model — `RouteTableIds` targets `VpcEndpointRouteTableIdList` (xmlName
//! `RouteTableId`), and the operation's `errors` list is empty; `DescribeRouteTables`'
//! documented `association.main` filter ("indicates whether the route table is the main route
//! table") is what names the table a VPC comes with, and `DescribePrefixLists` ("the prefix
//! list name and prefix list ID of the service") is what resolves the destination the
//! endpoint's route carries.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, elements, form_encode, items_of, opt_element};

/// The Region every request below is signed for, and the Region of the endpoint services the
/// rows name.
const REGION: &str = "us-east-1";

/// The Amazon EC2 API version an ec2Query client sends in the form body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of DMS's awsJson1_1 operations, which is its Smithy service
/// shape name.
const DMS_TARGET_PREFIX: &str = "AmazonDMSv20160101";

/// The endpoint type the rows create, the one a service's prefix list is reached through
/// (`VpcEndpoint$VpcEndpointType`).
const GATEWAY: &str = "Gateway";

/// The state a gateway endpoint is created in: it stands up no endpoint network interface, so
/// it is in service as soon as it exists (`VpcEndpoint$State`).
const ENDPOINT_AVAILABLE: &str = "available";

/// The transient status a replication instance is created in, and the status the clock then
/// owes it (`ReplicationInstance$ReplicationInstanceStatus`).
const INSTANCE_CREATING: &str = "creating";
const INSTANCE_AVAILABLE: &str = "available";

/// The status a replication subnet group holding resolved subnets reports
/// (`ReplicationSubnetGroup$SubnetGroupStatus`), and the status each of its subnets does
/// (`Subnet$SubnetStatus`).
const SUBNET_GROUP_COMPLETE: &str = "Complete";
const SUBNET_ACTIVE: &str = "Active";

/// The target a route table's route to its own VPC carries (`Route$GatewayId`), which is the
/// route every table comes with rather than the endpoint's.
const LOCAL_TARGET: &str = "local";

/// How long one poll of a clock-owed transition waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a clock-owed transition before giving up — far past every
/// span this emulator gives one, so a loaded machine does not fail a row the clock settles.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// The two services this rule spans, over ONE engine, as the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_databasemigrationservice_amazondmsv20160101::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request to one of the two services; the signature itself is never checked,
/// because this router runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One request parameter of an ec2Query form body.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One ec2Query list, written as the `Name.1`, `Name.2`, … parameters a client sends.
fn indexed(name: &str, values: &[String]) -> Vec<(String, String)> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| param(&format!("{name}.{}", index + 1), value))
        .collect()
}

/// Dispatch one request as it arrives on the wire, answering its status and its body text.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> (u16, String) {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One ec2Query action, answered with its HTTP status and its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        &format!("ec2.{REGION}.amazonaws.com"),
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("ec2")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One ec2Query action that has to be served, answered with its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = ec2(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon EC2 {action}, got {status} with body {body}"
    );
    body
}

/// The text of one element of an answer, or a panic naming the case and quoting the answer.
///
/// `crate::query_wire::element` reads the same element; this wrapper exists only to carry the
/// ROW into the panic, because a table-driven suite whose failure does not name its case
/// cannot be acted on. The reading itself is the shared one.
fn member(case: &str, body: &str, name: &str) -> String {
    opt_element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> element in {body}"))
}

/// One DMS call, as an awsJson1_1 client makes it, answered with its status and body text.
async fn dms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, String) {
    let target = format!("{DMS_TARGET_PREFIX}.{operation}");
    dispatch(
        router,
        &format!("dms.{REGION}.amazonaws.com"),
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization("dms")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One DMS call that has to be served, answered with its body as JSON.
async fn dms_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = dms(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DMS {operation}, got {status} with body {text}"
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|_| panic!("case {case:?}: {operation} answered a non-JSON body {text}"))
}

/// The string at `pointer` of a JSON answer, or a panic naming the case and quoting it.
fn text(case: &str, value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no string at {pointer} in {value}"))
        .to_string()
}

/// The subnet ids a reported `ReplicationSubnetGroup` holds (`Subnet$SubnetIdentifier`).
fn group_subnets(group: &Value) -> BTreeSet<String> {
    group
        .get("Subnets")
        .and_then(Value::as_array)
        .map(|subnets| {
            subnets
                .iter()
                .filter_map(|subnet| subnet.get("SubnetIdentifier").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The Availability Zones a reported `ReplicationSubnetGroup` places its subnets in
/// (`Subnet$SubnetAvailabilityZone`).
fn group_zones(group: &Value) -> BTreeSet<String> {
    group
        .get("Subnets")
        .and_then(Value::as_array)
        .map(|subnets| {
            subnets
                .iter()
                .filter_map(|subnet| {
                    subnet
                        .pointer("/SubnetAvailabilityZone/Name")
                        .and_then(Value::as_str)
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The statuses a reported `ReplicationSubnetGroup` gives its subnets.
fn group_subnet_statuses(group: &Value) -> BTreeSet<String> {
    group
        .get("Subnets")
        .and_then(Value::as_array)
        .map(|subnets| {
            subnets
                .iter()
                .filter_map(|subnet| subnet.get("SubnetStatus").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One estate: a VPC of the Region, a gateway endpoint to one of its services associated with
/// some of the VPC's route tables, and the DMS resources placed in the same network.
struct EstateCase {
    case: &'static str,
    /// The second octet of the VPC's `/16`, so each row's network is its own.
    octet: u8,
    /// The service the gateway endpoint is created to, named by the last label of its
    /// endpoint service name — the Region publishes a prefix list for each, which the row
    /// resolves through `DescribePrefixLists` rather than assuming.
    service_label: &'static str,
    /// Whether the create associates the implicit main route table the VPC comes with.
    associate_main: bool,
    /// How many route tables the row creates beside the main one and associates as well.
    created_tables: usize,
    /// How many Availability Zones the estate's subnets spread over — one subnet per zone.
    zones: usize,
    /// Whether the replication instance is a Multi-AZ deployment, which places a standby in a
    /// second zone and forbids naming a zone outright.
    multi_az: bool,
}

/// The rows: both services the Region serves gateway endpoints for, every combination of the
/// implicit main route table and tables created beside it (including none at all), two and
/// three zones of subnets, and a single-AZ and a Multi-AZ instance.
fn estate_cases() -> Vec<EstateCase> {
    vec![
        EstateCase {
            case: "Amazon S3 on the VPC's own main route table",
            octet: 31,
            service_label: "s3",
            associate_main: true,
            created_tables: 0,
            zones: 2,
            multi_az: false,
        },
        EstateCase {
            case: "Amazon S3 on the main route table and two tables created beside it",
            octet: 32,
            service_label: "s3",
            associate_main: true,
            created_tables: 2,
            zones: 3,
            multi_az: true,
        },
        EstateCase {
            case: "Amazon DynamoDB on a created table, leaving the main table unassociated",
            octet: 33,
            service_label: "dynamodb",
            associate_main: false,
            created_tables: 1,
            zones: 2,
            multi_az: false,
        },
        EstateCase {
            case: "Amazon DynamoDB with no route table associated at all",
            octet: 34,
            service_label: "dynamodb",
            associate_main: false,
            created_tables: 0,
            zones: 2,
            multi_az: false,
        },
    ]
}

/// The id of the route table a VPC comes with, read through the documented
/// `association.main` filter rather than guessed.
async fn main_route_table(router: &MultiServiceRouter, case: &str, vpc_id: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "DescribeRouteTables",
        &[
            param("Filter.1.Name", "vpc-id"),
            param("Filter.1.Value.1", vpc_id),
            param("Filter.2.Name", "association.main"),
            param("Filter.2.Value.1", "true"),
        ],
    )
    .await;
    // The answer names the table twice — once as the table and once in the association that
    // makes it the main one — so the DISTINCT ids are what says how many tables matched.
    let tables: BTreeSet<String> = elements(&body, "routeTableId").into_iter().collect();
    assert_eq!(
        tables.len(),
        1,
        "case {case:?}: expected {vpc_id} to come with exactly one main route table, got \
         {tables:?} from {body}"
    );
    let networks = elements(&body, "vpcId");
    assert_eq!(
        networks,
        vec![vpc_id.to_string()],
        "case {case:?}: expected the filtered answer to report only {vpc_id}'s table, got \
         {networks:?} from {body}"
    );
    member(case, &body, "routeTableId")
}

/// The prefix list id the Region publishes for the endpoint service `service_name`, which is
/// the destination an endpoint's route carries.
async fn published_prefix_list(
    router: &MultiServiceRouter,
    case: &str,
    service_name: &str,
) -> String {
    let body = ec2_ok(
        router,
        case,
        "DescribePrefixLists",
        &[
            param("Filter.1.Name", "prefix-list-name"),
            param("Filter.1.Value.1", service_name),
        ],
    )
    .await;
    let names = elements(&body, "prefixListName");
    assert_eq!(
        names,
        vec![service_name.to_string()],
        "case {case:?}: expected {REGION} to publish a prefix list for {service_name}, got \
         {names:?} from {body}"
    );
    member(case, &body, "prefixListId")
}

/// The routes one route table reports, each as the `(destinationPrefixListId, gatewayId)` pair
/// a client reads a gateway route by.
async fn table_routes(
    router: &MultiServiceRouter,
    case: &str,
    route_table_id: &str,
) -> Vec<(Option<String>, Option<String>)> {
    let body = ec2_ok(
        router,
        case,
        "DescribeRouteTables",
        &[param("RouteTableId.1", route_table_id)],
    )
    .await;
    let routes = block_of(&body, "routeSet").unwrap_or_default();
    items_of(routes)
        .into_iter()
        .map(|route| {
            (
                opt_element(route, "destinationPrefixListId"),
                opt_element(route, "gatewayId"),
            )
        })
        .collect()
}

/// Poll `DescribeReplicationInstances` until the instance `arn` reports `available`, answering
/// the instance as that reader reports it. The move out of the transient status is owed by the
/// clock, so this waits for the CONDITION under a generous deadline.
async fn settled_instance(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let described = dms_ok(
            router,
            case,
            "DescribeReplicationInstances",
            &json!({ "Filters": [{ "Name": "replication-instance-arn", "Values": [arn] }] }),
        )
        .await;
        let status = text(case, &described, "/ReplicationInstances/0/ReplicationInstanceStatus");
        if status == INSTANCE_AVAILABLE {
            return described
                .pointer("/ReplicationInstances/0")
                .cloned()
                .unwrap_or(Value::Null);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the replication instance {arn} never reached \
             {INSTANCE_AVAILABLE:?}, last status {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The whole estate, row by row: the gateway endpoint is created and associated with exactly
/// the tables the request named, each of those tables routes to it through the service's
/// prefix list, and the DMS resources placed in the same VPC resolve against it.
#[tokio::test]
async fn a_gateway_endpoint_carries_the_private_migration_estate_it_was_created_for() {
    for row in estate_cases() {
        let case = row.case;
        // One emulator per row, so each row's network, endpoint and DMS resources are its
        // own and no row can be served by what another wrote.
        let router = emulator();

        // The Region's zones come from the service that owns them, so the estate spreads over
        // zones a client could really place a subnet in.
        let zones = {
            let body = ec2_ok(&router, case, "DescribeAvailabilityZones", &[]).await;
            let reported = elements(&body, "zoneName");
            assert!(
                reported.len() >= row.zones,
                "case {case:?}: expected Amazon EC2 to report at least {} zones for {REGION}, \
                 got {reported:?}",
                row.zones
            );
            reported[..row.zones].to_vec()
        };

        // The network: a VPC, one subnet per zone, and the tables the row associates — the
        // implicit main table it came with, plus however many it creates beside it.
        let cidr = format!("10.{}.0.0/16", row.octet);
        let vpc_id = {
            let body = ec2_ok(&router, case, "CreateVpc", &[param("CidrBlock", &cidr)]).await;
            member(case, &body, "vpcId")
        };
        let main_table = main_route_table(&router, case, &vpc_id).await;
        let mut subnets: Vec<String> = Vec::new();
        for (index, zone) in zones.iter().enumerate() {
            let subnet_cidr = format!("10.{}.{}.0/24", row.octet, index + 1);
            let body = ec2_ok(
                &router,
                case,
                "CreateSubnet",
                &[
                    param("VpcId", &vpc_id),
                    param("CidrBlock", &subnet_cidr),
                    param("AvailabilityZone", zone),
                ],
            )
            .await;
            subnets.push(member(case, &body, "subnetId"));
        }
        let mut associated: Vec<String> = Vec::new();
        if row.associate_main {
            associated.push(main_table.clone());
        }
        for _ in 0..row.created_tables {
            let body = ec2_ok(
                &router,
                case,
                "CreateRouteTable",
                &[param("VpcId", &vpc_id)],
            )
            .await;
            associated.push(member(case, &body, "routeTableId"));
        }

        // The call the whole estate is built on: a gateway endpoint to a service of the
        // Region, naming the route tables it is reached from. It is SERVED — the operation
        // models no error, so there is no refusal class for this request shape.
        let service_name = format!("com.amazonaws.{REGION}.{}", row.service_label);
        let prefix_list_id = published_prefix_list(&router, case, &service_name).await;
        let mut params = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", GATEWAY),
        ];
        params.extend(indexed("RouteTableId", &associated));
        let (status, created) = ec2(&router, "CreateVpcEndpoint", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateVpcEndpoint naming {} route \
             table(s), got {status} with body {created}",
            associated.len()
        );
        let endpoint_id = member(case, &created, "vpcEndpointId");
        for (name, expected) in [
            ("vpcEndpointType", GATEWAY),
            ("state", ENDPOINT_AVAILABLE),
            ("serviceName", service_name.as_str()),
            ("vpcId", vpc_id.as_str()),
        ] {
            let answered = member(case, &created, name);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the endpoint's <{name}> to be {expected:?}, got \
                 {answered:?} in {created}"
            );
        }
        let answered_tables: Vec<String> = block_of(&created, "routeTableIdSet")
            .map(|set| items_of(set).into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        assert_eq!(
            answered_tables, associated,
            "case {case:?}: expected the create to report exactly the tables it named, got \
             {created}"
        );

        // The endpoint the describe reports is the one the create answered, associations
        // included: the record was persisted rather than composed for the create's reply.
        let described = ec2_ok(
            &router,
            case,
            "DescribeVpcEndpoints",
            &[param("VpcEndpointId.1", &endpoint_id)],
        )
        .await;
        let described_tables: Vec<String> = block_of(&described, "routeTableIdSet")
            .map(|set| items_of(set).into_iter().map(str::to_string).collect())
            .unwrap_or_default();
        assert_eq!(
            described_tables, associated,
            "case {case:?}: expected DescribeVpcEndpoints to report the endpoint's tables as \
             {associated:?}, got {described}"
        );
        let described_state = member(case, &described, "state");
        assert_eq!(
            described_state, ENDPOINT_AVAILABLE,
            "case {case:?}: expected DescribeVpcEndpoints to report {ENDPOINT_AVAILABLE:?}, \
             got {described_state:?}"
        );

        // Association is REACHABILITY: every table the create named routes to the endpoint
        // through the service's own prefix list, and keeps the local route of its VPC; a
        // table the create did not name routes to the endpoint from nowhere.
        for table in &associated {
            let routes = table_routes(&router, case, table).await;
            assert!(
                routes.contains(&(
                    Some(prefix_list_id.clone()),
                    Some(endpoint_id.clone())
                )),
                "case {case:?}: expected {table} to route {prefix_list_id} to {endpoint_id}, \
                 got {routes:?}"
            );
            assert!(
                routes
                    .iter()
                    .any(|(_, gateway)| gateway.as_deref() == Some(LOCAL_TARGET)),
                "case {case:?}: expected {table} to keep the local route of its VPC, got \
                 {routes:?}"
            );
        }
        if !row.associate_main {
            let routes = table_routes(&router, case, &main_table).await;
            assert!(
                routes
                    .iter()
                    .all(|(_, gateway)| gateway.as_deref() != Some(endpoint_id.as_str())),
                "case {case:?}: expected the unassociated main table {main_table} to hold no \
                 route to {endpoint_id}, got {routes:?}"
            );
        }

        // The DMS half of the estate, in the same VPC: the subnet group resolves the subnets
        // against the service that owns them, so it reports EC2's VPC and EC2's zones.
        let group_name = format!("dms-estate-{}", row.octet);
        let created_group = dms_ok(
            &router,
            case,
            "CreateReplicationSubnetGroup",
            &json!({
                "ReplicationSubnetGroupIdentifier": group_name,
                "ReplicationSubnetGroupDescription": "subnets reaching the gateway endpoint",
                "SubnetIds": subnets,
            }),
        )
        .await;
        let group = created_group
            .pointer("/ReplicationSubnetGroup")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            text(case, &group, "/VpcId"),
            vpc_id,
            "case {case:?}: expected the subnet group to report the VPC its subnets sit in, \
             got {group}"
        );
        assert_eq!(
            group_subnets(&group),
            subnets.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: expected the subnet group to hold exactly the subnets it was \
             created over, got {group}"
        );
        assert_eq!(
            group_zones(&group),
            zones.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: expected the subnet group to report the zones Amazon EC2 placed \
             its subnets in, got {group}"
        );
        assert_eq!(
            text(case, &group, "/SubnetGroupStatus"),
            SUBNET_GROUP_COMPLETE,
            "case {case:?}: expected a group over resolved subnets to be \
             {SUBNET_GROUP_COMPLETE:?}, got {group}"
        );
        assert_eq!(
            group_subnet_statuses(&group),
            BTreeSet::from([SUBNET_ACTIVE.to_string()]),
            "case {case:?}: expected every subnet of the group to be {SUBNET_ACTIVE:?}, got \
             {group}"
        );

        // Read back through DMS's own reader: the group was persisted, not composed for the
        // create's reply.
        let listed = dms_ok(
            &router,
            case,
            "DescribeReplicationSubnetGroups",
            &json!({
                "Filters": [{
                    "Name": "replication-subnet-group-id",
                    "Values": [group_name],
                }],
            }),
        )
        .await;
        let reread = listed
            .pointer("/ReplicationSubnetGroups/0")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reread, group,
            "case {case:?}: expected the describe to report the group the create answered, \
             got {listed}"
        );

        // The replication instance placed in that group: created in a transient status the
        // clock owes `available`, and reporting the group — and so the VPC and subnets — it
        // was placed in.
        let instance_name = format!("dms-instance-{}", row.octet);
        let mut request = json!({
            "ReplicationInstanceIdentifier": instance_name,
            "ReplicationInstanceClass": "dms.t3.medium",
            "ReplicationSubnetGroupIdentifier": group_name,
            "MultiAZ": row.multi_az,
        });
        // "You can't set the AvailabilityZone parameter if the Multi-AZ parameter is set to
        // true" (`CreateReplicationInstance$MultiAZ`), so only a single-AZ row names one.
        if !row.multi_az {
            request["AvailabilityZone"] = json!(zones[0]);
        }
        let created_instance =
            dms_ok(&router, case, "CreateReplicationInstance", &request).await;
        let instance_arn = text(
            case,
            &created_instance,
            "/ReplicationInstance/ReplicationInstanceArn",
        );
        let answered_status = text(
            case,
            &created_instance,
            "/ReplicationInstance/ReplicationInstanceStatus",
        );
        assert_eq!(
            answered_status, INSTANCE_CREATING,
            "case {case:?}: expected the create to answer the transient \
             {INSTANCE_CREATING:?}, got {answered_status:?} in {created_instance}"
        );

        let instance = settled_instance(&router, case, &instance_arn).await;
        let placed = instance
            .pointer("/ReplicationSubnetGroup")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            placed, group,
            "case {case:?}: expected the settled instance to report the group it was placed \
             in, got {instance}"
        );
        if row.multi_az {
            // A Multi-AZ deployment keeps a standby in a second zone of the Region, so the
            // two zones it reports are different ones.
            let primary = text(case, &instance, "/AvailabilityZone");
            let secondary = text(case, &instance, "/SecondaryAvailabilityZone");
            assert_ne!(
                primary, secondary,
                "case {case:?}: expected a Multi-AZ instance to stand its standby in another \
                 zone, got {instance}"
            );
        } else {
            let primary = text(case, &instance, "/AvailabilityZone");
            assert_eq!(
                primary, zones[0],
                "case {case:?}: expected the instance to sit in the zone the create named, \
                 got {instance}"
            );
            assert!(
                instance.get("SecondaryAvailabilityZone").is_none(),
                "case {case:?}: expected a single-AZ instance to report no standby zone, got \
                 {instance}"
            );
        }
    }
}
