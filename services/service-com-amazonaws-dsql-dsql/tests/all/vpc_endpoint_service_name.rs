// SPDX-License-Identifier: CC0-1.0

//! `GetVpcEndpointServiceName` over the wire.
//!
//! The RULE under test: the operation reports the VPC endpoint service that fronts the
//! PERSISTED cluster the `@httpLabel` `identifier` names — `com.amazonaws.{region}.dsql-`
//! qualified by the SHORT endpoint-service suffix that cluster was minted with, in that
//! cluster's region — together with that cluster's VPC CONNECTION endpoint, which is that
//! cluster's OWN host: its identifier as the leading label, the `dsql-` label carrying the
//! same suffix the service name carries, the cluster's region, and the zone this emulator
//! announces its own names in. A cluster this service does not hold is refused with the
//! modelled `ResourceNotFoundException` (404).
//!
//! Every case drives `MultiServiceRouter::dispatch` with a signed-looking restJson1
//! request, so the route, the `@httpLabel` binding, the boundary constraint check and
//! the handler all run the way a client runs them. The tables span the class the rule
//! generalises over: several regions, several clusters per region, every cluster status
//! the clock drives a cluster through, and the whole class of malformed identifiers.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::harness::{absent_identifier, wait_for_the_clock, Answer, Dsql};

/// `ServiceName`'s published bounds. Source: the service's own model,
/// `smithy_query(query="shape", shape="ServiceName")` — `@length(min: 1, max: 128)`
/// and `@pattern("^com\\.amazonaws\\.[a-z0-9-]+\\.dsql[a-z0-9-]*$")`.
const SERVICE_NAME_MIN_LEN: usize = 1;
const SERVICE_NAME_MAX_LEN: usize = 128;

/// Whether `value` satisfies `ServiceName`'s pattern, written out by hand from the
/// pattern above: the literal `com.amazonaws.`, a `[a-z0-9-]+` region, a `.`, the
/// literal `dsql`, then a `[a-z0-9-]*` qualifier.
fn is_service_name(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("com.amazonaws.") else {
        return false;
    };
    let Some((region, tail)) = rest.split_once('.') else {
        return false;
    };
    let region_ok = !region.is_empty()
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let Some(qualifier) = tail.strip_prefix("dsql") else {
        return false;
    };
    let qualifier_ok = qualifier
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    region_ok
        && qualifier_ok
        && (SERVICE_NAME_MIN_LEN..=SERVICE_NAME_MAX_LEN).contains(&value.len())
}

/// Whether `value` satisfies `ClusterVpcEndpoint`'s `^[a-zA-Z0-9.-]+$`. Source:
/// `smithy_query(query="shape", shape="ClusterVpcEndpoint")`.
fn is_cluster_vpc_endpoint(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

impl Dsql {
    /// `GET /clusters/{identifier}/vpc-endpoint-service-name`, the route the model binds
    /// this operation to (`smithy_query(query="operation",
    /// operation="GetVpcEndpointServiceName")`).
    async fn vpc_endpoint_service_name(&self, identifier: &str) -> Answer {
        self.call(
            "GET",
            &format!("/clusters/{identifier}/vpc-endpoint-service-name"),
            None,
        )
        .await
    }
}

/// The regions the rule is exercised in: the ids differ in length and in the number of
/// hyphens, and one is a non-commercial-looking form, so the region segment is read
/// from the cluster rather than assumed.
const REGIONS: &[&str] = &["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"];

/// The zone this emulator announces its own names in when the harness drives it in process
/// — the loopback zone, which RFC 6761 reserves, with every name under it, for the loopback
/// interface the core binds its listeners on. A name this service announces therefore ends
/// in it; `cluster_lifecycle` spans the other bases the emulator can be served at.
const ANNOUNCED_ZONE: &str = "localhost";

/// How many characters the per-cluster endpoint-service suffix carries. Source: the
/// recorded service's own reply, whose `serviceName` carries a four-character token
/// after the `dsql-` label prefix — the endpoint service's own short name, never the
/// cluster's 26-character identifier. The suffix VALUE of any one reply is minted per
/// cluster and is not asserted anywhere below.
const SUFFIX_LENGTH: usize = 4;

/// The endpoint-service suffix a reply's `serviceName` carries: what follows its
/// `dsql-` label prefix.
fn reported_suffix(service_name: &str) -> String {
    service_name
        .rsplit_once(".dsql-")
        .map(|(_region, suffix)| suffix.to_string())
        .unwrap_or_default()
}

/// Whether `suffix` has the FORM the rule requires of a cluster's endpoint-service
/// suffix: [`SUFFIX_LENGTH`] characters of the lowercase alphanumerics `ServiceName`'s
/// `[a-z0-9-]*` and one DNS label both admit. That it is a name of the ENDPOINT SERVICE
/// and not of the cluster is asserted beside each use, against the reply itself.
fn is_endpoint_service_suffix(suffix: &str) -> bool {
    suffix.len() == SUFFIX_LENGTH
        && suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Whether the `clusterVpcEndpoint` of ONE reply is the VPC connection host of the cluster
/// `identifier` fronted by the endpoint service `suffix` in `region`: the cluster's own
/// labels under the zone this emulator announces in, and nothing of the address this process
/// happens to be reached at.
fn is_this_clusters_vpc_host(
    reported: &str,
    identifier: &str,
    suffix: &str,
    region: &str,
) -> bool {
    reported == format!("{identifier}.dsql-{suffix}.{region}.{ANNOUNCED_ZONE}")
}

/// RULE: for a persisted cluster in any region, the reported service name is
/// `com.amazonaws.{cluster region}.dsql-{that cluster's endpoint-service suffix}` and
/// the reported VPC connection endpoint is that cluster's own host — its identifier, the
/// same endpoint-service suffix, its region, and the zone this emulator announces in.
#[tokio::test]
async fn the_service_name_names_the_persisted_cluster_and_its_region() {
    for region in REGIONS {
        let dsql = Dsql::in_region(region);
        // Several clusters per region: the qualifier has to follow the cluster, not the
        // region alone.
        for case in ["first cluster", "second cluster", "third cluster"] {
            let identifier = dsql.create_settled().await;
            let answered = dsql.vpc_endpoint_service_name(&identifier).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?} in {region}: GetVpcEndpointServiceName must answer 200, got {} {:?}",
                answered.status, answered.body
            );

            let service_name = answered.str("serviceName");
            let suffix = reported_suffix(&service_name);
            let expected = format!("com.amazonaws.{region}.dsql-{suffix}");
            assert_eq!(
                service_name, expected,
                "case {case:?} in {region}: serviceName must be {expected:?}, got {service_name:?}"
            );
            assert!(
                is_endpoint_service_suffix(&suffix),
                "case {case:?} in {region}: serviceName must be qualified by a short \
                 per-cluster endpoint-service suffix of {SUFFIX_LENGTH} lowercase \
                 alphanumerics, got {suffix:?}"
            );
            assert!(
                !service_name.contains(&identifier),
                "case {case:?} in {region}: serviceName must name the endpoint service \
                 and not the cluster {identifier:?}, got {service_name:?}"
            );
            assert!(
                is_service_name(&service_name),
                "case {case:?} in {region}: serviceName must satisfy ServiceName's pattern \
                 and length, got {service_name:?}"
            );

            let reported = answered.str("clusterVpcEndpoint");
            // The round trip: the cluster the reply is about is the one the control
            // plane holds, and the VPC host it is fronted at names that cluster.
            let read_back = dsql.get(&identifier).await;
            assert!(
                is_this_clusters_vpc_host(&reported, &identifier, &suffix, region),
                "case {case:?} in {region}: clusterVpcEndpoint must be \
                 {identifier}.dsql-{suffix}.{region}.{ANNOUNCED_ZONE}, got {reported:?}"
            );
            assert!(
                is_cluster_vpc_endpoint(&reported),
                "case {case:?} in {region}: clusterVpcEndpoint must satisfy \
                 ^[a-zA-Z0-9.-]+$, got {reported:?}"
            );
            assert_eq!(
                read_back.str("identifier"),
                identifier,
                "case {case:?} in {region}: GetCluster must report the cluster the reply \
                 is about, got {:?}",
                read_back.str("identifier")
            );
            assert_ne!(
                reported,
                read_back.str("endpoint"),
                "case {case:?} in {region}: the VPC access path has a host of its own, \
                 not the cluster's data-plane endpoint {:?}",
                read_back.str("endpoint")
            );
        }
    }
}

/// RULE (invariant): both members are properties of the CLUSTER, so distinct clusters get
/// distinct names and distinct VPC hosts, while repeated reads of one cluster get the same
/// name and the same host.
#[tokio::test]
async fn names_are_stable_per_cluster_and_distinct_between_clusters() {
    let dsql = Dsql::new();
    let mut seen: Vec<(String, String, String)> = Vec::new();
    for _ in 0..4 {
        let identifier = dsql.create_settled().await;
        let first = dsql.vpc_endpoint_service_name(&identifier).await;
        let second = dsql.vpc_endpoint_service_name(&identifier).await;
        for member in ["serviceName", "clusterVpcEndpoint"] {
            assert_eq!(
                first.str(member),
                second.str(member),
                "case {identifier:?}: two reads of one cluster must report one {member}, got \
                 {:?} then {:?}",
                first.str(member),
                second.str(member)
            );
        }
        seen.push((
            identifier,
            first.str("serviceName"),
            first.str("clusterVpcEndpoint"),
        ));
    }

    for (left_index, (left_id, left_name, left_endpoint)) in seen.iter().enumerate() {
        for (right_id, right_name, right_endpoint) in seen.iter().skip(left_index + 1) {
            assert_ne!(
                left_name, right_name,
                "case {left_id:?} vs {right_id:?}: distinct clusters must get distinct \
                 service names, both got {left_name:?}"
            );
            assert_ne!(
                left_endpoint, right_endpoint,
                "case {left_id:?} vs {right_id:?}: distinct clusters must be fronted at \
                 distinct VPC hosts, both got {left_endpoint:?}"
            );
        }
    }
}

/// RULE: the region in the name is the CLUSTER's (the region of the ARN it was minted
/// with), not the region the reading request happens to be signed for.
#[tokio::test]
async fn the_region_is_the_clusters_not_the_readers() {
    for (home, reader) in [
        ("us-east-1", "eu-west-1"),
        ("eu-west-1", "ap-southeast-2"),
        ("ap-southeast-2", "us-east-1"),
    ] {
        // Two views of ONE store, signed for different regions — the production router
        // builds every service from a single engine, so both see the same clusters.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let at_home = Dsql::sharing(&engine, home);
        let elsewhere = Dsql::sharing(&engine, reader);

        let identifier = at_home.create_settled().await;
        let arn = at_home.get(&identifier).await.str("arn");
        assert!(
            arn.contains(&format!(":dsql:{home}:")),
            "case {home}->{reader}: the cluster ARN must carry its home region, got {arn:?}"
        );

        // The cluster's endpoint-service suffix is minted with the cluster, so the
        // expectation is read off the cluster's own region's reply once and then
        // required of the read signed for another region too.
        let suffix = reported_suffix(
            &at_home
                .vpc_endpoint_service_name(&identifier)
                .await
                .str("serviceName"),
        );
        let expected = format!("com.amazonaws.{home}.dsql-{suffix}");
        for (case, view) in [("the cluster's own region", &at_home), ("another region", &elsewhere)]
        {
            let answered = view.vpc_endpoint_service_name(&identifier).await;
            assert_eq!(
                answered.status, 200,
                "case {home}->{reader} read from {case}: must answer 200, got {} {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.str("serviceName"),
                expected,
                "case {home}->{reader} read from {case}: serviceName must stay in the \
                 cluster's region, expected {expected:?}, got {:?}",
                answered.str("serviceName")
            );
            // The VPC host follows the CLUSTER's Region too, not the Region either call is
            // signed for: both reads name the same host.
            let want = format!("{identifier}.dsql-{suffix}.{home}.{ANNOUNCED_ZONE}");
            assert_eq!(
                answered.str("clusterVpcEndpoint"),
                want,
                "case {home}->{reader} read from {case}: clusterVpcEndpoint must be {want:?}, \
                 got {:?}",
                answered.str("clusterVpcEndpoint")
            );
        }
    }
}

/// RULE: the name depends on the cluster's identity, not on the status the clock has it
/// in, so it reads the same while the cluster is still `CREATING` as once it is `ACTIVE`.
#[tokio::test]
async fn the_name_does_not_depend_on_the_clusters_status() {
    let dsql = Dsql::new();
    let created = dsql.create(json!({"deletionProtectionEnabled": false})).await;
    assert_eq!(
        created.status, 200,
        "CreateCluster must answer 200, got {} {:?}",
        created.status, created.body
    );
    let identifier = created.str("identifier");

    let while_creating = dsql.vpc_endpoint_service_name(&identifier).await;
    assert_eq!(
        while_creating.status, 200,
        "case CREATING: GetVpcEndpointServiceName must answer 200, got {} {:?}",
        while_creating.status, while_creating.body
    );
    assert_eq!(
        dsql.get(&identifier).await.str("status"),
        "CREATING",
        "case CREATING: the cluster must still be in its transient status"
    );

    wait_for_the_clock().await;
    let when_active = dsql.vpc_endpoint_service_name(&identifier).await;
    assert_eq!(
        dsql.get(&identifier).await.str("status"),
        "ACTIVE",
        "case ACTIVE: the clock must have settled the cluster"
    );
    assert_eq!(
        while_creating.str("serviceName"),
        when_active.str("serviceName"),
        "case CREATING vs ACTIVE: the name must not depend on the status, got {:?} then {:?}",
        while_creating.str("serviceName"),
        when_active.str("serviceName")
    );
}

/// RULE: a cluster this service does not hold is refused with the modelled
/// `ResourceNotFoundException` (404) — never a name made up from the request.
#[tokio::test]
async fn an_unheld_cluster_is_refused_with_resource_not_found() {
    let dsql = Dsql::new();
    let deleted = dsql.create_settled().await;
    let delete = dsql.delete(&deleted).await;
    assert_eq!(
        delete.status, 200,
        "case deleted: DeleteCluster must answer 200, got {} {:?}",
        delete.status, delete.body
    );
    wait_for_the_clock().await;

    // Every well-formed identifier the store does not hold: one never created, one the
    // clock has finished deleting.
    for (case, identifier) in [
        ("never created", absent_identifier()),
        ("deleted", deleted),
    ] {
        let answered = dsql.vpc_endpoint_service_name(&identifier).await;
        assert_eq!(
            answered.status, 404,
            "case {case:?}: must answer 404, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered
                .error
                .as_deref()
                .unwrap_or_default()
                .rsplit('#')
                .next()
                .unwrap_or_default(),
            "ResourceNotFoundException",
            "case {case:?}: must answer ResourceNotFoundException, got {:?}",
            answered.error
        );
        assert_eq!(
            answered.get("serviceName"),
            Value::Null,
            "case {case:?}: a refusal must report no service name, got {:?}",
            answered.get("serviceName")
        );
    }
}

/// RULE: `identifier` is constrained to `^[a-z0-9]{26}$`, so the whole class of
/// malformed identifiers is refused at the boundary with 400 `ValidationException`
/// rather than reaching the store.
#[tokio::test]
async fn a_malformed_identifier_is_refused_as_invalid() {
    let dsql = Dsql::new();
    for (case, identifier) in [
        ("too short", "abc".to_string()),
        ("one character short", "a".repeat(25)),
        ("one character long", "a".repeat(27)),
        ("uppercase", "A".repeat(26)),
        ("an underscore", format!("{}_", "a".repeat(25))),
    ] {
        let answered = dsql.vpc_endpoint_service_name(&identifier).await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: a malformed identifier must be refused with 400, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered
                .error
                .as_deref()
                .unwrap_or_default()
                .rsplit('#')
                .next()
                .unwrap_or_default(),
            "ValidationException",
            "case {case:?}: must answer ValidationException, got {:?}",
            answered.error
        );
    }
}

/// RULE: the endpoint service that fronts a cluster has a SHORT name of its OWN — a
/// per-cluster suffix minted with the cluster and stored on it — so the `serviceName` never
/// renders the cluster's identifier as the endpoint service, distinct clusters name
/// distinct endpoint services, and the `clusterVpcEndpoint` beside the name carries BOTH
/// labels: the cluster it fronts and that same endpoint-service suffix.
#[tokio::test]
async fn the_endpoint_service_is_named_by_a_short_per_cluster_suffix() {
    // The rows span the request forms a cluster is created from — the minimum, tags, a
    // cluster that refuses deletion, and a multi-Region configuration (which leaves the
    // cluster in a transient status) — because the endpoint service is a property of the
    // cluster itself and of nothing the creating request carried.
    let rows: &[(&str, Value)] = &[
        (
            "the minimum request",
            json!({"deletionProtectionEnabled": false}),
        ),
        (
            "tags supplied",
            json!({"deletionProtectionEnabled": false, "tags": {"env": "endpoint-service"}}),
        ),
        (
            "deletion protection on",
            json!({"deletionProtectionEnabled": true}),
        ),
        (
            "a witness region",
            json!({
                "deletionProtectionEnabled": false,
                "multiRegionProperties": {"witnessRegion": "us-west-2"}
            }),
        ),
    ];
    let region = "us-east-1";
    let dsql = Dsql::in_region(region);
    let mut named: Vec<(String, String)> = Vec::new();
    for (case, request) in rows {
        let created = dsql.create(request.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateCluster must answer 200, got {} {:?}",
            created.status, created.body
        );
        let identifier = created.str("identifier");
        let connection_endpoint = created.str("endpoint");

        let answered = dsql.vpc_endpoint_service_name(&identifier).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: GetVpcEndpointServiceName must answer 200, got {} {:?}",
            answered.status, answered.body
        );
        let service_name = answered.str("serviceName");
        let reported = answered.str("clusterVpcEndpoint");
        let suffix = reported_suffix(&service_name);

        assert!(
            is_endpoint_service_suffix(&suffix),
            "case {case:?}: the endpoint service must be named by a short per-cluster \
             suffix of {SUFFIX_LENGTH} lowercase alphanumerics, got {suffix:?} in \
             {service_name:?} for cluster {identifier:?}"
        );
        assert_eq!(
            service_name,
            format!("com.amazonaws.{region}.dsql-{suffix}"),
            "case {case:?}: serviceName must be the endpoint service of this cluster in \
             its own region, got {service_name:?}"
        );
        assert!(
            !service_name.contains(&identifier),
            "case {case:?}: serviceName must name the endpoint service and not the \
             cluster {identifier:?}, got {service_name:?}"
        );
        assert!(
            is_this_clusters_vpc_host(&reported, &identifier, &suffix, region),
            "case {case:?}: clusterVpcEndpoint must be \
             {identifier}.dsql-{suffix}.{region}.{ANNOUNCED_ZONE}, got {reported:?}"
        );
        assert_ne!(
            reported, connection_endpoint,
            "case {case:?}: the VPC access path has a host of its own, not the cluster's \
             data-plane endpoint {connection_endpoint:?}"
        );
        assert!(
            is_cluster_vpc_endpoint(&reported),
            "case {case:?}: clusterVpcEndpoint must satisfy ^[a-zA-Z0-9.-]+$, got \
             {reported:?}"
        );
        named.push((identifier, suffix));
    }

    for (index, (identifier, suffix)) in named.iter().enumerate() {
        for (other_identifier, other_suffix) in named.iter().skip(index + 1) {
            assert_ne!(
                suffix, other_suffix,
                "case {identifier:?} vs {other_identifier:?}: distinct clusters must name \
                 distinct endpoint services, both got {suffix:?}"
            );
        }
    }
}
