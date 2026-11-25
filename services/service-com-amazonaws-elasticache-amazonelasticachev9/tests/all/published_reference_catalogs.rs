// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the reference CATALOGS this service publishes and for
//! how the catalog reads project them: the engine default parameters, a group's
//! effective parameters, the reserved-cache-node offerings and the service
//! updates.
//!
//! The rules under test, one per catalog:
//!
//! * a family's parameter catalog is published in the order of the WORDS of
//!   each parameter name — the name with the hyphens that separate its words
//!   removed — so the ACL-log row precedes the ACL-pubsub row and the
//!   active-defragmentation flag precedes the active-defragmentation effort
//!   counters, and both reads of the catalog report that one order;
//! * every row of it carries the published catalog's own metadata for the
//!   parameter (its default value, data type, value set and one-line
//!   description), identical through the engine-default read, a family default
//!   group and a group of one's own;
//! * a parameter whose value depends on the cache node type carries, per node
//!   type, the value the node type's published CAPACITY gives it: `maxmemory` is
//!   that capacity, the replication output-buffer limits are a tenth of it and
//!   Memcached's cache size is the same capacity in mebibytes — so two node
//!   types of one SIZE in different families report different values;
//! * every published offering is a stable id bound to one (engine, node type,
//!   term, payment option) tuple, with the price triple that payment option
//!   calls for;
//! * every published service update carries its own name, prose and window,
//!   reported identically by the list read and by the read that names it, and
//!   the same on a second store this build brings up.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts, reusing the harness in [`crate::harness`] and the parameter
//! scanners in [`crate::describe_cache_parameters`]; only the HTTP status and
//! the XML body are inspected, so a catalog that is not served from the
//! persisted reference data cannot pass. The tables range over what each rule
//! generalises over — the family, the parameter KIND (a counter with a closed
//! range, a flag, an enumeration of literals, a size), the node type (two sizes
//! of one family and two families at one size), the engine and the payment
//! option — so the one family, node type and offering a recorded call happens
//! to name are each one row of many.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::describe_cache_parameters::{create_group, describe_ok, one, paged_names};
use crate::harness::{params, Emulator, Reply};

/// The group this suite creates to read a family's catalog through a group of
/// one's own, as the parameter-group workflow does.
const CUSTOM_GROUP: &str = "published-catalog-rows";

/// The families every parameter row below is asserted on: the two Redis OSS
/// generations that publish the ACL parameters and the current Valkey line, so
/// no rule turns on the one family a recorded call names.
const PARAMETER_FAMILIES: [&str; 3] = ["redis6.x", "redis7", "valkey8"];

/// Rows of the published parameter catalog, as `(parameter, default value, data
/// type, allowed values, description)`.
///
/// Source: the parameter table of the ElastiCache User Guide's Redis OSS /
/// Valkey parameter reference — each row's published default, data type, value
/// set and one-line description. The rows span the kinds the catalog is made of:
/// a counter with a closed range, two enumerations of literal values, a flag, a
/// size with an upper bound and a sampling count.
const PUBLISHED_ROWS: [(&str, &str, &str, &str, &str); 6] = [
    (
        "acllog-max-len",
        "128",
        "integer",
        "1-10000",
        "The maximum length of the ACL Log",
    ),
    (
        "acl-pubsub-default",
        "allchannels",
        "string",
        "resetchannels,allchannels",
        "Default pubsub channel permissions for ACL users deployed to this cluster",
    ),
    (
        "activedefrag",
        "no",
        "string",
        "yes,no",
        "Enabled active defragmentation",
    ),
    (
        "active-defrag-cycle-max",
        "75",
        "integer",
        "1-75",
        "Maximal effort for defrag in CPU percentage",
    ),
    (
        "hll-sparse-max-bytes",
        "3000",
        "integer",
        "1-16000",
        "HyperLogLog sparse representation bytes limit",
    ),
    (
        "maxmemory-samples",
        "3",
        "integer",
        "1-64",
        "Max memory samples",
    ),
];

/// Pairs of parameters whose published order the catalog must report, as
/// `(first, second)`.
///
/// Source: the order a parameter read answers in — the words of the name decide
/// it, not the hyphens between them, so `acllog-max-len` comes before
/// `acl-pubsub-default` and `activedefrag` before `active-defrag-cycle-max`,
/// both of which comparing the raw names would reverse.
const PUBLISHED_ORDER_PAIRS: [(&str, &str); 3] = [
    ("acllog-max-len", "acl-pubsub-default"),
    ("activedefrag", "active-defrag-cycle-max"),
    ("activedefrag", "activerehashing"),
];

/// The published capacity of a cache node type, as `(node type, maxmemory in
/// bytes, Memcached cache size in mebibytes, replication output-buffer hard
/// limit in bytes)`.
///
/// Source: the per-node-type values the ElastiCache node-type-specific
/// parameter tables publish for `maxmemory`, `max_cache_memory` and
/// `client-output-buffer-limit-replica-hard-limit`. The rows span two sizes of
/// one family and three families at one size, which is what the rule
/// generalises over: the capacity belongs to the node TYPE, so two types of one
/// size do not report the same value.
const NODE_CAPACITIES: [(&str, i64, i64, i64); 5] = [
    ("cache.m4.large", 6_892_290_048, 6_573, 689_229_005),
    ("cache.m5.large", 6_854_542_745, 6_537, 685_454_275),
    ("cache.m5.xlarge", 13_891_534_134, 13_248, 1_389_153_413),
    ("cache.r5.large", 14_037_181_030, 13_387, 1_403_718_103),
    ("cache.t3.micro", 536_870_912, 512, 53_687_091),
];

/// The offering catalog slices this suite asserts the pricing rule on, as
/// `(engine, node type)` — node types the current catalog publishes beside the
/// ones a recorded call names, on two different engines.
const OFFERING_SLICES: [(&str, &str); 3] = [
    ("redis", "cache.m4.large"),
    ("valkey", "cache.r4.xlarge"),
    ("memcached", "cache.t3.small"),
];

/// Read `family`'s engine defaults in one page: the documented maximum
/// `MaxRecords` is 100, which is above every published family's parameter count.
async fn engine_defaults(emulator: &Emulator, family: &str) -> Reply {
    let reply = emulator
        .call(
            "DescribeEngineDefaultParameters",
            &params(&[
                ("CacheParameterGroupFamily", family),
                ("MaxRecords", "100"),
            ]),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {family}: DescribeEngineDefaultParameters must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
}

/// The order the published catalog reports `names` in: by the WORDS of each
/// name (the name with its hyphens removed), ties broken by the raw name.
///
/// Derived from the names a read reported rather than from the catalog's stored
/// key, so what is asserted is the published order and not how this crate
/// happens to hold it.
fn published_order(names: &[String]) -> Vec<String> {
    let mut ordered = names.to_vec();
    ordered.sort_by_key(|name| (name.replace('-', ""), name.clone()));
    ordered
}

#[tokio::test]
async fn every_family_publishes_its_parameters_in_the_catalog_s_word_order() {
    let emulator = Emulator::new();

    // The axis: the family, so every published generation of both engines is a
    // row and the order rule is held on each engine's own catalog.
    for family in state::PARAMETER_GROUP_FAMILIES {
        let group = format!("default.{family}");
        for (read, reply) in [
            ("engine defaults", engine_defaults(&emulator, family).await),
            (
                "default group",
                describe_ok(&emulator, &group, &[("MaxRecords", "100")]).await,
            ),
        ] {
            let reported = paged_names(&reply);
            assert!(
                !reported.is_empty(),
                "case {family}/{read}: a published family reports parameters, got {}",
                reply.body
            );
            assert_eq!(
                reported,
                published_order(&reported),
                "case {family}/{read}: expected the published word order, got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_published_order_of_two_parameters_does_not_turn_on_their_hyphens() {
    let emulator = Emulator::new();
    create_group(&emulator, CUSTOM_GROUP, "redis7").await;

    // The axis: the pair of parameters whose published order is asserted, and
    // the read it is asserted through. Each pair is one the raw-name order
    // would reverse.
    for (first, second) in PUBLISHED_ORDER_PAIRS {
        for (read, reply) in [
            ("engine defaults", engine_defaults(&emulator, "redis7").await),
            (
                "own group",
                describe_ok(&emulator, CUSTOM_GROUP, &[("MaxRecords", "100")]).await,
            ),
        ] {
            let reported = paged_names(&reply);
            let position = |name: &str| -> usize {
                reported
                    .iter()
                    .position(|reported| reported == name)
                    .unwrap_or_else(|| {
                        panic!("case {first}/{second} {read}: {name} must be reported")
                    })
            };
            assert!(
                position(first) < position(second),
                "case {read}: expected {first} before {second}, got positions {} and {}",
                position(first),
                position(second)
            );
        }
    }
}

#[tokio::test]
async fn every_read_reports_the_published_metadata_of_a_catalog_row() {
    let emulator = Emulator::new();

    // The axes: the family publishing the row, the row itself and the read it
    // is reported through — the family's engine defaults, the family's default
    // group and a group of one's own created from that family.
    for family in PARAMETER_FAMILIES {
        let own_group = format!("{CUSTOM_GROUP}-{}", family.replace('.', ""));
        create_group(&emulator, &own_group, family).await;
        let default_group = format!("default.{family}");
        let reads = [
            ("engine defaults", engine_defaults(&emulator, family).await),
            (
                "default group",
                describe_ok(&emulator, &default_group, &[("MaxRecords", "100")]).await,
            ),
            (
                "own group",
                describe_ok(&emulator, &own_group, &[("MaxRecords", "100")]).await,
            ),
        ];
        for (name, value, data_type, allowed_values, description) in PUBLISHED_ROWS {
            for (read, reply) in &reads {
                let case = format!("{family}/{name}/{read}");
                let reported = one(reply, &case, name);
                assert_eq!(
                    reported.field("ParameterValue"),
                    Some(value),
                    "case {case}: expected the published default {value:?}, got {:?}",
                    reported.field("ParameterValue")
                );
                assert_eq!(
                    reported.field("DataType"),
                    Some(data_type),
                    "case {case}: expected data type {data_type:?}, got {:?}",
                    reported.field("DataType")
                );
                assert_eq!(
                    reported.field("AllowedValues"),
                    Some(allowed_values),
                    "case {case}: expected allowed values {allowed_values:?}, got {:?}",
                    reported.field("AllowedValues")
                );
                assert_eq!(
                    reported.field("Description"),
                    Some(description),
                    "case {case}: expected the published description {description:?}, got {:?}",
                    reported.field("Description")
                );
                assert_eq!(
                    reported.field("Source"),
                    Some(state::PARAMETER_SOURCE_SYSTEM),
                    "case {case}: an untouched row reports the service's value, got {:?}",
                    reported.field("Source")
                );
            }
        }
    }
}

#[tokio::test]
async fn a_node_type_specific_value_is_the_node_type_s_published_capacity() {
    let emulator = Emulator::new();

    // The axis: the node type, spanning two sizes of one family and three
    // families at one size. Each row is asserted on the Redis OSS-compatible
    // catalog (memory in bytes) and on the Memcached one (the same capacity in
    // mebibytes), which is what makes the capacity the node type's own rather
    // than each catalog's.
    let redis = engine_defaults(&emulator, "redis7").await;
    let memcached = engine_defaults(&emulator, "memcached1.6").await;
    for (node_type, maxmemory, cache_size_mib, replica_hard_limit) in NODE_CAPACITIES {
        for (parameter, expected, reply) in [
            ("maxmemory", maxmemory, &redis),
            (
                "client-output-buffer-limit-replica-hard-limit",
                replica_hard_limit,
                &redis,
            ),
            ("max_cache_memory", cache_size_mib, &memcached),
        ] {
            let case = format!("{node_type}/{parameter}");
            let reported = node_type_value(reply, &case, parameter, node_type);
            assert_eq!(
                reported,
                expected.to_string(),
                "case {case}: expected the published value {expected}, got {reported}"
            );
        }
    }

    // Two node types of one SIZE in different families publish different
    // capacities, which is what makes the capacity a fact about the node type.
    let general = node_type_value(&redis, "same size", "maxmemory", "cache.m5.large");
    let memory_optimized = node_type_value(&redis, "same size", "maxmemory", "cache.r5.large");
    assert!(
        general != memory_optimized,
        "case same size: cache.m5.large and cache.r5.large must publish their own capacity, \
         both reported {general}"
    );
}

/// The value `parameter` publishes for `node_type` in a catalog read.
///
/// The `CacheNodeTypeSpecificValue` pairs of one parameter are the ones between
/// its `ParameterName` and the next parameter's, so a parameter cannot borrow
/// the next one's values.
fn node_type_value(reply: &Reply, case: &str, parameter: &str, node_type: &str) -> String {
    let mut nodes = reply
        .nodes
        .iter()
        .skip_while(|(path, text)| !(path.ends_with("/ParameterName") && text == parameter));
    nodes
        .next()
        .unwrap_or_else(|| panic!("case {case}: {parameter} must be reported"));
    let pairs: Vec<(&str, &str)> = nodes
        .take_while(|(path, _)| !path.ends_with("/ParameterName"))
        .filter(|(path, _)| {
            path.ends_with("/CacheNodeTypeSpecificValue/CacheNodeType")
                || path.ends_with("/CacheNodeTypeSpecificValue/Value")
        })
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let mut found: Option<String> = None;
    let mut seen: Option<&str> = None;
    for (path, text) in pairs {
        if path.ends_with("/CacheNodeType") {
            seen = Some(text);
        } else if seen == Some(node_type) {
            found = Some(text.to_string());
        }
    }
    found.unwrap_or_else(|| panic!("case {case}: {parameter} must publish a value for {node_type}"))
}

/// One `ReservedCacheNodesOffering` element of a response, as the wire reports
/// it.
#[derive(Debug, Clone, PartialEq)]
struct WireOffering {
    id: String,
    cache_node_type: String,
    product_description: String,
    duration: String,
    offering_type: String,
    fixed_price: f64,
    usage_price: f64,
    recurring_charge: f64,
}

/// Every offering a reply reports, in document order.
///
/// Every member is read as a column and the columns are zipped, which is also
/// the assertion that each offering carries every member of the shape — the
/// recurring charge included, since an offering that paid for its whole term up
/// front still publishes the charge as zero.
fn wire_offerings(case: &str, reply: &Reply) -> Vec<WireOffering> {
    let column = |member: &str| -> Vec<String> {
        reply
            .all(member)
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    let ids = column("ReservedCacheNodesOfferingId");
    let node_types = column("CacheNodeType");
    let engines = column("ProductDescription");
    let durations = column("Duration");
    let offering_types = column("OfferingType");
    let fixed = column("FixedPrice");
    let usage = column("UsagePrice");
    let recurring = column("RecurringChargeAmount");
    for (member, values) in [
        ("CacheNodeType", &node_types),
        ("ProductDescription", &engines),
        ("Duration", &durations),
        ("OfferingType", &offering_types),
        ("FixedPrice", &fixed),
        ("UsagePrice", &usage),
        ("RecurringChargeAmount", &recurring),
    ] {
        assert_eq!(
            values.len(),
            ids.len(),
            "case {case}: {member} must be reported once per offering ({}), got {}: {}",
            ids.len(),
            values.len(),
            reply.body
        );
    }
    let number = |member: &str, text: &str| -> f64 {
        text.parse()
            .unwrap_or_else(|_| panic!("case {case}: {member} must be a number, got {text:?}"))
    };
    (0..ids.len())
        .map(|index| WireOffering {
            id: ids[index].clone(),
            cache_node_type: node_types[index].clone(),
            product_description: engines[index].clone(),
            duration: durations[index].clone(),
            offering_type: offering_types[index].clone(),
            fixed_price: number("FixedPrice", &fixed[index]),
            usage_price: number("UsagePrice", &usage[index]),
            recurring_charge: number("RecurringChargeAmount", &recurring[index]),
        })
        .collect()
}

#[tokio::test]
async fn every_published_offering_is_a_stable_id_bound_to_its_own_price_triple() {
    let emulator = Emulator::new();

    // The axes: the engine and the cache node type the catalog is sliced by,
    // and within a slice the term and the payment option, which is what the
    // price triple follows from.
    for (engine, node_type) in OFFERING_SLICES {
        let case = format!("{engine}/{node_type}");
        let filter = [
            ("ProductDescription", engine),
            ("CacheNodeType", node_type),
        ];
        let reply = emulator
            .call(
                "DescribeReservedCacheNodesOfferings",
                &params(&filter),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: must succeed, got {} {}",
            reply.status, reply.body
        );
        let offerings = wire_offerings(&case, &reply);
        assert_eq!(
            offerings.len(),
            state::OFFERING_TERMS.len() * state::STOCKED_OFFERING_TYPES.len(),
            "case {case}: expected one offering per term and payment option, got {offerings:?}"
        );
        let mut tuples: BTreeSet<(String, String)> = BTreeSet::new();
        for offering in &offerings {
            let WireOffering {
                id,
                cache_node_type,
                product_description,
                duration,
                offering_type,
                fixed_price,
                usage_price,
                recurring_charge,
            } = offering;
            let row = format!("{case}/{duration}/{offering_type}");
            assert_eq!(
                cache_node_type, node_type,
                "case {row}: expected the filtered node type, got {cache_node_type}"
            );
            assert_eq!(
                product_description, engine,
                "case {row}: expected the filtered engine, got {product_description}"
            );
            assert!(
                state::OFFERING_TERMS
                    .iter()
                    .any(|(_, seconds)| seconds.to_string() == *duration),
                "case {row}: expected one of the published terms, got {duration}"
            );
            assert_eq!(
                *usage_price, 0.0,
                "case {row}: a reservation's hourly charge is its recurring charge, got \
                 {usage_price}"
            );
            // The payment option decides the triple: a no-upfront reservation
            // pays its whole term hourly, an all-upfront one pays it up front,
            // and a partial-upfront one pays both.
            let (fixed_is_paid, recurring_is_paid) = match offering_type.as_str() {
                state::OFFERING_TYPE_NO_UPFRONT => (false, true),
                state::OFFERING_TYPE_ALL_UPFRONT => (true, false),
                _ => (true, true),
            };
            assert_eq!(
                *fixed_price > 0.0,
                fixed_is_paid,
                "case {row}: unexpected FixedPrice {fixed_price} for {offering_type}"
            );
            assert_eq!(
                *recurring_charge > 0.0,
                recurring_is_paid,
                "case {row}: unexpected recurring charge {recurring_charge} for {offering_type}"
            );
            assert!(
                tuples.insert((duration.clone(), offering_type.clone())),
                "case {row}: a term and payment option name ONE offering, got {offerings:?}"
            );

            // The id names that one offering: asking for it alone answers the
            // same record, with the same attributes and the same prices.
            let by_id = emulator
                .call(
                    "DescribeReservedCacheNodesOfferings",
                    &params(&[("ReservedCacheNodesOfferingId", id)]),
                )
                .await;
            assert_eq!(
                by_id.status, 200,
                "case {row}: a published id must resolve, got {} {}",
                by_id.status, by_id.body
            );
            let same = wire_offerings(&row, &by_id);
            assert_eq!(
                same.as_slice(),
                ::std::slice::from_ref(offering),
                "case {row}: the record an id resolves to must be the one the catalog listed"
            );
        }
    }
}

#[tokio::test]
async fn the_offering_catalog_publishes_the_same_ids_on_a_second_store() {
    // An offering is a published catalog entry, not per-account state, so its
    // id is the same on every store this build brings up: a client that saved
    // an id can still buy the reservation it names.
    let first = Emulator::new();
    let second = Emulator::new();
    let filter = [("CacheNodeType", "cache.m4.large")];
    let mut ids: Vec<Vec<String>> = Vec::new();
    for emulator in [&first, &second] {
        let reply = emulator
            .call("DescribeReservedCacheNodesOfferings", &params(&filter))
            .await;
        assert_eq!(
            reply.status, 200,
            "a filtered catalog read must succeed, got {} {}",
            reply.status, reply.body
        );
        ids.push(
            reply
                .all("ReservedCacheNodesOfferingId")
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
    }
    assert!(
        !ids[0].is_empty(),
        "case cache.m4.large: the catalog publishes offerings for a node type it sells"
    );
    assert_eq!(
        ids[0], ids[1],
        "case cache.m4.large: a published offering id is the same on every store, got {:?} and {:?}",
        ids[0], ids[1]
    );
}

/// Every service update a reply reports, keyed by `ServiceUpdateName`, as the
/// members a client branches on.
fn wire_updates(case: &str, reply: &Reply) -> BTreeMap<String, Vec<String>> {
    let names = reply.all("ServiceUpdateName");
    let column = |member: &str| -> Vec<String> {
        let values = reply.all(member);
        assert_eq!(
            values.len(),
            names.len(),
            "case {case}: {member} must be reported once per update ({}), got {}: {}",
            names.len(),
            values.len(),
            reply.body
        );
        values.into_iter().map(str::to_string).collect()
    };
    let columns: Vec<Vec<String>> = [
        "Engine",
        "EngineVersion",
        "ServiceUpdateType",
        "ServiceUpdateSeverity",
        "ServiceUpdateDescription",
        "ServiceUpdateReleaseDate",
        "ServiceUpdateRecommendedApplyByDate",
        "ServiceUpdateEndDate",
        "AutoUpdateAfterRecommendedApplyByDate",
        "EstimatedUpdateTime",
    ]
    .into_iter()
    .map(column)
    .collect();
    names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.to_string(),
                columns.iter().map(|values| values[index].clone()).collect(),
            )
        })
        .collect()
}

/// Read the whole published service-update catalog in one page.
async fn service_updates(emulator: &Emulator, extra: &[(&str, &str)]) -> Reply {
    let mut pairs = params(&[("MaxRecords", "100")]);
    pairs.extend(params(extra));
    let reply = emulator.call("DescribeServiceUpdates", &pairs).await;
    assert_eq!(
        reply.status, 200,
        "case {extra:?}: DescribeServiceUpdates must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
}

#[tokio::test]
async fn every_published_service_update_is_its_own_record_on_every_store() {
    let first = Emulator::new();
    let second = Emulator::new();
    let catalog = wire_updates("list", &service_updates(&first, &[]).await);
    assert!(
        !catalog.is_empty(),
        "the service publishes a service-update catalog"
    );
    assert_eq!(
        catalog,
        wire_updates("second store", &service_updates(&second, &[]).await),
        "a published update is the same record on every store this build brings up"
    );

    // The axis: the update. Each one is read BY NAME as well, which is the
    // assertion that none of its members is composed from the request: the
    // record a name resolves to is the record the list carried.
    for (name, members) in &catalog {
        let reply = service_updates(&first, &[("ServiceUpdateName", name)]).await;
        let by_name = wire_updates(name, &reply);
        assert_eq!(
            by_name.len(),
            1,
            "case {name}: a name resolves to one update, got {by_name:?}"
        );
        assert_eq!(
            by_name.get(name),
            Some(members),
            "case {name}: the record a name resolves to must be the one the list carried"
        );
    }
}

#[tokio::test]
async fn the_published_catalog_carries_updates_a_client_may_still_apply() {
    let emulator = Emulator::new();

    // The axis: the `ServiceUpdateStatus` the filter names. A published catalog
    // carries updates inside their availability window as well as ones the
    // clock has carried past it, so the filter a compliance client uses —
    // "which updates may I still apply?" — answers with records rather than
    // with nothing.
    let available = wire_updates(
        "available",
        &service_updates(&emulator, &[("ServiceUpdateStatus.member.1", "available")]).await,
    );
    assert!(
        !available.is_empty(),
        "case available: the catalog must publish updates still inside their window"
    );
    let expired = wire_updates(
        "expired",
        &service_updates(&emulator, &[("ServiceUpdateStatus.member.1", "expired")]).await,
    );
    assert!(
        !expired.is_empty(),
        "case expired: the catalog must publish updates the clock has carried past their end date"
    );
    let whole = wire_updates("list", &service_updates(&emulator, &[]).await);
    assert_eq!(
        available.len() + expired.len(),
        whole.len(),
        "case statuses: every published update is available or expired, got {} and {} of {}",
        available.len(),
        expired.len(),
        whole.len()
    );
    for (name, members) in &available {
        assert_eq!(
            whole.get(name),
            Some(members),
            "case {name}: a filtered read reports the record the whole catalog carries"
        );
    }
}
