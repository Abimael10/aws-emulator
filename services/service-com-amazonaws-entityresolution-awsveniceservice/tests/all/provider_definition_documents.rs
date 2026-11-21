// SPDX-License-Identifier: CC0-1.0

//! The provider-service catalog as PUBLISHED data: that it enumerates every provider
//! service each provider publishes, and that each entry's definition documents carry
//! the per-property metadata a caller fills a `providerProperties` payload from.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `entityresolution` signing name, through the
//! harness the `schema_mapping_workflow_lifecycle` suite owns — so what is asserted is
//! what an AWS SDK client would see.
//!
//! The rules under test, each over the WHOLE catalog rather than one entry:
//!
//! * `ListProviderServices` reports one summary per published provider service, for
//!   every provider the service documents and every service each of them publishes, in
//!   the catalog's own stable `<providerName>/<providerServiceName>` order — so a
//!   positional read, a `providerName` filter and a paginated read all line up, and no
//!   provider or service the catalog does not publish appears.
//! * A definition document (`providerConfigurationDefinition`,
//!   `providerJobConfiguration`, `providerEntityOutputDefinition`) is a JSON-Schema
//!   descriptor: every property it declares carries its own published metadata — a
//!   `type`, what it is for, the length bounds and `pattern` its value must satisfy,
//!   and the fixed `value` where the catalog publishes one — and an object that
//!   requires properties names them in its own `required` list. A bare property name is
//!   not a descriptor.
//! * The metadata a property carries is the PUBLISHED catalog's own, served as the
//!   catalog holds it for whichever provider service is addressed: the prose is the
//!   provider's sentence for that property rather than one built from the property's
//!   name, an identifier's `pattern` is the provider's own expression — the literal
//!   prefixes its ids begin with included, where it publishes them — rather than a
//!   permissive character class, and every key the catalog publishes for a property, the
//!   per-property `required` flag included, reaches the wire with its own value.
//! * `ProviderMarketplaceConfiguration.listingId` is the id of the Data Exchange
//!   LISTING the provider service is subscribed through, which carries the fixed
//!   `prodview-` prefix, while the data set, revision and asset it resolves to are Data
//!   Exchange ids of their own.
//!
//! The recorded list-then-get round trip of one provider service is one row of these
//! tables.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde_json::Value;

use crate::schema_mapping_workflow_lifecycle::{list_member, string_member, Emulator, Reply, REGION};

/// One provider that populates the catalog, with the `SchemaAttributeType`s it accepts.
struct ProviderCase {
    /// The provider as the catalog names it: an `EntityName`
    /// (`^[a-zA-Z_0-9-]*$`), which is the token a `providerName` filter and a
    /// `providerServiceArn` carry.
    provider_name: &'static str,
    /// How the model spells the provider in prose, so a failure names the provider a
    /// reader recognises.
    documented_as: &'static str,
    /// Exactly the attribute types that provider is documented to support.
    attribute_types: &'static [&'static str],
}

/// The providers Entity Resolution integrates and the `SchemaAttributeType`s each of
/// them accepts, as the model documents them per provider in the doc of
/// `ProviderSchemaAttribute$type`
/// (`smithy_query(query="doc",shape="ProviderSchemaAttribute",member="type")`): LiveRamp,
/// TransUnion and Unified ID 2.0. The `provider_name` beside each is the catalog's own
/// `EntityName` token for that provider, which is what the wire carries.
const PROVIDERS: &[ProviderCase] = &[
    ProviderCase {
        provider_name: "liveramp",
        documented_as: "LiveRamp",
        attribute_types: &[
            "NAME",
            "NAME_FIRST",
            "NAME_MIDDLE",
            "NAME_LAST",
            "ADDRESS",
            "ADDRESS_STREET1",
            "ADDRESS_STREET2",
            "ADDRESS_STREET3",
            "ADDRESS_CITY",
            "ADDRESS_STATE",
            "ADDRESS_COUNTRY",
            "ADDRESS_POSTALCODE",
            "PHONE",
            "PHONE_NUMBER",
            "EMAIL_ADDRESS",
            "UNIQUE_ID",
            "PROVIDER_ID",
        ],
    },
    ProviderCase {
        provider_name: "transunion",
        documented_as: "TransUnion",
        attribute_types: &[
            "NAME",
            "NAME_FIRST",
            "NAME_LAST",
            "ADDRESS",
            "ADDRESS_CITY",
            "ADDRESS_STATE",
            "ADDRESS_COUNTRY",
            "ADDRESS_POSTALCODE",
            "PHONE_NUMBER",
            "EMAIL_ADDRESS",
            "UNIQUE_ID",
            "DATE",
            "IPV4",
            "IPV6",
            "MAID",
        ],
    },
    ProviderCase {
        provider_name: "uid2",
        documented_as: "Unified ID 2.0",
        attribute_types: &["PHONE_NUMBER", "EMAIL_ADDRESS", "UNIQUE_ID"],
    },
];

/// The three definition documents of an entry, each modelled as a `Document`. The
/// entity output definition is modelled @required; the other two are published only by
/// an entry that takes a configuration or a job option.
const DEFINITION_DOCUMENTS: [&str; 3] = [
    "providerConfigurationDefinition",
    "providerJobConfiguration",
    "providerEntityOutputDefinition",
];

/// The fixed prefix a Data Exchange listing id carries, which is what a client matches
/// to follow a provider service to its listing.
const LISTING_ID_PREFIX: &str = "prodview-";

/// Is `value` a Data Exchange listing id — the fixed prefix and a non-empty id of the
/// characters such an id is published with?
pub(crate) fn is_listing_id(value: &str) -> bool {
    value.strip_prefix(LISTING_ID_PREFIX).is_some_and(|id| {
        !id.is_empty() && id.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    })
}

/// The modelled `maxResults` minimum for `ListProviderServices`: `range 15..25`
/// (`smithy_query(query="operation",operation="ListProviderServices")`), so this is the
/// smallest page a caller can ask for.
const MAX_RESULTS_MIN: i64 = 15;

/// `GetProviderService`'s `providerServiceName` `@httpLabel` is a `ProviderServiceArn`,
/// and a non-greedy label is ONE path segment, so the `/`s inside the ARN ride
/// percent-encoded — which is what an AWS SDK sends.
fn label(arn: &str) -> String {
    arn.replace('/', "%2F")
}

async fn list(emulator: &Emulator, query: &str) -> Reply {
    emulator
        .call_in_region(REGION, "GET", &format!("/providerservices{query}"), None)
        .await
}

async fn get(emulator: &Emulator, provider_name: &str, arn: &str) -> Reply {
    emulator
        .call_in_region(
            REGION,
            "GET",
            &format!("/providerservices/{provider_name}/{}", label(arn)),
            None,
        )
        .await
}

/// The summaries of a page the rule says succeeds.
fn summaries(case: &str, reply: &Reply) -> Vec<Value> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: ListProviderServices must answer 200, got {reply:?}"
    );
    list_member(&reply.body, "providerServiceSummaries").clone()
}

/// The catalog keys of a page, in the order the page returned them.
fn keys(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            format!(
                "{}/{}",
                string_member(entry, "providerName"),
                string_member(entry, "providerServiceName")
            )
        })
        .collect()
}

/// One member of an object, which the rule says is present.
fn member<'a>(case: &str, record: &'a Value, name: &str) -> &'a Value {
    record
        .get(name)
        .filter(|value| !value.is_null())
        .unwrap_or_else(|| panic!("case {case:?}: member `{name}` is missing from {record:?}"))
}

/// The whole catalog, as summaries, with every entry's detail record beside it.
async fn catalog(emulator: &Emulator) -> Vec<(String, Value, Value)> {
    let listed = summaries("the whole catalog", &list(emulator, "").await);
    let mut entries = Vec::new();
    for summary in listed {
        let case = format!(
            "{}/{}",
            string_member(&summary, "providerName"),
            string_member(&summary, "providerServiceName")
        );
        let read = get(
            emulator,
            &string_member(&summary, "providerName"),
            &string_member(&summary, "providerServiceArn"),
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetProviderService must answer the entry the list named, got {read:?}"
        );
        entries.push((case, summary, read.body));
    }
    assert!(
        !entries.is_empty(),
        "the Region's provider-service catalog must not be empty"
    );
    entries
}

/// Every provider the service documents populates the catalog, every entry belongs to
/// one of them with attributes that provider accepts, and the whole catalog is reported
/// in its own stable key order however it is read.
#[tokio::test]
async fn every_documented_provider_populates_the_catalog_in_key_order() {
    let emulator = Emulator::new();
    let listed = summaries("the whole catalog", &list(&emulator, "").await);
    let all_keys = keys(&listed);

    // The catalog's own order: ascending by `<providerName>/<providerServiceName>`,
    // with no entry reported twice, so a positional read means the same thing on every
    // call.
    let mut sorted = all_keys.clone();
    sorted.sort();
    assert_eq!(
        all_keys, sorted,
        "the catalog must be reported in ascending key order, got {all_keys:?}"
    );
    let unique: BTreeSet<&String> = all_keys.iter().collect();
    assert_eq!(
        unique.len(),
        all_keys.len(),
        "no provider service may be reported twice, got {all_keys:?}"
    );

    // Which providers are present, and with how many services each.
    let mut by_provider: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for summary in &listed {
        by_provider
            .entry(string_member(summary, "providerName"))
            .or_default()
            .push(string_member(summary, "providerServiceName"));
    }

    for provider in PROVIDERS {
        let case = provider.documented_as;
        let services = by_provider.get(provider.provider_name).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the catalog must publish the provider services of \
                 {:?}, got {by_provider:?}",
                provider.provider_name
            )
        });
        assert!(
            !services.is_empty(),
            "case {case:?}: a provider in the catalog publishes at least one service, got \
             {by_provider:?}"
        );
    }

    let documented: BTreeSet<&str> = PROVIDERS
        .iter()
        .map(|provider| provider.provider_name)
        .collect();
    for provider_name in by_provider.keys() {
        assert!(
            documented.contains(provider_name.as_str()),
            "the catalog must publish only providers this service integrates, got \
             {provider_name:?}"
        );
    }

    // Every entry's input schema stays inside the attribute types its own provider
    // accepts.
    for (case, summary, read) in catalog(&emulator).await {
        let provider_name = string_member(&summary, "providerName");
        let provider = PROVIDERS
            .iter()
            .find(|provider| provider.provider_name == provider_name)
            .unwrap_or_else(|| panic!("case {case:?}: {provider_name:?} is an integrated provider"));
        let accepted: BTreeSet<&str> = provider.attribute_types.iter().copied().collect();
        for attribute in list_member(member(&case, &read, "providerComponentSchema"), "providerSchemaAttributes") {
            let attribute_type = string_member(attribute, "type");
            assert!(
                accepted.contains(attribute_type.as_str()),
                "case {case:?}: {} accepts {accepted:?}, so a schema attribute of type \
                 {attribute_type:?} is not one of its own",
                provider.documented_as
            );
        }
    }

    // However the catalog is paged, the sequence is the same one: the smallest page a
    // caller may ask for, followed from token to token, yields exactly the unfiltered
    // list, and so does the union of the per-provider filters.
    let mut paged = Vec::new();
    let mut pages = 0;
    let mut query = format!("?maxResults={MAX_RESULTS_MIN}");
    loop {
        let reply = list(&emulator, &query).await;
        let page = summaries("paged", &reply);
        paged.extend(keys(&page));
        pages += 1;
        match reply.body.get("nextToken").and_then(Value::as_str) {
            Some(token) => {
                query = format!("?maxResults={MAX_RESULTS_MIN}&nextToken={token}");
            }
            None => break,
        }
    }
    assert_eq!(
        paged, all_keys,
        "paging must walk the catalog's own order exactly once, got {paged:?}"
    );
    // A page bound at or above the catalog's size leaves nothing out, so the walk above
    // ends in one page carrying no continuation token: the pages a bound produces is
    // what the catalog's own size decides, never more.
    let expected_pages = all_keys
        .len()
        .div_ceil(usize::try_from(MAX_RESULTS_MIN).expect("a positive page bound"))
        .max(1);
    assert_eq!(
        pages, expected_pages,
        "a catalog of {} entries is served in {expected_pages} page(s) of at most \
         {MAX_RESULTS_MIN}, got {pages}",
        all_keys.len()
    );

    let mut filtered = Vec::new();
    for provider_name in by_provider.keys() {
        let page = summaries(
            provider_name,
            &list(&emulator, &format!("?providerName={provider_name}")).await,
        );
        filtered.extend(keys(&page));
    }
    assert_eq!(
        filtered, all_keys,
        "the per-provider filters together must cover the whole catalog, got {filtered:?}"
    );
}

/// Is `pattern` anchored at both ends, so a caller validating a candidate value with it
/// accepts only a WHOLE value?
///
/// A published pattern anchors its end with `$`, and its start either directly with `^`
/// or — where the value may begin with any of several literal prefixes — inside every
/// branch of a LEADING alternation, which is how a provider publishes the prefixes its
/// identifiers can start with. An alternation anywhere else would let the expression
/// match mid-value, so it is not anchoring.
fn is_anchored(pattern: &str) -> bool {
    if !pattern.ends_with('$') {
        return false;
    }
    if pattern.starts_with('^') {
        return true;
    }
    let Some((alternation, rest)) = pattern
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
    else {
        return false;
    };
    !alternation.is_empty()
        && alternation.split('|').all(|branch| branch.starts_with('^'))
        && !rest.contains('|')
}

/// What a descriptor may declare as the `type` of the value that goes in a property.
const PROPERTY_TYPES: [&str; 3] = ["string", "boolean", "object"];

/// Which metadata a definition document published somewhere in the catalog, so that
/// "every property carries its metadata" cannot be satisfied by publishing none of it.
#[derive(Default, Debug)]
struct MetadataSeen {
    description: bool,
    length: bool,
    pattern: bool,
    value: bool,
    required: bool,
}

/// Walks one JSON-Schema object of a definition document: every property it declares
/// carries its own published metadata, and a `required` list names only properties the
/// object declares.
fn check_schema_object(case: &str, path: &str, schema: &Value, seen: &mut MetadataSeen) {
    let properties = member(case, schema, "properties")
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: {path} declares `properties`, got {schema:?}"))
        .clone();
    assert!(
        !properties.is_empty(),
        "case {case:?}: {path} declares at least one property, got {schema:?}"
    );

    if let Some(required) = schema.get("required").filter(|value| !value.is_null()) {
        let names = required
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: {path}.required is a list, got {required:?}"));
        assert!(
            !names.is_empty(),
            "case {case:?}: {path} carries no `required` list rather than an empty one"
        );
        for name in names {
            let name = name
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: {path}.required names a property, got {name:?}"));
            assert!(
                properties.contains_key(name),
                "case {case:?}: {path} requires {name:?}, which it must also declare, got \
                 {schema:?}"
            );
        }
        seen.required = true;
    }

    for (name, property) in &properties {
        let property_path = format!("{path}.properties.{name}");
        let value_type = string_member(property, "type");
        assert!(
            PROPERTY_TYPES.contains(&value_type.as_str()),
            "case {case:?}: {property_path} declares the type of its value, got {value_type:?}"
        );
        if value_type == "object" {
            // A property that holds an object is itself a schema object, so the same
            // rule applies to the properties under it.
            check_schema_object(case, &property_path, property, seen);
            continue;
        }

        let description = string_member(property, "description");
        assert!(
            !description.is_empty(),
            "case {case:?}: {property_path} must say what it is for, got {property:?}"
        );
        seen.description = true;

        let min_length = property.get("minLength").filter(|value| !value.is_null());
        let max_length = property.get("maxLength").filter(|value| !value.is_null());
        assert_eq!(
            min_length.is_some(),
            max_length.is_some(),
            "case {case:?}: {property_path} publishes both of its length bounds or neither, got \
             {property:?}"
        );
        if let (Some(min_length), Some(max_length)) = (min_length, max_length) {
            let min_length = min_length.as_u64().unwrap_or_else(|| {
                panic!("case {case:?}: {property_path}.minLength is a length, got {min_length:?}")
            });
            let max_length = max_length.as_u64().unwrap_or_else(|| {
                panic!("case {case:?}: {property_path}.maxLength is a length, got {max_length:?}")
            });
            assert!(
                0 < min_length && min_length <= max_length,
                "case {case:?}: {property_path} bounds its value's length, got \
                 {min_length}..{max_length}"
            );
            seen.length = true;
        }

        if let Some(pattern) = property.get("pattern").filter(|value| !value.is_null()) {
            let pattern = pattern
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: {property_path}.pattern is a regular expression, got {pattern:?}"));
            assert!(
                is_anchored(pattern),
                "case {case:?}: {property_path}.pattern must match the whole value, got \
                 {pattern:?}"
            );
            seen.pattern = true;
        }

        if let Some(published) = property.get("value").filter(|value| !value.is_null()) {
            let matches_type = match value_type.as_str() {
                "boolean" => published.is_boolean(),
                _ => published.as_str().is_some_and(|text| !text.is_empty()),
            };
            assert!(
                matches_type,
                "case {case:?}: {property_path}.value has the property's own {value_type} type, \
                 got {published:?}"
            );
            seen.value = true;
        }
    }
}

/// Every property of every definition document carries its published metadata, and the
/// catalog as a whole publishes each kind of it.
#[tokio::test]
async fn definition_document_properties_carry_their_published_metadata() {
    let emulator = Emulator::new();
    let mut seen = MetadataSeen::default();
    let mut documents_checked = 0;

    for (case, _, read) in catalog(&emulator).await {
        for document_name in DEFINITION_DOCUMENTS {
            let Some(document) = read.get(document_name).filter(|value| !value.is_null()) else {
                // Only `providerEntityOutputDefinition` is modelled @required; the
                // other two are published by the entries that take a configuration or
                // a job option.
                assert_ne!(
                    document_name, "providerEntityOutputDefinition",
                    "case {case:?}: the entity output definition is modelled @required, got {read:?}"
                );
                continue;
            };
            assert_eq!(
                string_member(document, "type"),
                "object",
                "case {case:?}: {document_name} is a JSON Schema object, got {document:?}"
            );
            check_schema_object(&case, document_name, document, &mut seen);
            documents_checked += 1;
        }

        // The ID-namespace configuration definitions are schema objects of the same
        // kind, published by the entries whose service type has ID namespaces, so the
        // same rule holds for them.
        if let Some(namespace) = read
            .get("providerIdNameSpaceConfiguration")
            .filter(|value| !value.is_null())
        {
            for definition_name in [
                "providerSourceConfigurationDefinition",
                "providerTargetConfigurationDefinition",
            ] {
                let definition = member(&case, namespace, definition_name);
                check_schema_object(&case, definition_name, definition, &mut seen);
                documents_checked += 1;
            }
        }
    }

    assert!(
        documents_checked >= DEFINITION_DOCUMENTS.len(),
        "every definition document the catalog publishes must have been walked, got \
         {documents_checked} of them"
    );
    for (kind, published) in [
        ("description", seen.description),
        ("minLength/maxLength", seen.length),
        ("pattern", seen.pattern),
        ("value", seen.value),
        ("required", seen.required),
    ] {
        assert!(
            published,
            "the catalog must publish {kind} metadata somewhere in its definition documents, got \
             {seen:?}"
        );
    }
}

/// `providerEntityOutputDefinition` tells a caller which identifier the provider
/// guarantees in its output, and how to validate it.
#[tokio::test]
async fn the_entity_output_definition_names_the_identifier_the_provider_guarantees() {
    let emulator = Emulator::new();

    for (case, _, read) in catalog(&emulator).await {
        let definition = member(&case, &read, "providerEntityOutputDefinition");
        let required: Vec<String> = member(&case, definition, "required")
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: the definition's `required` is a list"))
            .iter()
            .map(|name| {
                name.as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a required name is a string"))
                    .to_string()
            })
            .collect();
        assert_eq!(
            required,
            vec!["entity".to_string()],
            "case {case:?}: the provider answers one `entity` per input record, got {definition:?}"
        );

        let entity = member(&case, member(&case, definition, "properties"), "entity");
        let identifiers = member(&case, entity, "properties")
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: `entity` declares its properties"));
        let guaranteed: Vec<String> = member(&case, entity, "required")
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: `entity` carries a `required` list"))
            .iter()
            .map(|name| {
                name.as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a required name is a string"))
                    .to_string()
            })
            .collect();
        assert!(
            !guaranteed.is_empty(),
            "case {case:?}: `entity` names the identifier the provider guarantees, got {entity:?}"
        );
        for name in &guaranteed {
            let identifier = identifiers.get(name).unwrap_or_else(|| {
                panic!("case {case:?}: `entity` requires {name:?}, so it also declares it")
            });
            // The metadata a caller validates the provider's answer with: what the
            // identifier is, how long it is and what it looks like.
            for metadata in ["description", "minLength", "maxLength", "pattern"] {
                assert!(
                    identifier
                        .get(metadata)
                        .is_some_and(|value| !value.is_null()),
                    "case {case:?}: the {name:?} identifier must publish its {metadata}, got \
                     {identifier:?}"
                );
            }
        }
    }
}

/// The marketplace configuration reports the Data Exchange LISTING the provider service
/// is subscribed through, beside the data set, revision and asset that listing resolves
/// to.
#[tokio::test]
async fn the_marketplace_configuration_reports_the_listing_it_is_subscribed_through() {
    let emulator = Emulator::new();
    let mut listings = BTreeSet::new();

    for (case, _, read) in catalog(&emulator).await {
        let marketplace = member(
            &case,
            member(&case, &read, "providerEndpointConfiguration"),
            "marketplaceConfiguration",
        );
        let listing_id = string_member(marketplace, "listingId");
        assert!(
            is_listing_id(&listing_id),
            "case {case:?}: listingId must be the `{LISTING_ID_PREFIX}…` id of the provider \
             service's Data Exchange listing, got {listing_id:?}"
        );
        assert!(
            listings.insert(listing_id.clone()),
            "case {case:?}: each provider service is subscribed through its own listing, but \
             {listing_id:?} was already reported"
        );

        // The data set, revision and asset the subscription resolves to are Data
        // Exchange ids of their own, so none of them is the listing id over again.
        for id_member in ["dataSetId", "revisionId", "assetId"] {
            let id = string_member(marketplace, id_member);
            assert!(
                !id.is_empty() && id != listing_id && !is_listing_id(&id),
                "case {case:?}: {id_member} is a Data Exchange id of its own, got {id:?}"
            );
        }
    }

    assert!(
        listings.len() >= PROVIDERS.len(),
        "every published provider service reports its own listing, got {listings:?}"
    );
}

/// How the published catalog states that a `providerProperties` payload must carry a
/// property, which is what the served descriptor has to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Requiredness {
    /// The catalog publishes no requiredness: neither the declaring object's `required`
    /// list nor a `required` key on the property's own descriptor.
    Unpublished,
    /// The declaring object names the property in its own JSON Schema `required` list.
    Listed,
    /// The property's own descriptor carries a `required` flag with this value — which
    /// is a KEY a caller reads, so `false` is published rather than left out.
    Flag(bool),
}

/// One property of one entry's definition documents, with the metadata the published
/// catalog carries for it.
///
/// Source: the AWS-managed provider-service catalog as this emulator seeds it
/// (`src/state/provider_catalog.rs`), which is what `GetProviderService` serves for the
/// addressed `providerName`/`providerServiceName`. These rows span the catalog rather
/// than one entry: three providers, all three definition documents, properties nested
/// one and two levels deep, string- and flag-valued ones, and each of the three ways
/// requiredness is published.
struct PropertyCase {
    provider_name: &'static str,
    provider_service_name: &'static str,
    /// Which definition document declares it.
    document: &'static str,
    /// The member names from the document down to the property.
    path: &'static [&'static str],
    /// The catalog's own prose for the property.
    description: &'static str,
    /// How the catalog publishes its requiredness.
    requiredness: Requiredness,
    /// The `pattern` the catalog publishes for its value, where this row pins the
    /// expression itself.
    pattern: Option<&'static str>,
    /// Values the published `pattern` must ACCEPT — one per form the provider mints.
    accepted: &'static [&'static str],
    /// Values it must REJECT, so a caller that validates locally refuses exactly what
    /// the provider would.
    rejected: &'static [&'static str],
}

/// The properties the rule is asserted over.
const PROPERTY_CASES: &[PropertyCase] = &[
    // LiveRamp's own identifier: its prose, and the alternation of the literal prefixes
    // a RampID begins with followed by the character class the rest is drawn from, so a
    // candidate carrying no such prefix is refused.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerEntityOutputDefinition",
        path: &["entity", "RAMPID"],
        description:
            "LiveRamp's universal identifier that is tied to devices in the LiveRamp Identity Graph",
        requiredness: Requiredness::Listed,
        pattern: Some("(^XY|^Xi|^Xc|^Xm|^hY)[a-zA-Z0-9_-]*$"),
        accepted: &["XYabc123", "Xi_9-Z", "Xc0", "Xm-A", "hYZZZ"],
        rejected: &["xyabc123", "ZZabc123", "Yh123", "abcXY", "XY abc", "X"],
    },
    // The same identifier published by another of that provider's services carries the
    // same catalog metadata, byte for byte.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "idmapping",
        document: "providerEntityOutputDefinition",
        path: &["entity", "RAMPID"],
        description:
            "LiveRamp's universal identifier that is tied to devices in the LiveRamp Identity Graph",
        requiredness: Requiredness::Listed,
        pattern: Some("(^XY|^Xi|^Xc|^Xm|^hY)[a-zA-Z0-9_-]*$"),
        accepted: &["XYabc123", "hY0-_Z"],
        rejected: &["QQabc123", "xyabc123"],
    },
    PropertyCase {
        provider_name: "transunion",
        provider_service_name: "assignment",
        document: "providerEntityOutputDefinition",
        path: &["entity", "TRUAUDIENCE_ID"],
        description: "TransUnion's persistent identifier for the individual a record resolves to \
                      in the TruAudience identity graph",
        requiredness: Requiredness::Listed,
        pattern: Some("^[a-zA-Z0-9]+$"),
        accepted: &["A1b2C3", "0123456789abcdefABCDEF"],
        rejected: &["has-hyphen", "has_underscore", "has space", ""],
    },
    PropertyCase {
        provider_name: "uid2",
        provider_service_name: "assignment",
        document: "providerEntityOutputDefinition",
        path: &["entity", "UID2"],
        description: "Unified ID 2.0 token issued for the email address or phone number in a \
                      record",
        requiredness: Requiredness::Listed,
        pattern: Some("^[a-zA-Z0-9+/=]+$"),
        accepted: &["QWxhZGRpbjpvcGVuc2VzYW1l", "abcABC123+/="],
        rejected: &["under_score", "dash-dash", "with space"],
    },
    // The job knobs: the catalog's prose for each, and the `required` flag on the two it
    // publishes one for — `false`, the key present.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerJobConfiguration",
        path: &["data_format"],
        description: "Data format expected by Provider",
        requiredness: Requiredness::Unpublished,
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerJobConfiguration",
        path: &["include_source_id"],
        description: "Include UNIQUE_ID in the mappings for creating a job",
        requiredness: Requiredness::Flag(false),
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerJobConfiguration",
        path: &["prepare_job"],
        description: "Prepare a job before starting it",
        requiredness: Requiredness::Flag(false),
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
    // The same knobs on another service of that provider.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment-pii",
        document: "providerJobConfiguration",
        path: &["prepare_job"],
        description: "Prepare a job before starting it",
        requiredness: Requiredness::Flag(false),
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
    // Two levels deep: the property inside the job's intermediate-data location.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "idmapping",
        document: "providerJobConfiguration",
        path: &["intermediateSourceConfiguration", "intermediateS3Path"],
        description: "Amazon S3 path under which the intermediate data is written",
        requiredness: Requiredness::Listed,
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
    // The configuration document: a credential handed over as a Secrets Manager secret
    // ARN, whose published pattern accepts an ARN of that form and nothing else.
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerConfigurationDefinition",
        path: &["client_id"],
        description: "SecretsManagerclientid",
        requiredness: Requiredness::Listed,
        pattern: None,
        accepted: &["arn:aws:secretsmanager:us-east-1:123456789012:secret:liveramp/client-id"],
        rejected: &[
            "liveramp-client-id",
            "arn:aws:secretsmanager:us-east-1:12345:secret:short-account",
        ],
    },
    PropertyCase {
        provider_name: "liveramp",
        provider_service_name: "assignment",
        document: "providerConfigurationDefinition",
        path: &["resolution_flow"],
        description: "Providers resolution flow",
        requiredness: Requiredness::Unpublished,
        pattern: None,
        accepted: &[],
        rejected: &[],
    },
];

/// Walks `path` from a definition document down to one property, answering the object
/// that DECLARES the property beside the property's own descriptor.
fn descriptor<'a>(case: &str, document: &'a Value, path: &[&str]) -> (&'a Value, &'a Value) {
    let (name, parents) = path
        .split_last()
        .unwrap_or_else(|| panic!("case {case:?}: a property path names a property"));
    let mut declaring = document;
    for parent in parents {
        declaring = member(case, member(case, declaring, "properties"), parent);
    }
    let property = member(case, member(case, declaring, "properties"), name);
    (declaring, property)
}

/// How the served descriptor states the requiredness of the property `name` that
/// `declaring` declares.
fn served_requiredness(case: &str, declaring: &Value, property: &Value, name: &str) -> Requiredness {
    let listed = declaring
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|names| names.iter().any(|listed| listed.as_str() == Some(name)));
    let flag = property.get("required").filter(|value| !value.is_null());
    match (listed, flag) {
        (true, None) => Requiredness::Listed,
        (false, Some(flag)) => Requiredness::Flag(flag.as_bool().unwrap_or_else(|| {
            panic!("case {case:?}: a property's `required` key is a boolean, got {flag:?}")
        })),
        (false, None) => Requiredness::Unpublished,
        (true, Some(flag)) => panic!(
            "case {case:?}: {name:?} is both listed by its object and flagged {flag:?} on its own \
             descriptor"
        ),
    }
}

/// Every property's metadata is the published catalog's own for the addressed provider
/// service: its prose, its pattern and the way it publishes requiredness — nothing
/// derived from the property's name, and no published key dropped.
#[tokio::test]
async fn definition_property_metadata_is_the_published_catalogs_own() {
    let emulator = Emulator::new();
    let entries: BTreeMap<String, Value> = catalog(&emulator)
        .await
        .into_iter()
        .map(|(key, _, read)| (key, read))
        .collect();

    for expected in PROPERTY_CASES {
        let key = format!(
            "{}/{}",
            expected.provider_name, expected.provider_service_name
        );
        let read = entries.get(&key).unwrap_or_else(|| {
            panic!(
                "case {key:?}: the catalog publishes this provider service, got {:?}",
                entries.keys().collect::<Vec<_>>()
            )
        });
        let case = format!("{key} {}.{}", expected.document, expected.path.join("."));
        let document = member(&case, read, expected.document);
        let (declaring, property) = descriptor(&case, document, expected.path);
        let name = expected
            .path
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: a property path names a property"));

        let description = string_member(property, "description");
        assert_eq!(
            description, expected.description,
            "case {case:?}: the description must be the catalog's own prose, got \
             {description:?}"
        );

        let served = served_requiredness(&case, declaring, property, name);
        assert_eq!(
            served, expected.requiredness,
            "case {case:?}: the catalog publishes requiredness as {:?}, got {served:?} from \
             {property:?} in {declaring:?}",
            expected.requiredness
        );

        let served_pattern = property
            .get("pattern")
            .filter(|value| !value.is_null())
            .map(|pattern| {
                pattern
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a pattern is a regular expression, got {pattern:?}")
                    })
                    .to_string()
            });
        if let Some(pattern) = expected.pattern {
            assert_eq!(
                served_pattern.as_deref(),
                Some(pattern),
                "case {case:?}: the catalog publishes the pattern {pattern:?}, got \
                 {served_pattern:?}"
            );
        }

        if !expected.accepted.is_empty() || !expected.rejected.is_empty() {
            let pattern = served_pattern.clone().unwrap_or_else(|| {
                panic!("case {case:?}: the property publishes a pattern, got {property:?}")
            });
            let expression = Regex::new(&pattern).unwrap_or_else(|error| {
                panic!("case {case:?}: {pattern:?} is a usable expression, got {error}")
            });
            for value in expected.accepted {
                assert!(
                    expression.is_match(value),
                    "case {case:?}: the published pattern {pattern:?} must accept {value:?}"
                );
            }
            for value in expected.rejected {
                assert!(
                    !expression.is_match(value),
                    "case {case:?}: the published pattern {pattern:?} must reject {value:?}"
                );
            }
        }
    }
}

/// No property's metadata anywhere in the catalog is synthesized from the property's own
/// name: the prose does not spell the member name it is declared under, a pattern is
/// anchored at both ends, and a `required` key is a boolean.
#[tokio::test]
async fn no_definition_property_metadata_is_derived_from_its_name() {
    let emulator = Emulator::new();
    let mut properties_walked = 0;

    for (case, _, read) in catalog(&emulator).await {
        for document_name in DEFINITION_DOCUMENTS {
            let Some(document) = read.get(document_name).filter(|value| !value.is_null()) else {
                continue;
            };
            walk_property_metadata(&case, document_name, document, &mut properties_walked);
        }
        if let Some(namespace) = read
            .get("providerIdNameSpaceConfiguration")
            .filter(|value| !value.is_null())
        {
            for definition_name in [
                "providerSourceConfigurationDefinition",
                "providerTargetConfigurationDefinition",
            ] {
                let definition = member(&case, namespace, definition_name);
                walk_property_metadata(&case, definition_name, definition, &mut properties_walked);
            }
        }
    }

    assert!(
        properties_walked > 0,
        "the catalog's definition documents must declare properties to walk, got \
         {properties_walked}"
    );
}

/// Checks every property of one schema object, and of the objects under it, against the
/// no-synthesis rule.
fn walk_property_metadata(case: &str, path: &str, schema: &Value, walked: &mut usize) {
    let properties = member(case, schema, "properties")
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: {path} declares `properties`, got {schema:?}"))
        .clone();
    for (name, property) in &properties {
        let property_path = format!("{path}.properties.{name}");
        *walked += 1;

        let value_type = string_member(property, "type");
        match property.get("description").filter(|value| !value.is_null()) {
            Some(_) => {
                let description = string_member(property, "description");
                assert!(
                    !description.is_empty(),
                    "case {case:?}: {property_path} must say what it is for, got {property:?}"
                );
                assert!(
                    !description.contains(name.as_str()),
                    "case {case:?}: {property_path} must carry the catalog's prose rather than a \
                     sentence built from its own name, got {description:?}"
                );
            }
            // A property whose value a caller fills in says what goes there; a property
            // that only groups the ones under it (the provider's `entity`) is described
            // by what it declares.
            None => assert_eq!(
                value_type, "object",
                "case {case:?}: {property_path} must say what it is for, got {property:?}"
            ),
        }

        if let Some(pattern) = property.get("pattern").filter(|value| !value.is_null()) {
            let pattern = pattern.as_str().unwrap_or_else(|| {
                panic!("case {case:?}: {property_path}.pattern is an expression, got {pattern:?}")
            });
            assert!(
                is_anchored(pattern),
                "case {case:?}: {property_path}.pattern must match the whole value, got \
                 {pattern:?}"
            );
            if let Err(error) = Regex::new(pattern) {
                panic!(
                    "case {case:?}: {property_path}.pattern must be usable by a client that \
                     validates with it, got {error}"
                );
            }
        }

        if value_type == "object" {
            // A property holding an object is a schema object itself, so its own
            // `required` member is the list of properties it requires.
            walk_property_metadata(case, &property_path, property, walked);
            continue;
        }
        if let Some(required) = property.get("required").filter(|value| !value.is_null()) {
            assert!(
                required.is_boolean(),
                "case {case:?}: {property_path}.required tells a caller whether the key must \
                 appear, so it is a boolean, got {required:?}"
            );
        }
    }
}
