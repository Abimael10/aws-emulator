// SPDX-License-Identifier: CC0-1.0

//! What a PERCENT-ESCAPE in a path segment stands for over every ROLE a segment of the
//! domain's REST data plane carries a NAME in, driven over the real wire path: a signed
//! request to a domain of this service, through the multi-service router, into the mounted
//! service. What is asserted is what a client sees.
//!
//! The rule, one level above the document id a client indexes by:
//!
//! > a URI's path is a sequence of percent-encoded segments (RFC 3986 §3.3), so the value a
//! > segment carries is what its escapes STAND FOR and never the escape text. That holds for
//! > every name a segment of this surface carries — the document id, the index name, an
//! > alias, an index or component template's name, an ingest pipeline's id — so the cluster
//! > stores, reports, resolves and refuses under the DECODED name, and the escape text names
//! > a different resource, one the cluster does not hold.
//!
//! [`super`] reads that rule over which CHARACTER is escaped, on the document-id and
//! index-name segments. This suite reads it over the segment's ROLE instead: one row per kind
//! of name this surface takes in a path, each written under the escaped spelling, read back
//! out of the collection that holds it, and probed under the escape text as well. The escape
//! in each row but the document id's is of a character that role's own naming rule admits, so
//! what the row reads is the decoding and not the naming rule; the document id's is the
//! separator of a hierarchical key, which a client cannot send unescaped. A snapshot
//! repository's name is the one path-named resource left out: registering one needs the object
//! store the repository lives in, which [`crate::snapshot_repositories`] already stands up, and
//! that name reaches state through the same reading of the path every row here drives.
//!
//! Asserted beside it, because it is the same reading of the same segment: a naming rule is
//! applied to the DECODED value, so an index name whose escapes spell a character
//! `IndexName`'s pattern forbids is refused quoting what they spell, and nothing is created.
//!
//! The harness is [`crate::domain_lifecycle`]'s, the data-plane request helper is
//! [`crate::domain_rest_data_plane`]'s and the escape-text spelling is [`super`]'s; this
//! suite adds no second copy of any of them.

use serde_json::{json, Value};

use crate::data_source_registry::created_domain;
use crate::domain_lifecycle::{Answer, Emulator};
use crate::domain_rest_data_plane::{error_type, rest_prefixed};
// The ONE spelling of the literal escape text a segment carries, from the suite that owns it.
use super::double_escaped;

/// The domain every request below is addressed to.
const DOMAIN: &str = "escape-roles";

/// The index the alias and document rows are written into.
const INDEX: &str = "ledger";

/// Which name a row's segment carries, which is what decides the route it is read through.
#[derive(Debug, Clone, Copy)]
enum Role {
    /// The index-name segment of an index create and read.
    Index,
    /// The alias segment of `PUT /{index}/_alias/{alias}`.
    Alias,
    /// The name segment of `PUT /_index_template/{name}`.
    IndexTemplate,
    /// The name segment of `PUT /_component_template/{name}`.
    ComponentTemplate,
    /// The id segment of `PUT /_ingest/pipeline/{id}`.
    IngestPipeline,
    /// The id segment of `PUT /_plugins/_ism/policies/{id}`, a plugin's own id space.
    IsmPolicy,
    /// The id segment of `PUT /{index}/_doc/{id}`.
    DocumentId,
}

/// One row of the table: a name as the client has to SEND it, and the name the cluster
/// therefore holds.
struct Named {
    case: &'static str,
    role: Role,
    /// The name as it travels in the path.
    sent: &'static str,
    /// What that segment's escapes stand for, which is the name the cluster holds.
    held: &'static str,
}

/// The names the rule is read over: one per role a segment names a resource in.
///
/// Every escape but the document id's encodes a character that role's own naming rule admits
/// (`-` and `.` inside an index name, from `IndexName`'s pattern, which the crate states as
/// `state::INDEX_NAME_PATTERN`), so the row reads the decoding alone.
fn rows() -> Vec<Named> {
    vec![
        Named {
            case: "the index-name segment",
            role: Role::Index,
            sent: "stock%2D2026",
            held: "stock-2026",
        },
        Named {
            case: "the alias segment of an index's alias write",
            role: Role::Alias,
            sent: "current%2Eledger",
            held: "current.ledger",
        },
        Named {
            case: "the name segment of an index template",
            role: Role::IndexTemplate,
            sent: "logs%2Dtemplate",
            held: "logs-template",
        },
        Named {
            case: "the name segment of a component template",
            role: Role::ComponentTemplate,
            sent: "base%2Dsettings",
            held: "base-settings",
        },
        Named {
            case: "the id segment of an ingest pipeline",
            role: Role::IngestPipeline,
            sent: "enrich%2Dkeys",
            held: "enrich-keys",
        },
        Named {
            case: "the id segment of an index-management policy, under a plugin's own prefix",
            role: Role::IsmPolicy,
            sent: "hot%2Dwarm",
            held: "hot-warm",
        },
        Named {
            case: "the id segment of a document keyed by an object key",
            role: Role::DocumentId,
            sent: "docs%2Fkey.json",
            held: "docs/key.json",
        },
    ]
}

/// One request on the domain's data plane, under the prefixed path addressing.
async fn plane(emulator: &Emulator, method: &str, path: &str, body: Option<Value>) -> Answer {
    rest_prefixed(emulator, DOMAIN, method, path, body).await
}

/// The schema a created index carries here: the smallest allocation, so a row's index costs
/// the store nothing it does not need.
fn index_schema() -> Value {
    json!({ "settings": { "index": { "number_of_shards": 1, "number_of_replicas": 0 } } })
}

/// The members of a JSON object, in the order it carries them.
fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default()
}

/// RULE: every segment of this surface that names a resource is read as the value its
/// escapes spell — the resource is created under that name, reported under it, and the escape
/// text names no resource the cluster holds.
#[tokio::test]
async fn every_segment_that_names_a_resource_is_read_as_the_value_its_escapes_spell() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    // The index the alias and document rows name.
    let made = plane(&emulator, "PUT", &format!("/{INDEX}"), Some(index_schema())).await;
    assert_eq!(
        made.status, 200,
        "case \"the index the rows are written into\": expected 200, got {} with {:?}",
        made.status, made.body
    );

    for row in rows() {
        match row.role {
            Role::Index => index_row(&emulator, &row).await,
            Role::Alias => alias_row(&emulator, &row).await,
            Role::IndexTemplate => index_template_row(&emulator, &row).await,
            Role::ComponentTemplate => component_template_row(&emulator, &row).await,
            Role::IngestPipeline => ingest_pipeline_row(&emulator, &row).await,
            Role::IsmPolicy => ism_policy_row(&emulator, &row).await,
            Role::DocumentId => document_id_row(&emulator, &row).await,
        }
    }
}

/// The index-name segment: the index is created under the decoded name, read back under it,
/// and the escape text names no index.
async fn index_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let created = plane(emulator, "PUT", &format!("/{sent}"), Some(index_schema())).await;
    assert_eq!(
        (created.status, created.body["index"].as_str()),
        (200, Some(held)),
        "case {case:?}: expected 200 naming index {held:?}, got {} with {:?}",
        created.status,
        created.body
    );

    let read = plane(emulator, "GET", &format!("/{sent}"), None).await;
    assert_eq!(
        (read.status, keys(&read.body)),
        (200, vec![held.to_string()]),
        "case {case:?}: expected the read to report index {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(emulator, "GET", &format!("/{}", double_escaped(sent)), None).await;
    assert_eq!(
        (absent.status, error_type(&absent).as_str()),
        (404, "index_not_found_exception"),
        "case {case:?}: expected the escape text of {sent:?} to name no index, got {} with {:?}",
        absent.status,
        absent.body
    );
}

/// The alias segment: the alias the index carries is the decoded name, and a removal of the
/// escape text names no alias of that index.
async fn alias_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let written = plane(
        emulator,
        "PUT",
        &format!("/{INDEX}/_alias/{sent}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (written.status, written.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case {case:?}: expected the alias write to be acknowledged, got {} with {:?}",
        written.status,
        written.body
    );

    let listed = plane(emulator, "GET", &format!("/{INDEX}/_alias"), None).await;
    assert_eq!(
        (listed.status, keys(&listed.body[INDEX]["aliases"])),
        (200, vec![held.to_string()]),
        "case {case:?}: expected the index to carry alias {held:?}, got {} with {:?}",
        listed.status,
        listed.body
    );

    let removed = plane(
        emulator,
        "DELETE",
        &format!("/{INDEX}/_alias/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        (removed.status, error_type(&removed).as_str()),
        (404, "aliases_not_found_exception"),
        "case {case:?}: expected the escape text of {sent:?} to name no alias, got {} with {:?}",
        removed.status,
        removed.body
    );
}

/// The index-template name segment: the template is reported under the decoded name, and the
/// escape text names no template.
async fn index_template_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let written = plane(
        emulator,
        "PUT",
        &format!("/_index_template/{sent}"),
        Some(json!({
            "index_patterns": ["templated-escape-*"],
            "template": index_schema(),
        })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case {case:?}: expected the template write to be acknowledged, got {} with {:?}",
        written.status,
        written.body
    );

    let read = plane(emulator, "GET", &format!("/_index_template/{sent}"), None).await;
    assert_eq!(
        (read.status, read.body["index_templates"][0]["name"].as_str()),
        (200, Some(held)),
        "case {case:?}: expected the template to be reported as {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(
        emulator,
        "GET",
        &format!("/_index_template/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: expected the escape text of {sent:?} to name no template, got {} with \
         {:?}",
        absent.status, absent.body
    );
}

/// The component-template name segment: reported under the decoded name, and the escape text
/// names no component.
async fn component_template_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let written = plane(
        emulator,
        "PUT",
        &format!("/_component_template/{sent}"),
        Some(json!({ "template": index_schema() })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case {case:?}: expected the component write to be acknowledged, got {} with {:?}",
        written.status,
        written.body
    );

    let read = plane(emulator, "GET", &format!("/_component_template/{sent}"), None).await;
    assert_eq!(
        (
            read.status,
            read.body["component_templates"][0]["name"].as_str()
        ),
        (200, Some(held)),
        "case {case:?}: expected the component to be reported as {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(
        emulator,
        "GET",
        &format!("/_component_template/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: expected the escape text of {sent:?} to name no component, got {} with \
         {:?}",
        absent.status, absent.body
    );
}

/// The ingest-pipeline id segment: the pipeline is reported under the decoded id, and the
/// escape text names no pipeline.
async fn ingest_pipeline_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let written = plane(
        emulator,
        "PUT",
        &format!("/_ingest/pipeline/{sent}"),
        Some(json!({
            "description": "stamps the key a document was indexed under",
            "processors": [{ "set": { "field": "stamped", "value": "yes" } }],
        })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case {case:?}: expected the pipeline write to be acknowledged, got {} with {:?}",
        written.status,
        written.body
    );

    let read = plane(emulator, "GET", &format!("/_ingest/pipeline/{sent}"), None).await;
    assert_eq!(
        (read.status, keys(&read.body)),
        (200, vec![held.to_string()]),
        "case {case:?}: expected the pipeline to be reported as {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(
        emulator,
        "GET",
        &format!("/_ingest/pipeline/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        (absent.status, error_type(&absent).as_str()),
        (404, "resource_not_found_exception"),
        "case {case:?}: expected the escape text of {sent:?} to name no pipeline, got {} with \
         {:?}",
        absent.status,
        absent.body
    );
}

/// The id segment of a PLUGIN's own resource: the index-management policy is created under
/// the decoded id, reports it on the write and on the read, and the escape text names no
/// policy.
async fn ism_policy_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);
    let route = "/_plugins/_ism/policies";

    let written = plane(
        emulator,
        "PUT",
        &format!("{route}/{sent}"),
        Some(json!({
            "policy": {
                "description": "moves an index out of hot storage",
                "default_state": "hot",
                "states": [{ "name": "hot", "actions": [], "transitions": [] }],
            }
        })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["_id"].as_str()),
        (201, Some(held)),
        "case {case:?}: expected 201 with _id {held:?}, got {} with {:?}",
        written.status,
        written.body
    );

    let read = plane(emulator, "GET", &format!("{route}/{sent}"), None).await;
    assert_eq!(
        (
            read.status,
            read.body["_id"].as_str(),
            read.body["policy"]["policy"]["policy_id"].as_str()
        ),
        (200, Some(held), Some(held)),
        "case {case:?}: expected the policy to be reported as {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(
        emulator,
        "GET",
        &format!("{route}/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        (absent.status, error_type(&absent).as_str()),
        (404, "status_exception"),
        "case {case:?}: expected the escape text of {sent:?} to name no policy, got {} with {:?}",
        absent.status,
        absent.body
    );
}

/// The document-id segment: the document is written and read under the decoded id, and the
/// escape text names no document.
async fn document_id_row(emulator: &Emulator, row: &Named) {
    let (case, sent, held) = (row.case, row.sent, row.held);

    let written = plane(
        emulator,
        "PUT",
        &format!("/{INDEX}/_doc/{sent}?refresh=true"),
        Some(json!({ "key": held })),
    )
    .await;
    assert_eq!(
        (written.status, written.body["_id"].as_str()),
        (201, Some(held)),
        "case {case:?}: expected 201 with _id {held:?}, got {} with {:?}",
        written.status,
        written.body
    );

    let read = plane(emulator, "GET", &format!("/{INDEX}/_doc/{sent}"), None).await;
    assert_eq!(
        (read.status, read.body["_id"].as_str()),
        (200, Some(held)),
        "case {case:?}: expected the document under {held:?}, got {} with {:?}",
        read.status,
        read.body
    );

    let absent = plane(
        emulator,
        "GET",
        &format!("/{INDEX}/_doc/{}", double_escaped(sent)),
        None,
    )
    .await;
    assert_eq!(
        (absent.status, absent.body["found"].as_bool()),
        (404, Some(false)),
        "case {case:?}: expected the escape text of {sent:?} to name no document, got {} with \
         {:?}",
        absent.status,
        absent.body
    );
}

/// RULE: a decoded alias name is resolved the way the index name it stands for is, so a
/// write, a read and a search addressed through the escaped alias all reach the index behind
/// it, and the refusal of a removal that names no alias quotes the decoded name.
#[tokio::test]
async fn an_escaped_alias_resolves_the_index_it_points_at_for_writes_reads_and_refusals() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    let index = "orders";
    let alias_sent = "sales%2Earchive";
    let alias_held = "sales.archive";
    let id_sent = "docs%2Forder-1.json";
    let id_held = "docs/order-1.json";

    let made = plane(&emulator, "PUT", &format!("/{index}"), Some(index_schema())).await;
    assert_eq!(
        made.status, 200,
        "case \"the index the alias points at\": expected 200, got {} with {:?}",
        made.status, made.body
    );
    let pointed = plane(
        &emulator,
        "PUT",
        &format!("/{index}/_alias/{alias_sent}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        pointed.status, 200,
        "case \"the alias write\": expected 200, got {} with {:?}",
        pointed.status, pointed.body
    );

    // A write addressed through the escaped alias lands in the index the decoded alias points
    // at, under the decoded id.
    let written = plane(
        &emulator,
        "PUT",
        &format!("/{alias_sent}/_doc/{id_sent}?refresh=true"),
        Some(json!({ "key": id_held })),
    )
    .await;
    assert_eq!(
        (
            written.status,
            written.body["_index"].as_str(),
            written.body["_id"].as_str()
        ),
        (201, Some(index), Some(id_held)),
        "case \"a write through the escaped alias\": expected 201 in {index:?} under \
         {id_held:?}, got {} with {:?}",
        written.status,
        written.body
    );

    // The index behind the alias answers that document by its decoded id.
    let read = plane(&emulator, "GET", &format!("/{index}/_doc/{id_sent}"), None).await;
    assert_eq!(
        (read.status, read.body["_id"].as_str()),
        (200, Some(id_held)),
        "case \"the read behind the alias\": expected the document under {id_held:?}, got {} \
         with {:?}",
        read.status,
        read.body
    );

    // And a search addressed through the escaped alias reports the same hit.
    let searched = plane(
        &emulator,
        "POST",
        &format!("/{alias_sent}/_search"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    let hits: Vec<(Option<&str>, Option<&str>)> = searched.body["hits"]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|hit| (hit["_index"].as_str(), hit["_id"].as_str()))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        (searched.status, hits),
        (200, vec![(Some(index), Some(id_held))]),
        "case \"a search through the escaped alias\": expected the one hit of {index:?} under \
         {id_held:?}, got {} with {:?}",
        searched.status,
        searched.body
    );

    // The removal names the decoded alias, so the second one is refused quoting that name
    // rather than the escape text.
    let removed = plane(
        &emulator,
        "DELETE",
        &format!("/{index}/_alias/{alias_sent}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "case \"the alias removal\": expected 200, got {} with {:?}",
        removed.status, removed.body
    );
    let again = plane(
        &emulator,
        "DELETE",
        &format!("/{index}/_alias/{alias_sent}"),
        None,
    )
    .await;
    assert_eq!(
        (
            again.status,
            error_type(&again).as_str(),
            again.body["error"]["reason"].as_str()
        ),
        (
            404,
            "aliases_not_found_exception",
            Some(format!("aliases [{alias_held}] missing").as_str())
        ),
        "case \"the second alias removal\": expected the missing-alias refusal quoting \
         {alias_held:?}, got {} with {:?}",
        again.status,
        again.body
    );
}

/// RULE: a naming rule is applied to what the segment's escapes SPELL and not to the escape
/// text — so an index name whose escapes spell a character `IndexName`'s pattern forbids is
/// refused quoting the decoded name, and no index is created under either spelling.
#[tokio::test]
async fn an_index_name_rule_is_applied_to_the_value_the_segment_spells() {
    let emulator = Emulator::new();
    created_domain(&emulator, DOMAIN).await;

    let sent = "docs%2Fledger";
    let held = "docs/ledger";

    let refused = plane(&emulator, "PUT", &format!("/{sent}"), Some(index_schema())).await;
    assert_eq!(
        (refused.status, error_type(&refused).as_str()),
        (400, "invalid_index_name_exception"),
        "case \"an index name whose escapes spell a forbidden character\": expected the \
         invalid-name refusal, got {} with {:?}",
        refused.status,
        refused.body
    );
    assert!(
        refused.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains(held),
        "case \"an index name whose escapes spell a forbidden character\": expected the reason \
         to quote {held:?}, got {:?}",
        refused.body
    );

    // Neither spelling names an index afterwards: the refusal created nothing.
    for (case, path) in [
        ("the decoded name", format!("/{sent}")),
        ("the escape text", format!("/{}", double_escaped(sent))),
    ] {
        let read = plane(&emulator, "GET", &path, None).await;
        assert_eq!(
            (read.status, error_type(&read).as_str()),
            (404, "index_not_found_exception"),
            "case {case:?}: expected no index to have been created, got {} with {:?}",
            read.status,
            read.body
        );
    }
}
