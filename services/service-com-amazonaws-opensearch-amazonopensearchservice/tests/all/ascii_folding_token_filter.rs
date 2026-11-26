// SPDX-License-Identifier: CC0-1.0

//! The TOKEN FILTERS a chain may name WITHOUT the index defining them — the ones the engine
//! ships — driven over the real wire path: a signed request to the endpoint a domain
//! publishes, through the multi-service router, into the mounted service.
//!
//! The rule, over the set of texts one chain is run on:
//!
//! > a chain may name a token filter the engine SHIPS without the index defining it, and
//! > `asciifolding` is one of them: it replaces each character outside Basic Latin by the
//! > ASCII characters that character stands for and leaves one standing for none as it is — so
//! > an index that chains it reports the folded terms from the analyze API and matches an
//! > accented spelling and its folded one alike; a filter name the engine ships nothing for is
//! > still refused where the index is created.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::{error_type, rest_prefixed};

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "folding-domain";

/// The index whose own analysis settings chain the built-in filter, and the analyzer it
/// defines.
const INDEX: &str = "folded";
const ANALYZER: &str = "folded_text";

/// `CreateDomain`, then the index whose custom analyzer chains `lowercase`, `asciifolding` and
/// a `stop` filter the index defines itself — the shape a client builds an accent-insensitive
/// index with.
async fn given_folding_index(emulator: &Emulator) -> Answer {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    rest_prefixed(
        emulator,
        DOMAIN,
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({
            "settings": {
                "number_of_shards": 1,
                "number_of_replicas": 0,
                "analysis": {
                    "filter": {
                        "house_stop": { "type": "stop", "stopwords": ["the"] },
                    },
                    "analyzer": {
                        ANALYZER: {
                            "type": "custom",
                            "tokenizer": "standard",
                            "filter": ["lowercase", "asciifolding", "house_stop"],
                        },
                    },
                },
            },
            "mappings": { "properties": {
                "title": { "type": "text", "analyzer": ANALYZER },
            } },
        })),
    )
    .await
}

/// The token texts an answer of the analyze API reports, in the order it reports them.
fn tokens(answer: &Answer) -> Vec<String> {
    answer.body["tokens"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|token| token["token"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// `POST /{INDEX}/_analyze` through the chain the index defines.
async fn analyzed(emulator: &Emulator, body: Value) -> Answer {
    rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{INDEX}/_analyze"),
        Some(body),
    )
    .await
}

/// RULE: the index is CREATED for a chain naming the built-in filter, and the chain folds each
/// character outside Basic Latin to the ASCII characters it stands for.
#[tokio::test]
async fn a_chain_naming_the_built_in_folding_filter_is_created_and_folds_its_tokens() {
    let emulator = Emulator::new();
    let created = given_folding_index(&emulator).await;
    assert_eq!(
        (created.status, created.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case \"PUT /{INDEX} with a chain naming asciifolding\": expected an acknowledged \
         create, got {} with {:?}",
        created.status,
        created.body
    );

    // (case, the text, the terms the chain leaves of it) — rows spanning the forms the rule
    // covers: a precomposed accented letter, a letter whose ASCII equivalent is several
    // letters, a ligature, a compatibility form, a text already inside Basic Latin, a script
    // standing for no ASCII at all, and the word the index's own `stop` filter drops.
    let rows: [(&str, &str, Vec<&str>); 7] = [
        ("one accented letter", "Café", vec!["cafe"]),
        (
            "accents over several words",
            "Dénoûment Piñata",
            vec!["denoument", "pinata"],
        ),
        ("a letter standing for two", "Straße", vec!["strasse"]),
        ("a ligature of its own", "Æon", vec!["aeon"]),
        ("a compatibility form", "ﬁnance", vec!["finance"]),
        ("a text already inside Basic Latin", "Plain Report", vec!["plain", "report"]),
        (
            "a script no ASCII character stands for, beside a dropped stop word",
            "the 日本語 café",
            vec!["日", "本", "語", "cafe"],
        ),
    ];
    for (case, text, expected) in rows {
        let answer = analyzed(&emulator, json!({ "analyzer": ANALYZER, "text": text })).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from _analyze, got {} with {:?}",
            answer.status, answer.body
        );
        let reported = tokens(&answer);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the terms {expected:?}, got {reported:?}"
        );
    }
}

/// RULE (the same chain, as a SEARCH): the terms the chain produces are what a `match` on a
/// field analyzed by it compares, so the accented spelling and the folded one find each other.
#[tokio::test]
async fn a_folded_field_matches_the_accented_spelling_and_the_folded_one_alike() {
    let emulator = Emulator::new();
    let created = given_folding_index(&emulator).await;
    assert_eq!(
        created.status, 200,
        "case \"PUT /{INDEX}\": expected 200, got {} with {:?}",
        created.status, created.body
    );
    for (id, title) in [("d1", "Le Café Moderne"), ("d2", "Plain Diner")] {
        let written = rest_prefixed(
            &emulator,
            DOMAIN,
            "PUT",
            &format!("/{INDEX}/_doc/{id}"),
            Some(json!({ "title": title })),
        )
        .await;
        assert!(
            written.status == 200 || written.status == 201,
            "case \"write {INDEX}/{id}\": expected 200 or 201, got {} with {:?}",
            written.status,
            written.body
        );
    }

    // (case, the term the query asks about, the ids it must find)
    let rows: [(&str, &str, Vec<&str>); 4] = [
        ("the folded spelling of an accented term", "cafe", vec!["d1"]),
        ("the accented spelling itself", "café", vec!["d1"]),
        ("the accented spelling in another case", "CAFÉ", vec!["d1"]),
        ("a term no document carries", "bistro", vec![]),
    ];
    for (case, term, expected) in rows {
        let answer = rest_prefixed(
            &emulator,
            DOMAIN,
            "POST",
            &format!("/{INDEX}/_search"),
            Some(json!({ "query": { "match": { "title": term } } })),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let found: Vec<String> = answer.body["hits"]["hits"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|hit| hit["_id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            found, expected,
            "case {case:?}: expected the hits {expected:?}, got {found:?}"
        );
    }
}

/// RULE (the other side of the same rule): a filter the engine ships NOTHING for is still
/// refused where the index is created, so a chain that is built is one that really runs.
#[tokio::test]
async fn a_token_filter_the_engine_ships_nothing_for_is_still_refused() {
    let emulator = Emulator::new();
    let created = given_folding_index(&emulator).await;
    assert_eq!(
        created.status, 200,
        "case \"PUT /{INDEX}\": expected 200, got {} with {:?}",
        created.status, created.body
    );

    // (case, the filter the chain names, the name the refusal states)
    let rows: [(&str, &str); 2] = [
        ("a filter name nothing registers", "nonesuch"),
        ("a near-miss of the built-in one", "ascii_folding"),
    ];
    for (case, filter) in rows {
        let refused = rest_prefixed(
            &emulator,
            DOMAIN,
            "PUT",
            &format!("/{INDEX}-{filter}"),
            Some(json!({
                "settings": { "analysis": { "analyzer": { ANALYZER: {
                    "type": "custom",
                    "tokenizer": "standard",
                    "filter": ["lowercase", filter],
                } } } },
            })),
        )
        .await;
        assert_eq!(
            (refused.status, error_type(&refused)),
            (400, "illegal_argument_exception".to_string()),
            "case {case:?}: expected the engine's own refusal, got {} with {:?}",
            refused.status,
            refused.body
        );
        let reason = refused.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reason,
            format!("failed to find token filter under [{filter}]"),
            "case {case:?}: expected the engine's own reason, got {reason:?}"
        );
    }
}
