// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the documented request rules this service enforces BEFORE it
//! mints or changes anything: every one of them is a refusal the service answers a
//! request with, where the emulator used to answer 200 and store the configuration.
//!
//! One suite per rule, each a TABLE whose rows differ along what the rule generalises
//! over — the value, the member that carries it, the kind of resource an ARN names, the
//! count — rather than along the one request that showed the rule was missing:
//!
//! * a cluster placed in multiple Availability Zones runs at least one data node per zone
//!   ([`zone_awareness_requires_a_data_node_per_zone`]), and a cluster that runs dedicated
//!   master nodes runs a quorum the service configures
//!   ([`a_dedicated_master_quorum_outside_the_published_counts_is_refused`]);
//! * a domain runs an engine release the service OFFERS
//!   ([`an_engine_version_outside_the_published_catalog_is_refused`]);
//! * fine-grained access control names the ONE master user the configuration it asks for
//!   is served by ([`fine_grained_access_control_names_its_master_user`]);
//! * an access policy is a JSON document, whichever call carries it
//!   ([`an_access_policy_that_is_not_json_is_refused_by_both_write_paths`]);
//! * an ARN handed over as a data source names a KIND of resource this service attaches,
//!   held by the account the request was made for
//!   ([`an_attach_resolves_the_kind_and_the_owner_of_the_arn_it_is_given`]);
//! * a package's content is imported from a location that holds an object
//!   ([`a_package_source_that_holds_no_object_is_refused`]);
//! * a saved-object migration is started into an application the account still holds
//!   ([`a_migration_into_an_application_being_deleted_is_refused`]);
//! * an index's analysis settings define analyzers the cluster can BUILD
//!   ([`index_settings_whose_analyzer_cannot_be_built_are_refused`]).
//!
//! Every accepted row is read back through an implemented read — `DescribeDomain`,
//! `DescribePackages`, `GetIndex`, `GetMigration` — so a rule that admits a request is
//! shown to have stored it, and every refused row is shown to have stored NOTHING.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::analysis::MAX_ANALYSIS_DEFINITIONS;
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    published_packages, PACKAGE_TYPE_TXT_DICTIONARY, SUPPORTED_ENGINE_VERSIONS,
};

use super::application_deletion::delete_application;
use super::control_plane_refusal_wording::message;
use super::data_source_attachments::{application, attach};
use super::domain_lifecycle::{
    error_code, string_member, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS, REGION,
};
use super::index_registry::create as create_index;
use super::saved_object_migrations::{
    active_application, application_with_source, get_migration, start_migration,
};

/// The status a refused request is answered on, and the error code it carries: this
/// service answers every rule below with the modelled `ValidationException` (HTTP 400)
/// unless a row names another.
const REFUSED_STATUS: u16 = 400;
const VALIDATION: &str = "ValidationException";

/// Assert that `answer` is the refusal `code` on `status`, and name the case.
fn assert_refused(answer: &Answer, status: u16, code: &str, case: &str) {
    assert_eq!(
        (answer.status, error_code(answer).as_str()),
        (status, code),
        "case {case}: expected HTTP {status} {code}, got {} / {} with {:?}",
        answer.status,
        error_code(answer),
        answer.body
    );
}

/// Assert that `answer` was served, and name the case.
fn assert_served(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Whether the account holds a domain under `name` — the proof a refused create stored
/// nothing and an accepted one stored the domain.
async fn holds_domain(emulator: &Emulator, name: &str) -> bool {
    let answer = emulator.describe(name).await;
    if answer.status == 200 {
        return true;
    }
    assert_eq!(
        (answer.status, error_code(&answer).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case DescribeDomain({name:?}): expected either the domain or the modelled \
         not-found, got {} with {:?}",
        answer.status,
        answer.body
    );
    false
}

/// `CreateDomain` with `cluster` as its `ClusterConfig`, asserted to be accepted or
/// refused as `accepted` says, with the store checked for what it left behind.
async fn cluster_case(emulator: &Emulator, case: &str, name: &str, cluster: Value, accepted: bool) {
    let answer = emulator
        .create(json!({ "DomainName": name, "ClusterConfig": cluster }))
        .await;
    if accepted {
        assert_served(&answer, case);
    } else {
        assert_refused(&answer, REFUSED_STATUS, VALIDATION, case);
    }
    assert_eq!(
        holds_domain(emulator, name).await,
        accepted,
        "case {case}: an accepted create stores the domain and a refused one stores nothing"
    );
}

/// One cluster configuration a zone-awareness row asks for.
struct ZoneRow {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// `ClusterConfig$InstanceCount`: the number of data nodes.
    nodes: i64,
    /// `ClusterConfig$ZoneAwarenessEnabled`, absent where `None`.
    zone_awareness: Option<bool>,
    /// `ZoneAwarenessConfig$AvailabilityZoneCount`, absent where `None`.
    zones: Option<i64>,
    /// Whether the service provisions that cluster.
    accepted: bool,
}

/// RULE: a cluster whose `ZoneAwarenessEnabled` is on is placed in multiple Availability
/// Zones and its data nodes are spread EVENLY across them, so the number of zones it asks
/// for is one the service places a cluster in ("valid values are 2 and 3") and the number of
/// data nodes it runs divides by that zone count — a request outside either is refused and
/// no domain is created.
#[tokio::test]
async fn zone_awareness_requires_a_data_node_per_zone() {
    let emulator = Emulator::new();

    // The rows differ along the data-node count (counts that divide by the zone count and
    // counts that do not, at and above the count itself), the zone count the configuration
    // names (each published value of `ZoneAwarenessConfig$AvailabilityZoneCount` — "valid
    // values are 2 and 3" — one past each bound, and none at all) and whether zone
    // awareness is asked for at all.
    let row = |case, nodes, zone_awareness, zones, accepted| ZoneRow {
        case,
        nodes,
        zone_awareness,
        zones,
        accepted,
    };
    let rows = vec![
        row("one node, no zone awareness", 1, None, None, true),
        row("one node, zone awareness off", 1, Some(false), None, true),
        row("one node, zone awareness on", 1, Some(true), None, false),
        row("one node over two zones", 1, Some(true), Some(2), false),
        row("one node over three zones", 1, Some(true), Some(3), false),
        row("two nodes over two zones", 2, Some(true), Some(2), true),
        row("two nodes over three zones", 2, Some(true), Some(3), false),
        row("three nodes over two zones", 3, Some(true), Some(2), false),
        row("three nodes over three zones", 3, Some(true), Some(3), true),
        row("four nodes over two zones", 4, Some(true), Some(2), true),
        row("six nodes over three zones", 6, Some(true), Some(3), true),
        row("seven nodes over three zones", 7, Some(true), Some(3), false),
        // A request that names no zone count is placed in the fewest zones multiple zones
        // can mean, so it is judged against that count.
        row("two nodes, no zone count", 2, Some(true), None, true),
        row("three nodes, no zone count", 3, Some(true), None, false),
        // A zone count the service does not place a cluster in is refused as the
        // unsupported value it is, whatever the node count beside it
        // (`ZoneAwarenessConfig$AvailabilityZoneCount`: "valid values are 2 and 3").
        row("one node over one zone", 1, Some(true), Some(1), false),
        row("two nodes over one zone", 2, Some(true), Some(1), false),
        row("three nodes over four zones", 3, Some(true), Some(4), false),
        row("four nodes over four zones", 4, Some(true), Some(4), false),
        // The count configures no placement while zone awareness is OFF, so it is not
        // judged then.
        row("one zone with zone awareness off", 1, Some(false), Some(1), true),
        // A cluster with NO data node at all is refused whatever else it asks for: a count
        // below one configures no cluster.
        row("no data node at all", 0, None, None, false),
        row("no data node with zone awareness", 0, Some(true), Some(2), false),
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("zones-{index}");
        let mut cluster =
            json!({ "InstanceType": "t3.small.search", "InstanceCount": row.nodes });
        if let Some(enabled) = row.zone_awareness {
            cluster["ZoneAwarenessEnabled"] = json!(enabled);
        }
        if let Some(count) = row.zones {
            cluster["ZoneAwarenessConfig"] = json!({ "AvailabilityZoneCount": count });
        }
        cluster_case(&emulator, row.case, &name, cluster, row.accepted).await;
    }
}

/// RULE: `ClusterConfig$DedicatedMasterCount` is documented "this number must be greater
/// than 2 and not 4, otherwise you receive a validation exception", so a cluster that turns
/// dedicated master nodes on with any other count is refused and no domain is created.
#[tokio::test]
async fn a_dedicated_master_quorum_outside_the_published_counts_is_refused() {
    let emulator = Emulator::new();

    // Rows sweep the count across the published rule: below it, at each refused value, at
    // the floor and above the one value above the floor that is refused.
    let rows: Vec<(&str, Option<i64>, bool)> = vec![
        ("one master node", Some(1), false),
        ("two master nodes", Some(2), false),
        ("three master nodes", Some(3), true),
        ("four master nodes", Some(4), false),
        ("five master nodes", Some(5), true),
        ("six master nodes", Some(6), true),
        ("no count at all", None, true),
    ];

    for (index, (case, masters, accepted)) in rows.iter().enumerate() {
        let name = format!("masters-{index}");
        let mut cluster = json!({
            "InstanceType": "m5.large.search",
            "InstanceCount": 2,
            "DedicatedMasterEnabled": true,
            "DedicatedMasterType": "m5.large.search",
        });
        if let Some(count) = masters {
            cluster["DedicatedMasterCount"] = json!(count);
        }
        cluster_case(&emulator, case, &name, cluster, *accepted).await;
    }

    // The count configures no node while the role is OFF, so it is not judged then: the
    // same value that is refused above is accepted here.
    cluster_case(
        &emulator,
        "a refused count with the master role turned off",
        "masters-role-off",
        json!({
            "InstanceType": "m5.large.search",
            "InstanceCount": 2,
            "DedicatedMasterCount": 4,
        }),
        true,
    )
    .await;
}

/// RULE: a domain runs an engine release the service OFFERS, so an `EngineVersion` inside
/// the member's pattern that the published catalog (the set `ListVersions` advertises) does
/// not carry is refused and no domain is created.
#[tokio::test]
async fn an_engine_version_outside_the_published_catalog_is_refused() {
    let emulator = Emulator::new();

    // Accepted: releases taken from the catalog the service itself publishes — the oldest,
    // the newest and one of each engine line — so the row set follows the catalog rather
    // than naming versions of its own.
    let newest = SUPPORTED_ENGINE_VERSIONS[SUPPORTED_ENGINE_VERSIONS.len() - 1];
    let published = [
        SUPPORTED_ENGINE_VERSIONS[0],
        "Elasticsearch_7.10",
        "OpenSearch_1.0",
        newest,
    ];
    for (index, version) in published.iter().enumerate() {
        let name = format!("engine-ok-{index}");
        let answer = emulator
            .create(json!({ "DomainName": name, "EngineVersion": version }))
            .await;
        assert_served(&answer, &format!("CreateDomain({version})"));
        assert_eq!(
            string_member(&answer.body["DomainStatus"], "EngineVersion", version),
            *version,
            "case CreateDomain({version}): expected the domain to run the release it was \
             created on, got {:?}",
            answer.body["DomainStatus"]
        );
    }

    // Refused: values inside `VersionString`'s own pattern
    // (`^Elasticsearch_[0-9]{1}\.[0-9]{1,2}$|^OpenSearch_[0-9]{1,2}\.[0-9]{1,2}$`) that name
    // no release the catalog carries — a minor between two published ones, a major past
    // every published one, and one of each engine line.
    let unpublished = [
        "OpenSearch_99.99",
        "OpenSearch_2.12",
        "Elasticsearch_9.9",
        "Elasticsearch_1.4",
    ];
    for (index, version) in unpublished.iter().enumerate() {
        let name = format!("engine-no-{index}");
        let case = format!("CreateDomain({version})");
        let answer = emulator
            .create(json!({ "DomainName": name, "EngineVersion": version }))
            .await;
        assert_refused(&answer, REFUSED_STATUS, VALIDATION, &case);
        assert!(
            !holds_domain(&emulator, &name).await,
            "case {case}: a refused create stores no domain"
        );
        assert!(
            !SUPPORTED_ENGINE_VERSIONS.contains(version),
            "case {case}: the row must name a release the published catalog does not carry"
        );
    }
}

/// The three prerequisites of fine-grained access control, merged into `body` — the
/// containers a create has to carry for the master-user rule to be the thing under test.
fn with_prerequisites(mut body: Value) -> Value {
    body["EncryptionAtRestOptions"] = json!({ "Enabled": true });
    body["NodeToNodeEncryptionOptions"] = json!({ "Enabled": true });
    body["DomainEndpointOptions"] = json!({ "EnforceHTTPS": true });
    body
}

/// RULE: a domain reached through fine-grained access control has exactly ONE master user,
/// and which kind it is follows from `InternalUserDatabaseEnabled` — the internal
/// database's credentials when it is on (`MasterUserName` and `MasterUserPassword` are
/// documented "only specify if InternalUserDatabaseEnabled is true") and the ARN of an IAM
/// principal when it is off (`MasterUserARN` is documented "only specify if
/// InternalUserDatabaseEnabled is false"). A create that names neither, or the pair
/// belonging to the other kind, is refused and no domain is created.
#[tokio::test]
async fn fine_grained_access_control_names_its_master_user() {
    let emulator = Emulator::new();
    let arn = ::service_common::caller_principal::ambient_user_arn();
    let credentials = json!({ "MasterUserName": "adminuser", "MasterUserPassword": "Str0ng!Pass" });

    // Rows differ along which kind of master user the request asks for and which members it
    // carries for it: both kinds complete, each kind missing its own members, and each kind
    // carrying the other's.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "the internal database with its credentials",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": true,
                "MasterUserOptions": credentials,
            }),
            true,
        ),
        (
            "an IAM master user with the internal database off",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": false,
                "MasterUserOptions": { "MasterUserARN": arn },
            }),
            true,
        ),
        (
            "the internal database with no master user options at all",
            json!({ "Enabled": true, "InternalUserDatabaseEnabled": true }),
            false,
        ),
        (
            "the internal database with a user name but no password",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": true,
                "MasterUserOptions": { "MasterUserName": "adminuser" },
            }),
            false,
        ),
        (
            "the internal database named by an ARN instead",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": true,
                "MasterUserOptions": { "MasterUserARN": arn },
            }),
            false,
        ),
        (
            "no internal database and no master user ARN",
            json!({ "Enabled": true, "InternalUserDatabaseEnabled": false }),
            false,
        ),
        (
            "no internal database and no options at all",
            json!({ "Enabled": true }),
            false,
        ),
        (
            "no internal database but its credentials",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": false,
                "MasterUserOptions": credentials,
            }),
            false,
        ),
        // The boundary is OFF, so it needs no master user at all: the rule is about what
        // serves fine-grained access control, not about every create.
        (
            "fine-grained access control switched off",
            json!({ "Enabled": false }),
            true,
        ),
    ];

    for (index, (case, options, accepted)) in rows.iter().enumerate() {
        let name = format!("fgac-master-{index}");
        let body = with_prerequisites(json!({
            "DomainName": name,
            "AdvancedSecurityOptions": options,
        }));
        let answer = emulator.create(body).await;
        if *accepted {
            assert_served(&answer, case);
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, case);
        }
        assert_eq!(
            holds_domain(&emulator, &name).await,
            *accepted,
            "case {case}: an accepted create stores the domain and a refused one stores nothing"
        );
    }
}

/// RULE: an `AccessPolicies` document is an IAM policy document, so a body that is not JSON
/// denotes no policy at all and is refused — by the create that stores one and by the
/// update that replaces it alike, with nothing stored either way.
#[tokio::test]
async fn an_access_policy_that_is_not_json_is_refused_by_both_write_paths() {
    let emulator = Emulator::new();

    // Rows differ along the SHAPE of the document: policies the service holds (an object,
    // one with several statements), values that name no policy but are JSON all the same (a
    // bare string, a number), the values that name no policy at all (empty, blank), and
    // bodies that are not JSON (prose, a truncated object, a trailing-comma object).
    let open_policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Principal": "*", "Action": "es:*", "Resource": "*" }],
    })
    .to_string();
    let rows: Vec<(&str, String, bool)> = vec![
        ("a policy document", open_policy.clone(), true),
        ("a JSON array", "[]".to_string(), true),
        ("a JSON string", "\"not a policy but JSON\"".to_string(), true),
        ("a JSON number", "7".to_string(), true),
        ("an empty document", String::new(), true),
        ("a blank document", "   ".to_string(), true),
        ("prose", "not json at all".to_string(), false),
        ("a truncated object", "{\"Version\":".to_string(), false),
        (
            "an object with a trailing comma",
            "{\"Version\": \"2012-10-17\",}".to_string(),
            false,
        ),
    ];

    for (index, (case, document, accepted)) in rows.iter().enumerate() {
        let name = format!("policy-{index}");
        let created = emulator
            .create(json!({ "DomainName": name, "AccessPolicies": document }))
            .await;
        let create_case = format!("CreateDomain({case})");
        if *accepted {
            assert_served(&created, &create_case);
        } else {
            assert_refused(&created, REFUSED_STATUS, VALIDATION, &create_case);
        }
        assert_eq!(
            holds_domain(&emulator, &name).await,
            *accepted,
            "case {create_case}: an accepted create stores the domain and a refused one \
             stores nothing"
        );
    }

    // The UPDATE path applies the same rule: a domain created with a policy the service
    // holds keeps it when an update carrying an unparseable one is refused. What a row
    // asserts is the SENTENCE, because an accepted update may still be refused for the
    // change already in flight — a different rule, with a different sentence.
    let malformed = "This policy contains invalid Json";
    let name = "policy-update";
    assert_served(
        &emulator
            .create(json!({ "DomainName": name, "AccessPolicies": open_policy }))
            .await,
        "CreateDomain(the update row)",
    );
    for (case, document, accepted) in &rows {
        let update_case = format!("UpdateDomainConfig({case})");
        let answer = emulator
            .update_config(name, json!({ "AccessPolicies": document }))
            .await;
        if *accepted {
            assert_ne!(
                message(&answer),
                malformed.to_string(),
                "case {update_case}: expected the document to be admitted, got {:?}",
                answer.body
            );
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, &update_case);
            assert_eq!(
                message(&answer),
                malformed.to_string(),
                "case {update_case}: expected the sentence for a policy that is not JSON, \
                 got {:?}",
                answer.body
            );
        }
    }
    let described = emulator.describe(name).await;
    assert_eq!(
        described.body["DomainStatus"]["AccessPolicies"].as_str(),
        Some(
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "es:*",
                    "Resource": "*",
                }],
            })
            .to_string()
            .as_str()
        ),
        "case the refused updates: the domain keeps the policy it was created with, got {:?}",
        described.body["DomainStatus"]["AccessPolicies"]
    );
}

/// One `dataSourceArn` an attach row hands over.
struct ArnRow {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `dataSourceArn` the attach carries.
    arn: String,
    /// Whether the service attaches it.
    accepted: bool,
}

/// RULE: `AttachDataSource` takes the ARN of a data source this service can read — a domain
/// of this account, a direct query data source of it, this account's Glue Data Catalog or an
/// OpenSearch Serverless collection of it — held by the account the request was made for: an
/// ARN naming another KIND of resource, and one naming another ACCOUNT's resource, are the
/// malformed values they are. Whether the named resource exists is the attachment's own
/// asynchronous outcome, so an ARN of the right kind and owner is accepted whether or not it
/// resolves.
#[tokio::test]
async fn an_attach_resolves_the_kind_and_the_owner_of_the_arn_it_is_given() {
    let emulator = Emulator::new();
    let id = application(&emulator, "attach-rules").await;
    let account = ::engine::emulator_account_id();

    // Rows differ along the KIND of resource the ARN names (each kind this service
    // attaches, and resources of other services and of other types of the same services)
    // and along the ACCOUNT it names.
    let row = |case, arn: String, accepted| ArnRow { case, arn, accepted };
    let rows = vec![
        row(
            "a domain of this account",
            format!("arn:aws:es:{REGION}:{account}:domain/attach-rules-domain"),
            true,
        ),
        row(
            "a direct query data source of this account",
            format!("arn:aws:es:{REGION}:{account}:datasource/attach_rules_source"),
            true,
        ),
        row(
            "this account's Glue Data Catalog",
            format!("arn:aws:glue:{REGION}:{account}:catalog"),
            true,
        ),
        row(
            "an OpenSearch Serverless collection of this account",
            format!("arn:aws:aoss:{REGION}:{account}:collection/attachrulescollection"),
            true,
        ),
        row(
            "an OpenSearch Serverless collection of another account",
            format!("arn:aws:aoss:{REGION}:555555555555:collection/elsewherecollection"),
            false,
        ),
        row(
            "a domain of another account",
            format!("arn:aws:es:{REGION}:555555555555:domain/elsewhere"),
            false,
        ),
        row(
            "a Glue Data Catalog of another account",
            format!("arn:aws:glue:{REGION}:555555555555:catalog"),
            false,
        ),
        row(
            "an EC2 instance of this account",
            format!("arn:aws:ec2:{REGION}:{account}:instance/i-0123456789abcdef0"),
            false,
        ),
        row(
            "an Amazon S3 bucket",
            "arn:aws:s3:::a-bucket-that-is-no-data-source".to_string(),
            false,
        ),
        row(
            "another resource type of this service",
            format!("arn:aws:es:{REGION}:{account}:application/attach-rules-app"),
            false,
        ),
        row(
            "another resource type of the catalog's own service",
            format!("arn:aws:glue:{REGION}:{account}:database/attach-rules-db"),
            false,
        ),
    ];

    for ArnRow {
        case,
        arn,
        accepted,
    } in &rows
    {
        let answer = attach(&emulator, &id, json!({ "dataSourceArn": arn })).await;
        if *accepted {
            assert_served(&answer, case);
            assert_eq!(
                answer.body["dataSourceArn"].as_str(),
                Some(arn.as_str()),
                "case {case}: the attachment records the data source it was asked for, got {:?}",
                answer.body
            );
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, case);
        }
    }

    // The refused attaches recorded nothing: the application holds exactly the attachments
    // of the rows that were served.
    let listed = attachment_arns(&emulator, &id).await;
    let expected: Vec<String> = rows
        .iter()
        .filter(|row| row.accepted)
        .map(|row| row.arn.clone())
        .collect();
    assert_eq!(
        listed.len(),
        expected.len(),
        "case the attachment set: expected one record per served attach, got {listed:?}"
    );
    for arn in &expected {
        assert!(
            listed.contains(arn),
            "case the attachment set: expected it to hold {arn}, got {listed:?}"
        );
    }
}

/// The `dataSourceArn`s the application `id` holds attachments for.
async fn attachment_arns(emulator: &Emulator, id: &str) -> Vec<String> {
    let answer = emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/application/{id}/listDataSourceAttachments"),
            Some(json!({})),
        )
        .await;
    assert_served(&answer, "ListDataSourceAttachments");
    answer.body["attachments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["dataSourceArn"].as_str().map(str::to_owned))
        .collect()
}

/// RULE: a package's content is IMPORTED from the Amazon S3 location the request names, so
/// a location that holds no object is nothing to import from: the call is refused at submit
/// time and no package is left copying content that would never arrive.
#[tokio::test]
async fn a_package_source_that_holds_no_object_is_refused() {
    let emulator = Emulator::new();
    let bucket = "documented-rules-bucket";
    emulator.given_object(bucket, "held/one.txt", "alpha\n");
    emulator.given_object(bucket, "held/two.txt", "beta\n");

    // Rows differ along WHICH part of the location resolves to nothing: both parts do, the
    // key does not, the bucket does not, neither member is given at all, and one is empty.
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "a location that holds an object",
            json!({ "S3BucketName": bucket, "S3Key": "held/one.txt" }),
            true,
        ),
        (
            "another location of the same bucket that holds one",
            json!({ "S3BucketName": bucket, "S3Key": "held/two.txt" }),
            true,
        ),
        (
            "a key the bucket holds no object at",
            json!({ "S3BucketName": bucket, "S3Key": "held/absent.txt" }),
            false,
        ),
        (
            "a bucket this account does not hold",
            json!({ "S3BucketName": "documented-rules-absent", "S3Key": "held/one.txt" }),
            false,
        ),
        ("a location naming nothing", json!({}), false),
        (
            "a location naming only its bucket",
            json!({ "S3BucketName": bucket }),
            false,
        ),
        (
            "a location whose key is empty",
            json!({ "S3BucketName": bucket, "S3Key": "" }),
            false,
        ),
    ];

    let mut created: Vec<String> = Vec::new();
    for (index, (case, source, accepted)) in rows.iter().enumerate() {
        let name = format!("source-rules-{index}");
        let answer = emulator
            .call(
                "POST",
                &format!("{PREFIX}/packages"),
                Some(json!({
                    "PackageName": name,
                    "PackageType": PACKAGE_TYPE_TXT_DICTIONARY,
                    "PackageSource": source,
                })),
            )
            .await;
        if *accepted {
            assert_served(&answer, case);
            created.push(string_member(
                &answer.body["PackageDetails"],
                "PackageID",
                case,
            ));
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, case);
        }
    }

    // The refused creates minted nothing: beside the packages the SERVICE publishes, the
    // account holds exactly the ones the served rows created, read back through the listing.
    let listed = emulator
        .call(
            "POST",
            &format!("{PREFIX}/packages/describe"),
            Some(json!({ "MaxResults": 100 })),
        )
        .await;
    assert_served(&listed, "DescribePackages");
    let ids: Vec<String> = listed.body["PackageDetailsList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|details| details["PackageID"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        ids.len(),
        created.len() + published_packages().len(),
        "case the package set: expected one package per served create, got {ids:?}"
    );
    for id in &created {
        assert!(
            ids.contains(id),
            "case the package set: expected it to hold {id}, got {ids:?}"
        );
    }
}

/// RULE: a saved-object migration is started INTO an application's workspace, so an
/// application the account no longer holds — one whose deletion has been asked for among
/// them — is nothing to migrate into, and the start is refused rather than minting a job
/// that would be taken away with the application.
#[tokio::test]
async fn a_migration_into_an_application_being_deleted_is_refused() {
    let emulator = Emulator::new();
    let source = format!(
        "arn:aws:aoss:{REGION}:{}:collection/documentedrules",
        ::engine::emulator_account_id()
    );
    let options = json!({
        "source": { "datasourceArn": source },
        "workspace": { "workspaceId": "ws-documented-rules" },
    });

    // A start into an application the account holds, and which is ACTIVE, is served, and
    // the job reads back.
    let held = application_with_source(&emulator, "migration-held", &source).await;
    let started = start_migration(
        &emulator,
        json!({ "applicationId": held, "migrationOptions": options }),
    )
    .await;
    assert_served(&started, "StartMigration(a held application)");
    let migration_id = string_member(&started.body, "migrationId", "a held application");
    let read = get_migration(&emulator, &migration_id).await;
    assert_served(&read, "GetMigration(the started job)");
    assert_eq!(
        read.body["applicationId"].as_str(),
        Some(held.as_str()),
        "case the started job: expected it to report the application it migrates into, got {:?}",
        read.body
    );

    // Rows differ along WHY the id names no application the account holds to migrate into:
    // never minted, of the shape the model admits but unused, one whose deletion has been
    // asked for, and one still being provisioned — an application that is not ACTIVE is not
    // yet one a job can be started into.
    let deleted = active_application(&emulator, "migration-deleted").await;
    assert_served(
        &delete_application(&emulator, &deleted).await,
        "DeleteApplication(the row being deleted)",
    );
    let provisioning = application(&emulator, "migration-provisioning").await;
    let rows: Vec<(&str, String)> = vec![
        ("an id no create ever minted", "nosuchapplication".to_string()),
        ("the shortest id the model admits", "abc".to_string()),
        ("an application being deleted", deleted.clone()),
        ("an application still being provisioned", provisioning.clone()),
    ];

    for (case, application_id) in &rows {
        let answer = start_migration(
            &emulator,
            json!({ "applicationId": application_id, "migrationOptions": options }),
        )
        .await;
        assert_refused(
            &answer,
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            case,
        );
    }
}

/// RULE: the settings are judged by ASSEMBLING every chain they define, so how many
/// components one document may define is bounded: a document past that bound is refused and
/// no index is created, while one at it is served.
#[tokio::test]
async fn index_settings_past_the_bound_on_what_they_define_are_refused() {
    let emulator = Emulator::new();
    let domain = "analyzer-bound";
    assert_served(
        &emulator.create(json!({ "DomainName": domain })).await,
        "CreateDomain(the definition bound)",
    );

    // One row at the bound and one past it, so what the rows differ along is the count the
    // bound is about rather than the shape of any definition.
    for (case, defined, accepted) in [
        ("at the bound", MAX_ANALYSIS_DEFINITIONS, true),
        ("one past the bound", MAX_ANALYSIS_DEFINITIONS + 1, false),
    ] {
        let index_name = format!("bound-{defined}");
        let mut analyzers = serde_json::Map::new();
        for ordinal in 0..defined {
            analyzers.insert(
                format!("probe{ordinal}"),
                json!({ "type": "custom", "tokenizer": "standard", "filter": ["lowercase"] }),
            );
        }
        let answer = create_index(
            &emulator,
            domain,
            json!({
                "IndexName": index_name,
                "IndexSchema": { "settings": { "analysis": { "analyzer": analyzers } } },
            }),
        )
        .await;
        let read = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/domain/{domain}/index/{index_name}"),
                None,
            )
            .await;
        if accepted {
            assert_served(&answer, case);
            assert_served(&read, &format!("GetIndex({case})"));
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, case);
            assert_eq!(
                read.status, RECORDED_NOT_FOUND_STATUS,
                "case {case}: a refused create stores no index, got {} with {:?}",
                read.status, read.body
            );
        }
    }
}

struct AnalyzerRow {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The analyzer the settings define under the name the mappings refer to.
    definition: Value,
    /// The character filters, tokenizers and token filters the settings DEFINE beside it,
    /// merged into the analysis section.
    defined: Value,
    /// Whether the cluster can build the chain, so the index is created.
    accepted: bool,
}

/// RULE: an index's analysis settings are what the cluster builds its analysis FROM, so a
/// defined analyzer naming an analyzer type, a tokenizer, a token filter or a character
/// filter nothing registers cannot be built: the create is refused with the engine's own
/// sentence and no index is created. A name the settings themselves DEFINE resolves through
/// that definition, so a chain composed of the index's own components is created.
#[tokio::test]
async fn index_settings_whose_analyzer_cannot_be_built_are_refused() {
    let emulator = Emulator::new();
    let domain = "analyzer-rules";
    assert_served(
        &emulator.create(json!({ "DomainName": domain })).await,
        "CreateDomain(the analyzer rows)",
    );

    // Rows differ along WHICH component of the chain nothing registers — the analyzer type
    // itself, the tokenizer, a token filter, a character filter — and along the FORM each is
    // named in: by name, configured inline, and by a name the settings define themselves.
    // Each component is swept on both sides of the rule.
    let row = |case, definition, defined, accepted| AnalyzerRow {
        case,
        definition,
        defined,
        accepted,
    };
    let rows = vec![
        row(
            "a custom analyzer of components the engine ships",
            json!({ "type": "custom", "tokenizer": "standard", "filter": ["lowercase"] }),
            json!({}),
            true,
        ),
        row(
            "a built-in analyzer named by type",
            json!({ "type": "english" }),
            json!({}),
            true,
        ),
        row(
            "a custom analyzer with no tokenizer at all",
            json!({ "type": "custom", "filter": ["trim"] }),
            json!({}),
            true,
        ),
        row(
            "a built-in character filter named by name",
            json!({ "type": "custom", "tokenizer": "standard", "char_filter": ["html_strip"] }),
            json!({}),
            true,
        ),
        row(
            "a character filter configured inline",
            json!({
                "type": "custom",
                "tokenizer": "standard",
                "char_filter": [{ "type": "mapping", "mappings": ["& => and"] }],
            }),
            json!({}),
            true,
        ),
        row(
            "a character filter the settings themselves define",
            json!({ "type": "custom", "tokenizer": "standard", "char_filter": ["my_map"] }),
            json!({ "char_filter": { "my_map": { "type": "mapping", "mappings": ["& => and"] } } }),
            true,
        ),
        row(
            "a tokenizer and a token filter the settings themselves define",
            json!({ "type": "custom", "tokenizer": "my_words", "filter": ["my_stop"] }),
            json!({
                "tokenizer": { "my_words": { "type": "pattern", "pattern": "\\W+" } },
                "filter": { "my_stop": { "type": "stop", "stopwords": ["and"] } },
            }),
            true,
        ),
        row(
            "a tokenizer the engine does not register",
            json!({ "type": "custom", "tokenizer": "icu_tokenizer" }),
            json!({}),
            false,
        ),
        row(
            "a token filter the engine does not register",
            json!({ "type": "custom", "tokenizer": "standard", "filter": ["icu_folding"] }),
            json!({}),
            false,
        ),
        row(
            "a character filter the engine does not register",
            json!({ "type": "custom", "tokenizer": "standard", "char_filter": ["icu_normalizer"] }),
            json!({}),
            false,
        ),
        row(
            "a defined character filter of a type the engine does not register",
            json!({ "type": "custom", "tokenizer": "standard", "char_filter": ["my_icu"] }),
            json!({ "char_filter": { "my_icu": { "type": "icu_normalizer" } } }),
            false,
        ),
        row(
            "an analyzer type the engine does not register",
            json!({ "type": "icu_analyzer" }),
            json!({}),
            false,
        ),
        row(
            "a stemmer for a language this engine has no algorithm for",
            json!({
                "type": "custom",
                "tokenizer": "standard",
                "filter": [{ "type": "stemmer", "language": "klingon" }],
            }),
            json!({}),
            false,
        ),
    ];

    for (index, row) in rows.iter().enumerate() {
        let index_name = format!("analyzer-index-{index}");
        let mut analysis = json!({ "analyzer": { "probe": row.definition.clone() } });
        if let (Some(target), Some(defined)) = (analysis.as_object_mut(), row.defined.as_object()) {
            for (member, value) in defined {
                target.insert(member.clone(), value.clone());
            }
        }
        let answer = create_index(
            &emulator,
            domain,
            json!({
                "IndexName": index_name,
                "IndexSchema": {
                    "settings": { "analysis": analysis },
                    "mappings": { "properties": { "text": { "type": "text", "analyzer": "probe" } } },
                },
            }),
        )
        .await;
        let read = emulator
            .call(
                "GET",
                &format!("{PREFIX}/opensearch/domain/{domain}/index/{index_name}"),
                None,
            )
            .await;
        if row.accepted {
            assert_served(&answer, row.case);
            assert_served(&read, &format!("GetIndex({})", row.case));
        } else {
            assert_refused(&answer, REFUSED_STATUS, VALIDATION, row.case);
            assert_eq!(
                read.status, RECORDED_NOT_FOUND_STATUS,
                "case {}: a refused create stores no index, got {} with {:?}",
                row.case, read.status, read.body
            );
        }
    }
}
