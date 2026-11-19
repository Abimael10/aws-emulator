// SPDX-License-Identifier: CC0-1.0

//! The rule every pure-read Redshift catalog answers from: what the catalog
//! PUBLISHES, over the awsQuery wire, and the agreement between a catalog read
//! and the operations that consume the same table.
//!
//! Four rules, each asserted over a table of inputs rather than one recorded
//! request:
//!
//! * the orderable-shape catalog offers a pairing for every node type a NEW
//!   cluster can be ordered with and the cluster types that node type is
//!   orderable as — the previous-generation family is offered for neither, and
//!   `single-node` only for the entry-size members — and every pairing it offers
//!   is a cluster `CreateCluster` really creates;
//! * the event catalog returns TEMPLATES: every published `EventDescription`
//!   writes the values an occurrence fills as `<placeholder>`, and the instant it
//!   names as `<time in UTC>`;
//! * a raised event's message is that same published template with its
//!   placeholders filled, so the catalog read and `DescribeEvents` cannot
//!   describe one event two ways;
//! * every reference-catalog read — one whose whole answer is a table the
//!   SERVICE publishes rather than anything the account holds — answers that
//!   seeded table PROJECTED by the selector its request carries: a request
//!   naming none reports the whole table, a selector the table publishes
//!   reports exactly the one entry it names, a value it does not publish is
//!   refused with the error that read declares for it, and a second boot
//!   publishes the same table entry for entry, so a key a client cached still
//!   names the same entry after a restart.
//!
//! The fourth rule is asserted ACROSS the reads that share it, over one table of
//! them, because it is a rule about the class and not about any one catalog: a
//! read that stopped projecting, or that drew an entry's key per boot, is a
//! defect of the rule however faithful that read's own rows are. What each
//! catalog's rows mean is owned by the suite for that read — `event_categories`,
//! `reserved_node_offerings`, `maintenance_tracks` and the first rule above —
//! and is not restated here.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, element_blocks, Emulator};

/// Every node type the model publishes, in its documented order.
///
/// Source: `smithy_query(query="doc", shape="CreateClusterMessage",
/// member="NodeType")` — "Valid Values: dc2.large | dc2.8xlarge | rg.large |
/// rg.xlarge | rg.4xlarge | rg.12xlarge | ra3.large | ra3.xlplus | ra3.4xlarge |
/// ra3.16xlarge".
const NODE_TYPES: [&str; 10] = [
    "dc2.large",
    "dc2.8xlarge",
    "rg.large",
    "rg.xlarge",
    "rg.4xlarge",
    "rg.12xlarge",
    "ra3.large",
    "ra3.xlplus",
    "ra3.4xlarge",
    "ra3.16xlarge",
];

/// The cluster types one node type can be ORDERED as, per node type: the
/// previous generation is not ordered at all, the entry-size member of each
/// current-generation family is ordered as either, and every larger member as
/// `multi-node` alone.
///
/// Source: the orderable-shape catalog the service publishes (what
/// `DescribeOrderableClusterOptions` reports); the model bounds only the two
/// value lists and pairs neither with the other. Written out here, one row per
/// node type the model publishes, so a wrong pairing fails this suite.
const ORDERABLE: [(&str, &[&str]); 10] = [
    ("dc2.large", &[]),
    ("dc2.8xlarge", &[]),
    ("rg.large", &["multi-node", "single-node"]),
    ("rg.xlarge", &["multi-node"]),
    ("rg.4xlarge", &["multi-node"]),
    ("rg.12xlarge", &["multi-node"]),
    ("ra3.large", &["multi-node", "single-node"]),
    ("ra3.xlplus", &["multi-node", "single-node"]),
    ("ra3.4xlarge", &["multi-node"]),
    ("ra3.16xlarge", &["multi-node"]),
];

/// One reference-catalog read: the operation, the input member ONE entry of its
/// table is selected by, the element each entry of the table is carried in, and
/// the element inside an entry that carries the value the selector names.
///
/// The three reads whose whole answer is a table the service publishes:
/// `DescribeEventCategories` (an entry per source type that raises events,
/// selected by `SourceType`), `DescribeReservedNodeOfferings` (an entry per
/// published offering, selected by `ReservedNodeOfferingId`) and
/// `DescribeClusterTracks` (an entry per published maintenance track, selected
/// by `MaintenanceTrackName`).
///
/// Source: `smithy_query(query="operation",
/// operation="DescribeEventCategories,DescribeReservedNodeOfferings,DescribeClusterTracks")`
/// — each read's input members and the `xmlName` of its output list's member
/// (`EventCategoriesMap`, `ReservedNodeOffering`, `MaintenanceTrack`). Written
/// out here so a read that stopped honouring its own selector fails this suite.
struct ReferenceCatalog {
    /// The `Action` the read is called as.
    action: &'static str,
    /// The input member one entry is selected by.
    selector: &'static str,
    /// The element one entry of the table is serialized as.
    entry: &'static str,
    /// The element inside an entry carrying the value [`Self::selector`] names
    /// it by.
    key: &'static str,
    /// The HTTP status and awsQuery error code a selector value the table does
    /// not publish is refused with, from that read's own modelled errors.
    refusal: (u16, &'static str),
}

/// Every reference-catalog read this service serves, in model order.
const REFERENCE_CATALOGS: [ReferenceCatalog; 3] = [
    ReferenceCatalog {
        action: "DescribeEventCategories",
        selector: "SourceType",
        entry: "EventCategoriesMap",
        key: "SourceType",
        // The read models no error of its own
        // (`smithy_query(query="operation",
        // operation="DescribeEventCategories")` — an empty errors list), so a
        // `SourceType` outside the published vocabulary names no selection the
        // service could make and is a bad parameter value.
        refusal: (400, "InvalidParameterValue"),
    },
    ReferenceCatalog {
        action: "DescribeReservedNodeOfferings",
        selector: "ReservedNodeOfferingId",
        entry: "ReservedNodeOffering",
        key: "ReservedNodeOfferingId",
        // `ReservedNodeOfferingNotFoundFault` — "Specified offering does not
        // exist" (HTTP 404), the error the read declares.
        refusal: (404, "ReservedNodeOfferingNotFound"),
    },
    ReferenceCatalog {
        action: "DescribeClusterTracks",
        selector: "MaintenanceTrackName",
        entry: "MaintenanceTrack",
        key: "MaintenanceTrackName",
        // `InvalidClusterTrackFault` — "The provided cluster track name is not
        // valid" (HTTP 400), the error the read declares.
        refusal: (400, "InvalidClusterTrack"),
    },
];

/// The entries one reference-catalog read reports, in page order, each as the
/// inner XML of its own element.
///
/// The WHOLE entry, so a comparison covers every member the read serialized
/// rather than only the ones a suite thought to name.
async fn reference_entries(
    emulator: &Emulator,
    catalog: &ReferenceCatalog,
    case: &str,
    params: &[(&str, &str)],
) -> Vec<String> {
    let answer = emulator.call(catalog.action, params).await;
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    element_blocks(&answer.body, catalog.entry)
}

/// The value the read's selector names one entry by.
///
/// A structure serializes its members in declaration order, so an entry's own
/// key is the FIRST occurrence of that element inside it — a maintenance
/// track's `UpdateTargets` carry the name of ANOTHER track further down.
fn entry_key(catalog: &ReferenceCatalog, case: &str, entry: &str) -> String {
    all_text(entry, catalog.key)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case}: expected a <{}> in {entry}", catalog.key))
}

/// The `(node type, cluster type)` pairs a `DescribeOrderableClusterOptions`
/// body carries, in document order.
///
/// An `OrderableClusterOption` serializes one `NodeType` and one `ClusterType`,
/// so the two element sequences line up index by index.
fn listed_pairs(body: &str) -> Vec<(String, String)> {
    let node_types = all_text(body, "NodeType");
    let cluster_types = all_text(body, "ClusterType");
    assert_eq!(
        node_types.len(),
        cluster_types.len(),
        "an option carries one of each member: got {} node types and {} cluster types in body {body}",
        node_types.len(),
        cluster_types.len()
    );
    node_types.into_iter().zip(cluster_types).collect()
}

#[tokio::test]
async fn the_orderable_catalog_offers_exactly_the_shapes_a_new_cluster_is_ordered_with() {
    let emulator = Emulator::new();

    // The whole catalog first: every row of the table, and nothing else.
    let answer = emulator
        .call("DescribeOrderableClusterOptions", &[])
        .await;
    assert_eq!(
        answer.status, 200,
        "case the whole catalog: expected 200, got {} with body {}",
        answer.status, answer.body
    );
    let whole = listed_pairs(&answer.body);
    let expected: Vec<(String, String)> = ORDERABLE
        .iter()
        .flat_map(|(node_type, cluster_types)| {
            cluster_types
                .iter()
                .map(move |cluster_type| ((*node_type).to_string(), (*cluster_type).to_string()))
        })
        .collect();
    assert_eq!(
        whole, expected,
        "case the whole catalog: expected {expected:?}, got {whole:?}"
    );

    // Then one row per node type the model publishes, selected by name: the
    // catalog offers that node type's orderable cluster types and no others.
    for (node_type, cluster_types) in ORDERABLE {
        let selected = emulator
            .call(
                "DescribeOrderableClusterOptions",
                &[("NodeType", node_type)],
            )
            .await;
        assert_eq!(
            selected.status, 200,
            "case select {node_type}: expected 200, got {} with body {}",
            selected.status, selected.body
        );
        let served = listed_pairs(&selected.body);
        let expected: Vec<(String, String)> = cluster_types
            .iter()
            .map(|cluster_type| (node_type.to_string(), (*cluster_type).to_string()))
            .collect();
        assert_eq!(
            served, expected,
            "case select {node_type}: expected {expected:?}, got {served:?}"
        );
    }

    // The node types the model publishes and the ones the catalog prices an
    // offering of are the same list, so no read invents a node type of its own.
    assert_eq!(
        state::ORDERABLE_NODE_TYPES.to_vec(),
        NODE_TYPES.to_vec(),
        "case published node types: expected the documented list, got {:?}",
        state::ORDERABLE_NODE_TYPES
    );
}

#[tokio::test]
async fn every_shape_the_catalog_offers_is_a_cluster_the_service_creates() {
    let emulator = Emulator::new();

    // One create per offered pairing, read back through `DescribeClusters`: a
    // catalog that offers a shape the service then refuses would send a client
    // that validates against it to a cluster it cannot have.
    let mut created = 0;
    for (node_type, cluster_types) in ORDERABLE {
        for cluster_type in cluster_types {
            let identifier = format!(
                "orderable-{}-{}",
                node_type.replace('.', "-"),
                cluster_type
            );
            let mut params: Vec<(&str, &str)> = vec![
                ("ClusterIdentifier", &identifier),
                ("NodeType", node_type),
                ("MasterUsername", "admin"),
                ("ClusterType", cluster_type),
            ];
            if *cluster_type == "multi-node" {
                params.push(("NumberOfNodes", "2"));
            }
            let answer = emulator.call("CreateCluster", &params).await;
            assert_eq!(
                answer.status, 200,
                "case create {node_type} / {cluster_type}: expected 200, got {} / {:?} with body {}",
                answer.status,
                answer.error_code(),
                answer.body
            );
            let described = emulator
                .call("DescribeClusters", &[("ClusterIdentifier", &identifier)])
                .await;
            assert_eq!(
                (
                    described.status,
                    described.text("NodeType"),
                    described.text("ClusterIdentifier")
                ),
                (
                    200,
                    Some((*node_type).to_string()),
                    Some(identifier.clone())
                ),
                "case read back {node_type} / {cluster_type}: expected the created cluster, got body {}",
                described.body
            );
            created += 1;
        }
    }
    assert_eq!(
        created,
        ORDERABLE
            .iter()
            .map(|(_, cluster_types)| cluster_types.len())
            .sum::<usize>(),
        "case coverage: expected one create per offered pairing, made {created}"
    );
}

#[tokio::test]
async fn the_event_catalog_publishes_templates_and_a_raised_event_fills_one() {
    let emulator = Emulator::new();

    // Every published description, across every source type the catalog admits:
    // the values an occurrence fills are written as `<placeholder>`, never in
    // another bracket form, and no placeholder is left empty.
    let catalog = emulator.call("DescribeEventCategories", &[]).await;
    assert_eq!(
        catalog.status, 200,
        "case read the catalog: expected 200, got {} with body {}",
        catalog.status, catalog.body
    );
    let entries = element_blocks(&catalog.body, "EventInfoMap");
    assert!(
        entries.len() > 1,
        "case read the catalog: expected the published events, got body {}",
        catalog.body
    );
    let mut templates: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        let event_id = all_text(entry, "EventId")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case catalog entry: expected an EventId in {entry}"));
        let description = all_text(entry, "EventDescription")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {event_id}: expected an EventDescription"));
        assert!(
            !description.contains('[') && !description.contains(']'),
            "case {event_id}: expected `<placeholder>` template text, got {description:?}"
        );
        let mut rest = description.as_str();
        while let Some(open) = rest.find('<') {
            let after = &rest[open + 1..];
            let close = after
                .find('>')
                .unwrap_or_else(|| panic!("case {event_id}: unclosed placeholder in {description:?}"));
            assert!(
                !after[..close].is_empty(),
                "case {event_id}: expected a named placeholder, got {description:?}"
            );
            rest = &after[close + 1..];
        }
        templates.push((event_id, description));
    }

    // The one placeholder filled from the clock is named for what it stands for,
    // so a client rendering an event id reads the same template AWS publishes.
    let timed: Vec<&(String, String)> = templates
        .iter()
        .filter(|(_, description)| description.contains("<time in UTC>"))
        .collect();
    assert!(
        !timed.is_empty(),
        "case the clock placeholder: expected published events naming their instant, got {templates:?}"
    );

    // A raised event's message is the published template with its placeholders
    // filled: the resource's name in quotes and the instant in UTC. Asserted on
    // the event a create raises, read back through `DescribeEvents`.
    let identifier = "catalog-template-cluster";
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case seed a cluster: expected 200, got {} with body {}",
        created.status, created.body
    );
    let events = emulator
        .call("DescribeEvents", &[("SourceIdentifier", identifier), ("SourceType", "cluster")])
        .await;
    assert_eq!(
        events.status, 200,
        "case read the events: expected 200, got {} with body {}",
        events.status, events.body
    );
    let event_id = state::EVENT_ID_CLUSTER_CREATED;
    let reported = all_text(&events.body, "EventId");
    assert_eq!(
        reported,
        vec![event_id.to_string()],
        "case read the events: expected the create's own event, got {reported:?}"
    );
    let (_, template) = templates
        .iter()
        .find(|(published, _)| published == event_id)
        .unwrap_or_else(|| panic!("case {event_id}: the catalog read publishes no such event"));
    let message = all_text(&events.body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {event_id}: expected a Message, got body {}", events.body));

    // The template's fixed text either side of each placeholder is the fixed
    // text of the message, with the cluster's own name quoted in between and the
    // instant rendered as `YYYY-MM-DD HH:MM UTC`.
    let filled = template.replace("<cluster name>", &format!("'{identifier}'"));
    let (prefix, suffix) = filled
        .split_once("<time in UTC>")
        .unwrap_or_else(|| panic!("case {event_id}: expected the clock placeholder in {template:?}"));
    assert!(
        message.starts_with(prefix) && message.ends_with(suffix),
        "case {event_id}: expected the published template {template:?} filled in, got {message:?}"
    );
    let instant = &message[prefix.len()..message.len() - suffix.len()];
    assert!(
        instant.len() == "YYYY-MM-DD HH:MM UTC".len()
            && instant.ends_with(" UTC")
            && instant
                .trim_end_matches(" UTC")
                .chars()
                .all(|character| character.is_ascii_digit()
                    || character == '-'
                    || character == ':'
                    || character == ' '),
        "case {event_id}: expected the instant in UTC, got {instant:?} from {message:?}"
    );
}

#[tokio::test]
async fn each_reference_catalog_read_projects_its_published_table_by_the_selector() {
    let emulator = Emulator::new();

    // One row per reference-catalog read: the rows differ in the table they
    // publish, in the member it is selected by and in how many entries it
    // carries, which is what makes this a rule rather than three cases.
    for catalog in &REFERENCE_CATALOGS {
        let action = catalog.action;

        // Answered before the account holds anything: the table is published by
        // the service, so a read of an untouched store already reports it.
        let case = format!("the whole {action} table");
        let whole = reference_entries(&emulator, catalog, &case, &[]).await;
        assert!(
            whole.len() > 1,
            "case the whole {action} table: expected the published table, got {} entry(ies)",
            whole.len()
        );

        let keys: Vec<String> = whole
            .iter()
            .map(|entry| entry_key(catalog, action, entry))
            .collect();
        let mut distinct = keys.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            keys.len(),
            "case {action} keys: expected every entry to carry its own {}, got {keys:?}",
            catalog.selector
        );

        // Every key the table publishes, one request each: the selector reports
        // the ONE entry it names, byte for byte the entry the unfiltered read
        // carried at that position — never the whole table, and never an entry
        // rebuilt for the filtered answer.
        for (position, key) in keys.iter().enumerate() {
            let case = format!("{action} with {} {key}", catalog.selector);
            let selected =
                reference_entries(&emulator, catalog, &case, &[(catalog.selector, key)]).await;
            assert_eq!(
                selected.len(),
                1,
                "case {case}: expected the one entry the selector names out of {}, got {}",
                whole.len(),
                selected.len()
            );
            assert_eq!(
                selected[0], whole[position],
                "case {case}: expected the published entry unchanged, got {:?}",
                selected[0]
            );
        }

        // A selector value the table does NOT publish names no entry, so the
        // read refuses it with the error it declares for that — never an empty
        // page, and never the whole table. The probes are derived from the
        // table's own first key, so they span the ways a value can be wrong
        // whatever the table holds: a name of another shape entirely, one that
        // merely EXTENDS a published key, and one that is only a PREFIX of it.
        let published = keys.first().expect("the table published a key");
        let extended = format!("{published}x");
        // One character short of the published key, cut on a character
        // boundary; a key of one character has no non-empty prefix, and an
        // empty selector is a request naming NO selector rather than a probe,
        // so that row falls back on the published key's own first character
        // doubled, which no key of the table is.
        let truncated = match published.char_indices().next_back() {
            Some((at, _)) if at > 0 => published[..at].to_string(),
            _ => format!("{published}{published}"),
        };
        for (probe, value) in [
            ("a name of another shape", "no-such-entry"),
            ("a published key with a character appended", extended.as_str()),
            ("a published key one character short", truncated.as_str()),
        ] {
            let case = format!("{action} with {probe} ({value:?}) as {}", catalog.selector);
            let answer = emulator
                .call(catalog.action, &[(catalog.selector, value)])
                .await;
            assert_eq!(
                (answer.status, answer.error_code()),
                (catalog.refusal.0, Some(catalog.refusal.1.to_string())),
                "case {case}: expected the declared refusal, got {} / {:?} with body {}",
                answer.status,
                answer.error_code(),
                answer.body
            );
        }
    }
}

#[tokio::test]
async fn every_reference_catalog_publishes_the_same_table_on_a_second_boot() {
    // Two independently constructed emulators, each seeding its own store: what
    // a restart gives a client. A table built per boot — an entry whose key is
    // drawn when the store is created — differs between the two, and a key
    // cached from the first names nothing in the second.
    let first = Emulator::new();
    let second = Emulator::new();

    for catalog in &REFERENCE_CATALOGS {
        let action = catalog.action;
        let before =
            reference_entries(&first, catalog, &format!("{action} on the first boot"), &[]).await;
        let after =
            reference_entries(&second, catalog, &format!("{action} on the second boot"), &[]).await;
        assert!(
            !before.is_empty(),
            "case {action} on the first boot: expected the published table, got no entry"
        );
        assert_eq!(
            before, after,
            "case {action} across boots: expected the same table entry for entry, got {} entry(ies) \
             then {} entry(ies)",
            before.len(),
            after.len()
        );

        // And each key read off the FIRST boot still selects its own entry on
        // the second, which is what a client caching one across a restart does.
        for (position, entry) in before.iter().enumerate() {
            let key = entry_key(catalog, action, entry);
            let case =
                format!("{action} with {} {key} cached from the first boot", catalog.selector);
            let selected =
                reference_entries(&second, catalog, &case, &[(catalog.selector, &key)]).await;
            assert_eq!(
                selected,
                vec![after[position].clone()],
                "case {case}: expected the entry the cached key names, got {selected:?}"
            );
        }
    }
}
