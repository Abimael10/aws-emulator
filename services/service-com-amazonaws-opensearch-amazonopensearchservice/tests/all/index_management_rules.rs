// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the rules the INDEX-MANAGEMENT operations share — `CreateIndex`,
//! `GetIndex`, `UpdateIndex` and `DeleteIndex` — driven over the real wire path (a
//! `restJson1` HTTP request through the multi-service router into the mounted service), so
//! what is asserted is what a client sees.
//!
//! The rules under test, each over a table spanning what it generalises over rather than one
//! recorded call:
//!
//! * a READ reports one index's schema KEYED BY THE INDEX NAME, the way the engine's own
//!   mapping API reports an index, so `IndexSchema[<indexName>]` is where a client reads the
//!   sections from — over rows differing in the sections the index carries, in the name's
//!   character classes and length, and over two indexes of one domain;
//! * an `UpdateIndex` writes an index's mappings and semantic enrichment and NOT the
//!   settings of the index itself: a schema naming a settings section at all is refused,
//!   whatever that section holds and however it is spelled, and the index keeps its schema;
//! * a mapping document the engine's mapping parser cannot READ (a value where a set of
//!   members belongs) leaves the merge the service's own work unfinished, so the update
//!   answers the modelled `InternalException`, while a mapping it reads and that names
//!   something the engine has no mapper for — a field type outside its catalog, a `dynamic`
//!   value it does not know — is the caller's input and so its `ValidationException`; both
//!   leave the index exactly as it was;
//! * these operations are served by a domain whose ENGINE serves them: a domain running an
//!   engine release that does not is refused `ValidationException` with the service's
//!   software-version sentence, and that refusal is answered BEFORE the index is looked up,
//!   so a name no index of the domain carries earns it too;
//! * an analysis component is addressed by the NAME its settings define it under, and a name
//!   carrying a character outside ASCII is one the domain's cluster cannot be handed: both
//!   index writes refuse such a document with the modelled `DependencyFailureException` and
//!   the service's fixed dependency sentence, the refusal is sustained across retries, and
//!   nothing is stored — over a table spanning every kind of component those settings define,
//!   both spellings of the settings section and a range of scripts, with the ASCII control
//!   served on the same domain.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    self, INDEX_MANAGEMENT_ENGINE_VERSION, INDEX_SETTINGS_UPDATE_UNSUPPORTED,
    INTERNAL_SERVICE_ERROR, MAPPING_FIELD_TYPES,
};

use super::data_source_registry::created_domain;
use super::domain_lifecycle::{error_code, settled_domain, string_member, Emulator, PREFIX};
use super::index_registry::{create as create_index, created_index, get as get_index};
use super::index_schema_updates::{read_schema, update as update_index, updated_index};

/// `DeleteIndex`: `DELETE /opensearch/domain/{DomainName}/index/{IndexName}`, the fourth
/// index-management operation the gate below is asserted over.
async fn delete_index(emulator: &Emulator, domain: &str, index: &str) -> super::domain_lifecycle::Answer {
    emulator
        .call(
            "DELETE",
            &format!("{PREFIX}/opensearch/domain/{domain}/index/{index}"),
            None,
        )
        .await
}

/// One index a read reports: the name it was created under and the schema it carries.
struct ReadRow {
    case: &'static str,
    name: &'static str,
    schema: Value,
}

/// RULE: `GetIndex` reports the index's schema under the index's own NAME, so a client that
/// asked about an index by name reads its sections from `IndexSchema[<indexName>]`, and the
/// document under that name is the one the write persisted.
#[tokio::test]
async fn a_read_reports_one_indexs_schema_keyed_by_its_name() {
    let emulator = Emulator::new();
    created_domain(&emulator, "keyed-reads").await;

    // The rows differ along the sections the schema carries, the JSON value types in it and
    // the name's own character classes and length — not along one recorded index.
    let rows = [
        ReadRow {
            case: "mappings alone",
            name: "logs-2026.01_v2",
            schema: json!({ "mappings": { "properties": { "title": { "type": "text" } } } }),
        },
        ReadRow {
            case: "mappings and settings together",
            name: "a",
            schema: json!({
                "mappings": { "properties": { "body": { "type": "keyword" } } },
                "settings": { "index": { "number_of_shards": 2 } }
            }),
        },
        ReadRow {
            case: "semantic enrichment beside the mapping",
            name: "semantic-index",
            schema: json!({
                "mappings": {
                    "properties": {
                        "summary": { "type": "text", "semantic_enrichment": { "enabled": true } }
                    }
                }
            }),
        },
        ReadRow {
            case: "an index created with no sections at all",
            name: "bare",
            schema: json!({}),
        },
    ];

    for row in &rows {
        created_index(&emulator, "keyed-reads", row.name, &row.schema, row.case).await;
    }

    for row in &rows {
        let read = get_index(&emulator, "keyed-reads", row.name).await;
        assert_eq!(
            read.status, 200,
            "case {}: expected 200 from GetIndex, got {} with {:?}",
            row.case, read.status, read.body
        );
        let reported = read.body["IndexSchema"].clone();
        assert!(
            reported.get(row.name).is_some(),
            "case {}: expected the schema under the index name {:?}, got {reported:?}",
            row.case,
            row.name
        );
        assert_eq!(
            reported[row.name], row.schema,
            "case {}: expected the stored schema under that name, got {reported:?}",
            row.case
        );
        // The report is about the index the request named and no other: it carries exactly
        // one entry, which is that index's.
        assert_eq!(
            reported.as_object().map(|sections| sections.len()),
            Some(1),
            "case {}: expected exactly the named index in the report, got {reported:?}",
            row.case
        );
        for other in rows.iter().filter(|other| other.name != row.name) {
            assert!(
                reported.get(other.name).is_none(),
                "case {}: expected no other index in the report, got {reported:?}",
                row.case
            );
        }
    }
}

/// One update that must be refused: what it carries, and the channel and sentence the
/// service answers it on.
struct RefusedRow {
    case: &'static str,
    /// The `IndexSchema` the update carries.
    schema: Value,
    /// The HTTP status and error code the refusal is answered on.
    status: u16,
    code: &'static str,
    /// The whole message, where the service's sentence for the class is fixed; `None` where
    /// the sentence names the offending part of the document, which the row checks for
    /// instead.
    message: Option<&'static str>,
    /// A part of the document the refusal must name, for a sentence built from the path.
    names: Option<&'static str>,
}

/// RULE: an `UpdateIndex` writes an index's mappings and semantic enrichment and not the
/// settings of the index itself, a mapping document the mapping parser cannot read is the
/// service's own internal failure, and a mapping that names something the engine has no
/// mapper for is refused as the input it is — every one of them leaving the index exactly as
/// it was.
#[tokio::test]
async fn an_update_refuses_a_settings_section_and_a_mapping_the_engine_cannot_build() {
    let emulator = Emulator::new();
    created_domain(&emulator, "update-rules").await;
    let created = json!({
        "mappings": { "properties": { "title": { "type": "text" } } },
        "settings": { "index": { "number_of_shards": 1 } }
    });

    // The rows differ along WHICH part of the document offends and along how it offends:
    // the settings section in each spelling a client writes it in, a mapping section that is
    // not a set of members at each level one can stand at, and a mapping that is read and
    // names a type or a `dynamic` value the engine has none of.
    let rows = [
        RefusedRow {
            case: "settings spelled directly",
            schema: json!({ "settings": { "number_of_replicas": 2 } }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "settings spelled under the index section",
            schema: json!({ "settings": { "index": { "number_of_replicas": 2 } } }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "a settings name written flattened",
            schema: json!({ "settings": { "index.number_of_replicas": 2 } }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "a settings section naming no setting at all",
            schema: json!({ "settings": {} }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "a settings section that is not a set of members",
            schema: json!({ "settings": "number_of_replicas=2" }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "settings beside a mapping that would otherwise land",
            schema: json!({
                "mappings": { "properties": { "summary": { "type": "text" } } },
                "settings": { "index": { "number_of_replicas": 2 } }
            }),
            status: 400,
            code: "ValidationException",
            message: Some(INDEX_SETTINGS_UPDATE_UNSUPPORTED),
            names: None,
        },
        RefusedRow {
            case: "a mappings section that is not a set of members",
            schema: json!({ "mappings": "properties" }),
            status: 500,
            code: "InternalException",
            message: Some(INTERNAL_SERVICE_ERROR),
            names: None,
        },
        RefusedRow {
            case: "a properties section that is not a set of members",
            schema: json!({ "mappings": { "properties": ["title"] } }),
            status: 500,
            code: "InternalException",
            message: Some(INTERNAL_SERVICE_ERROR),
            names: None,
        },
        RefusedRow {
            case: "a field mapping that is not a set of members",
            schema: json!({ "mappings": { "properties": { "summary": "text" } } }),
            status: 500,
            code: "InternalException",
            message: Some(INTERNAL_SERVICE_ERROR),
            names: None,
        },
        RefusedRow {
            case: "a multi-field section that is not a set of members",
            schema: json!({
                "mappings": { "properties": { "title": { "fields": 7 } } }
            }),
            status: 500,
            code: "InternalException",
            message: Some(INTERNAL_SERVICE_ERROR),
            names: None,
        },
        RefusedRow {
            case: "a field type the engine has no mapper for",
            schema: json!({
                "mappings": { "properties": { "summary": { "type": "stringy" } } }
            }),
            status: 400,
            code: "ValidationException",
            message: None,
            names: Some("summary"),
        },
        RefusedRow {
            case: "a field type that is not a name at all",
            schema: json!({ "mappings": { "properties": { "summary": { "type": 7 } } } }),
            status: 400,
            code: "ValidationException",
            message: None,
            names: Some("summary"),
        },
        RefusedRow {
            case: "a type the engine has no mapper for on a multi-field",
            schema: json!({
                "mappings": {
                    "properties": { "title": { "fields": { "raw": { "type": "keywordish" } } } }
                }
            }),
            status: 400,
            code: "ValidationException",
            message: None,
            names: Some("raw"),
        },
        RefusedRow {
            case: "a dynamic value outside the engine's set",
            schema: json!({ "mappings": { "dynamic": "sometimes" } }),
            status: 400,
            code: "ValidationException",
            message: None,
            names: Some("dynamic"),
        },
    ];

    for (at, row) in rows.iter().enumerate() {
        let index = format!("update-row-{at}");
        created_index(&emulator, "update-rules", &index, &created, row.case).await;

        let answer = update_index(&emulator, "update-rules", &index, &row.schema).await;
        assert_eq!(
            answer.status, row.status,
            "case {}: expected HTTP {}, got {} with {:?}",
            row.case, row.status, answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            row.code,
            "case {}: expected {}, got {:?}",
            row.case,
            row.code,
            answer.body
        );
        let said = string_member(&answer.body, "message", row.case);
        if let Some(message) = row.message {
            assert_eq!(
                said, message,
                "case {}: expected the service's sentence for the class, got {said:?}",
                row.case
            );
        }
        if let Some(part) = row.names {
            assert!(
                said.contains(part),
                "case {}: expected the refusal to name {part:?}, got {said:?}",
                row.case
            );
        }

        // A refused update changes nothing: the index reads back as the create left it.
        let stored = read_schema(&emulator, "update-rules", &index, row.case).await;
        assert_eq!(
            stored, created,
            "case {}: a refused update must leave the stored schema alone, got {stored:?}",
            row.case
        );
    }

    // And the mapping an accepted update carries still lands, so the refusals above are
    // about what those documents named rather than about updating at all. Every field type
    // the engine's catalog publishes is one such document: a handful spanning its groups is
    // driven here, including types this issue's reports never named.
    for kind in ["keyword", "long", "date", "boolean", "geo_point", "knn_vector"] {
        assert!(
            MAPPING_FIELD_TYPES.contains(&kind),
            "case {kind}: the catalog the service publishes must hold the type this case drives"
        );
        let index = format!("accepted-{kind}");
        created_index(&emulator, "update-rules", &index, &created, kind).await;
        let update = json!({ "mappings": { "properties": { kind: { "type": kind } } } });
        updated_index(&emulator, "update-rules", &index, &update, kind).await;
        let stored = read_schema(&emulator, "update-rules", &index, kind).await;
        assert_eq!(
            stored["mappings"]["properties"][kind]["type"],
            json!(kind),
            "case {kind}: expected the accepted mapping in the stored schema, got {stored:?}"
        );
    }
}

/// RULE: the index-management operations are served by a domain whose engine serves them. A
/// domain running an engine that does not is refused `ValidationException` with the
/// service's software-version sentence, and that refusal comes BEFORE the index lookup — so
/// a request naming an index the domain never held earns the gate rather than a not-found.
#[tokio::test]
async fn the_index_operations_are_served_only_by_an_engine_that_carries_them() {
    let emulator = Emulator::new();

    // The rows differ along the engine the domain runs: the two engine types the service
    // offers, and releases of the supporting one on either side of the bound these
    // operations arrived in.
    let bound = state::upgrade_rank(INDEX_MANAGEMENT_ENGINE_VERSION);
    let rows: Vec<(&str, &str, bool)> = state::published_engine_versions()
        .into_iter()
        .filter(|version| {
            [
                "Elasticsearch_7.10",
                "Elasticsearch_1.5",
                "OpenSearch_1.3",
                INDEX_MANAGEMENT_ENGINE_VERSION,
                "OpenSearch_2.11",
            ]
            .contains(version)
        })
        .map(|version| {
            let served = state::upgrade_rank(version) >= bound
                && state::engine_type_of(version) == Some(state::ENGINE_TYPE_OPENSEARCH);
            (version, version, served)
        })
        .collect();
    assert!(
        rows.iter().any(|(_, _, served)| *served) && rows.iter().any(|(_, _, served)| !*served),
        "the table must span engines that serve these operations and engines that do not, got \
         {rows:?}"
    );

    for (at, (case, version, served)) in rows.iter().enumerate() {
        let domain = format!("gated-domain-{at}");
        let answer = emulator
            .create(json!({ "DomainName": domain, "EngineVersion": version }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the domain to be created, got {} with {:?}",
            answer.status, answer.body
        );
        settled_domain(&emulator, &domain).await;

        let schema = json!({ "mappings": { "properties": { "title": { "type": "text" } } } });
        let created = create_index(
            &emulator,
            &domain,
            json!({ "IndexName": "gated-index", "IndexSchema": schema }),
        )
        .await;
        if *served {
            assert_eq!(
                created.status, 200,
                "case {case}: expected the create to be served, got {} with {:?}",
                created.status, created.body
            );
            let read = get_index(&emulator, &domain, "gated-index").await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected the read to be served, got {} with {:?}",
                read.status, read.body
            );
            continue;
        }

        // Every one of the four operations is refused alike — the read and the delete of an
        // index this domain never held among them, which is what shows the gate is answered
        // before the index is looked up.
        let refusals = [
            ("CreateIndex", created),
            (
                "GetIndex",
                get_index(&emulator, &domain, "never-created").await,
            ),
            (
                "UpdateIndex",
                update_index(&emulator, &domain, "never-created", &schema).await,
            ),
            (
                "DeleteIndex",
                delete_index(&emulator, &domain, "never-created").await,
            ),
        ];
        for (operation, answer) in refusals {
            assert_eq!(
                answer.status, 400,
                "case {case}/{operation}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                error_code(&answer),
                "ValidationException",
                "case {case}/{operation}: expected ValidationException, got {:?}",
                answer.body
            );
            assert_eq!(
                string_member(&answer.body, "message", case),
                state::INDEX_MANAGEMENT_UNSUPPORTED,
                "case {case}/{operation}: expected the gate's own sentence, got {:?}",
                answer.body
            );
        }
    }
}

/// RULE: the sentence that gate is refused with is ONE sentence
/// ([`state::INDEX_MANAGEMENT_UNSUPPORTED`]) — the feature the operations configure, named
/// as unsupported on the domain's software — for EVERY engine version the gate covers and
/// for both operations a caller reaches it through, and it is answered before the request's
/// own `IndexSchema` is judged.
///
/// The rows differ along the engine: both engine types the service offers, several majors of
/// each, and the two releases either side of the bound the feature arrives at — each read off
/// the published catalog rather than written out. What a gated call carries is varied too: a
/// schema the operations accept, and one whose settings section the serving releases refuse
/// with a sentence of their own — so what the rule pins is that the gate speaks first and
/// says one thing.
#[tokio::test]
async fn the_version_gate_states_one_sentence_for_every_engine_it_gates() {
    let emulator = Emulator::new();

    let bound = state::upgrade_rank(INDEX_MANAGEMENT_ENGINE_VERSION);
    let gated: Vec<&str> = state::published_engine_versions()
        .into_iter()
        .filter(|version| {
            [
                "Elasticsearch_1.5",
                "Elasticsearch_7.10",
                "OpenSearch_1.0",
                "OpenSearch_2.11",
                "OpenSearch_2.19",
            ]
            .contains(version)
        })
        .collect();
    assert!(
        gated.iter().all(|version| state::upgrade_rank(version) < bound
            || state::engine_type_of(version) != Some(state::ENGINE_TYPE_OPENSEARCH)),
        "every row must name a release the feature is not offered on, got {gated:?}"
    );
    assert_eq!(
        gated.len(),
        5,
        "the table spans five published releases; a row that left the catalog must fail here \
         rather than disappear, got {gated:?}"
    );

    // The domains are created first and settled together, so no case waits on a span of its
    // own.
    for (at, version) in gated.iter().enumerate() {
        let domain = format!("one-sentence-{at}");
        let answer = emulator
            .create(json!({ "DomainName": domain, "EngineVersion": version }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {version}: expected the domain to be created, got {} with {:?}",
            answer.status, answer.body
        );
    }
    for (at, _) in gated.iter().enumerate() {
        settled_domain(&emulator, &format!("one-sentence-{at}")).await;
    }

    // The two schemas: one these operations accept on a serving release, and one whose
    // settings section they refuse there with their own sentence
    // ([`INDEX_SETTINGS_UPDATE_UNSUPPORTED`]), which is what shows the gate is answered
    // before the document is judged.
    let schemas = [
        (
            "a mapping these operations accept",
            json!({ "mappings": { "properties": { "title": { "type": "text" } } } }),
        ),
        (
            "a settings section a serving release refuses with its own sentence",
            json!({ "settings": { "index": { "number_of_shards": 1 } } }),
        ),
    ];

    for (at, version) in gated.iter().enumerate() {
        let domain = format!("one-sentence-{at}");
        for (shape, schema) in &schemas {
            let answers = [
                (
                    "UpdateIndex",
                    update_index(&emulator, &domain, "gate-index", schema).await,
                ),
                ("GetIndex", get_index(&emulator, &domain, "gate-index").await),
            ];
            for (operation, answer) in answers {
                let case = format!("{version} / {shape} / {operation}");
                assert_eq!(
                    answer.status, 400,
                    "case {case}: expected HTTP 400, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    error_code(&answer),
                    "ValidationException",
                    "case {case}: expected ValidationException, got {:?}",
                    answer.body
                );
                assert_eq!(
                    string_member(&answer.body, "message", &case),
                    state::INDEX_MANAGEMENT_UNSUPPORTED,
                    "case {case}: expected the one gate sentence, got {:?}",
                    answer.body
                );
                assert_ne!(
                    string_member(&answer.body, "message", &case),
                    INDEX_SETTINGS_UPDATE_UNSUPPORTED,
                    "case {case}: the gate speaks before the document is judged"
                );
            }
        }
    }

    // On a release that DOES offer the feature the same calls are past the gate: the read of
    // an index nothing created is the missing index, and the write that carries a settings
    // section earns the sentence those operations state for it.
    let serving = "one-sentence-serving";
    let answer = emulator
        .create(json!({
            "DomainName": serving,
            "EngineVersion": INDEX_MANAGEMENT_ENGINE_VERSION,
        }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case the serving release: expected the domain to be created, got {} with {:?}",
        answer.status, answer.body
    );
    settled_domain(&emulator, serving).await;
    let read = get_index(&emulator, serving, "gate-index").await;
    assert_ne!(
        string_member(&read.body, "message", "the serving release / GetIndex"),
        state::INDEX_MANAGEMENT_UNSUPPORTED,
        "case the serving release / GetIndex: expected the gate to be past, got {:?}",
        read.body
    );
    created_index(
        &emulator,
        serving,
        "gate-index",
        &json!({ "mappings": { "properties": { "title": { "type": "text" } } } }),
        "the serving release",
    )
    .await;
    let settings_update = update_index(
        &emulator,
        serving,
        "gate-index",
        &json!({ "settings": { "index": { "number_of_shards": 1 } } }),
    )
    .await;
    assert_eq!(
        string_member(
            &settings_update.body,
            "message",
            "the serving release / UpdateIndex"
        ),
        INDEX_SETTINGS_UPDATE_UNSUPPORTED,
        "case the serving release / UpdateIndex: expected the sentence these operations state \
         for a settings section, got {:?}",
        settings_update.body
    );
}

// ---------------------------------------------------------------------------
// The NAME an index's analysis settings may define a component under
// ---------------------------------------------------------------------------

/// The status this service answers a dependency failure on: `DependencyFailureException` is
/// `@httpError(424)` (`smithy_query(query="shape",shape="DependencyFailureException")`), and
/// the code that shape carries on the wire.
const DEPENDENCY_FAILURE_STATUS: u16 = 424;
const DEPENDENCY_FAILURE: &str = "DependencyFailureException";

/// The component names the rows below write, one per script: Han, a Latin letter carrying a
/// diacritic, Cyrillic, Greek and a name that is ASCII everywhere but one character — so the
/// rule is asserted over the name's characters rather than over one value, the almost
/// admissible value included.
const NON_ASCII_NAMES: [(&str, &str); 5] = [
    ("Han characters", "分析器"),
    ("a Latin letter with a diacritic", "analyseur_français"),
    ("Cyrillic characters", "анализатор"),
    ("Greek characters", "αναλυτής"),
    ("one non-ASCII character in an ASCII name", "s2r_analyzer_é"),
];

/// The ASCII name every control row defines its component under.
const ASCII_COMPONENT: &str = "s2r_custom_analyzer";

/// One kind of component an index's `analysis` settings define: the member they are defined
/// under, and a definition this engine can otherwise build, so a refusal is about the NAME.
struct ComponentKind {
    case: &'static str,
    member: &'static str,
    definition: Value,
}

/// Every kind of component those settings define, each with a definition built out of what
/// this engine registers.
fn component_kinds() -> Vec<ComponentKind> {
    vec![
        ComponentKind {
            case: "an analyzer",
            member: "analyzer",
            definition: json!({ "type": "custom", "tokenizer": "standard" }),
        },
        ComponentKind {
            case: "a token filter",
            member: "filter",
            definition: json!({ "type": "lowercase" }),
        },
        ComponentKind {
            case: "a tokenizer",
            member: "tokenizer",
            definition: json!({ "type": "standard" }),
        },
        ComponentKind {
            case: "a character filter",
            member: "char_filter",
            definition: json!({ "type": "html_strip" }),
        },
    ]
}

/// Where in the document a row writes the analysis settings: the same settings reach the
/// cluster written under the `index` section inside the index's settings and written directly
/// under them, so a row names which spelling it uses and the rule holds for both.
#[derive(Debug, Clone, Copy)]
enum AnalysisSection {
    UnderIndex,
    DirectlyUnderSettings,
}

/// The schema a row sends: `member` defining one component under `name`, in `section`.
fn analysis_schema(
    section: AnalysisSection,
    member: &str,
    name: &str,
    definition: &Value,
) -> Value {
    let analysis = json!({ member: { name: definition } });
    match section {
        AnalysisSection::UnderIndex => {
            json!({ "settings": { "index": { "analysis": analysis } } })
        }
        AnalysisSection::DirectlyUnderSettings => json!({ "settings": { "analysis": analysis } }),
    }
}

/// Assert that `answer` is the dependency failure this service answers a schema it cannot
/// hand the domain's cluster with: the modelled shape, its status and the fixed sentence.
fn assert_dependency_failure(answer: &super::domain_lifecycle::Answer, case: &str) {
    assert_eq!(
        (answer.status, error_code(answer).as_str()),
        (DEPENDENCY_FAILURE_STATUS, DEPENDENCY_FAILURE),
        "case {case}: expected {DEPENDENCY_FAILURE_STATUS} {DEPENDENCY_FAILURE}, got {} with \
         {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        string_member(&answer.body, "message", case),
        state::DOMAIN_UNAVAILABLE,
        "case {case}: expected the service's dependency sentence, got {:?}",
        answer.body
    );
}

/// RULE: an analysis component is addressed by the NAME its settings define it under, and a
/// name carrying a character outside ASCII is one the domain's cluster cannot be handed — so
/// a `CreateIndex` whose settings define one is refused with the modelled
/// `DependencyFailureException` and its fixed sentence, is refused again on a retry, and
/// creates no index.
#[tokio::test]
async fn a_create_defining_a_component_under_a_non_ascii_name_is_refused_and_creates_nothing() {
    let emulator = Emulator::new();
    let domain = "analysis-names";
    created_domain(&emulator, domain).await;

    for (kind_index, kind) in component_kinds().into_iter().enumerate() {
        for (script_index, (script, name)) in NON_ASCII_NAMES.iter().enumerate() {
            for (section_index, section) in [
                AnalysisSection::UnderIndex,
                AnalysisSection::DirectlyUnderSettings,
            ]
            .into_iter()
            .enumerate()
            {
                let case = format!("{} named with {script} in {section:?}", kind.case);
                let index =
                    format!("analysis-names-{kind_index}-{script_index}-{section_index}");
                let schema = analysis_schema(section, kind.member, name, &kind.definition);
                let body = json!({ "IndexName": &index, "IndexSchema": &schema });

                // SUSTAINED: the same request is refused the same way twice, since nothing
                // about the decision depends on the call.
                for attempt in 0..2 {
                    let answer = create_index(&emulator, domain, body.clone()).await;
                    assert_dependency_failure(&answer, &format!("{case}, attempt {attempt}"));
                }

                // And it created nothing: the read of that index answers the missing resource
                // it is.
                let read = get_index(&emulator, domain, &index).await;
                assert_eq!(
                    (read.status, error_code(&read).as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case}: expected the refused create to leave no index, got {} with {:?}",
                    read.status,
                    read.body
                );
            }
        }
    }

    // The CONTROL: the same documents with an ASCII component name are served on the same
    // domain and read back verbatim, so the refusals above are about the name and nothing
    // else.
    for (kind_index, kind) in component_kinds().into_iter().enumerate() {
        let index = format!("analysis-names-ascii-{kind_index}");
        let schema = analysis_schema(
            AnalysisSection::UnderIndex,
            kind.member,
            ASCII_COMPONENT,
            &kind.definition,
        );
        created_index(&emulator, domain, &index, &schema, kind.case).await;
        let read = read_schema(&emulator, domain, &index, kind.case).await;
        assert_eq!(
            read, schema,
            "case {}: expected the stored schema to read back verbatim, got {read:?}",
            kind.case
        );
    }
}

/// RULE: the same name rule holds for the other index write — an `UpdateIndex` whose analysis
/// settings define a component under a non-ASCII name is refused with the same dependency
/// failure, and the stored schema is left exactly as it was.
#[tokio::test]
async fn an_update_defining_a_component_under_a_non_ascii_name_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let domain = "analysis-name-updates";
    created_domain(&emulator, domain).await;

    let index = "analysis-name-updates-index";
    let stored = json!({
        "settings": { "index": { "analysis": { "analyzer": { ASCII_COMPONENT: {
            "type": "custom",
            "tokenizer": "standard",
        } } } } },
        "mappings": { "properties": { "title": { "type": "text", "analyzer": ASCII_COMPONENT } } },
    });
    created_index(&emulator, domain, index, &stored, "the stored index").await;

    for kind in component_kinds() {
        for (script, name) in NON_ASCII_NAMES {
            let case = format!("an update defining {} named with {script}", kind.case);
            // An update writes an index's MAPPINGS, so the analysis settings it carries are
            // written where no settings section stands — the spelling this operation reads
            // them in.
            let schema = json!({ "analysis": { kind.member: { name: &kind.definition } } });
            let answer = update_index(&emulator, domain, index, &schema).await;
            assert_dependency_failure(&answer, &case);

            let read = read_schema(&emulator, domain, index, &case).await;
            assert_eq!(
                read, stored,
                "case {case}: expected the refused update to leave the stored schema as it was, \
                 got {read:?}"
            );
        }
    }

    // The CONTROL: the same update with an ASCII component name is served.
    let accepted = json!({ "analysis": { "filter": { "s2r_lowercase": { "type": "lowercase" } } } });
    let answer = update_index(&emulator, domain, index, &accepted).await;
    assert_eq!(
        answer.status, 200,
        "case an ASCII component name: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}
