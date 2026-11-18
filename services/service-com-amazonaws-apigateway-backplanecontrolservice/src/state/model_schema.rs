// SPDX-License-Identifier: CC0-1.0

//! The REFERENCES a model's schema carries: how one is read, and whether it resolves to a
//! schema the API can address.
//!
//! A module of its own under [`crate::state`], because the question belongs to the model's
//! SCHEMA rather than to any one operation. A reference states what an instance of that part
//! of the document must satisfy (JSON Schema draft-04 section 7), and a model's schema states
//! one of two kinds: a FRAGMENT of the document itself, which draft-04 addresses by JSON
//! Pointer, and another MODEL of the same API, which API Gateway identifies by that model's
//! own URL under the API that holds it. The second is what a flattened read inlines
//! (`crate::ops::get_model::flattened_schema`) and what a deployed stage's request validator
//! resolves (`crate::service::invoke::request_validation`); both are what a write of a
//! model's schema is VALIDATED against here. One reading of a reference for all of them, so
//! the reference a create accepts is the one a read and a validator can resolve.

/// The model of the API `rest_api_id` a `$ref` names, when the reference names one.
///
/// API Gateway references another model of the same API by the model's own URL,
/// `.../restapis/{restApiId}/models/{modelName}`, so a reference is read as that tail and is
/// a model reference only when it names THIS API. Anything else — a JSON Pointer into the
/// document itself, a reference to another API, a URL of something else — names no model of
/// this API.
pub fn referenced_model<'a>(reference: &'a str, rest_api_id: &str) -> Option<&'a str> {
    let (_, addressed) = reference.rsplit_once("/restapis/")?;
    let (api, model) = addressed.split_once("/models/")?;
    (api == rest_api_id && !model.is_empty() && !model.contains('/')).then_some(model)
}

/// The first reference `schema` states that resolves to NOTHING, or `None` when every one of
/// them resolves (which includes a schema carrying none).
///
/// A reference resolves when the schema it names can be reached: a FRAGMENT reference
/// addresses a node of this very document by JSON Pointer, and a MODEL reference names a
/// model the API holds. One that resolves to nothing describes no payload — the part of the
/// document it stands for means nothing — which is why a write of the schema is refused
/// rather than stored. A reference to a resource OUTSIDE the API's own models (a URL of
/// another scheme, host or API) names nothing this service resolves either way, so it is left
/// alone here: that is the configuration a deployed stage reports it cannot apply.
///
/// A SCHEMA's references, which is what distinguishes this from
/// [`crate::state::unresolved_model_reference`]: that one resolves the model a METHOD names
/// for a content type, this one the schemas a model's own document is composed of.
///
/// `holds_model` answers whether the API holds a model under that name — the records as they
/// will stand once this write is applied, so a schema referring to the very model being
/// written resolves. A schema that is not a JSON document carries no reference this reading
/// can find: the document is read as JSON Schema only where the model's content type says it
/// is one, and a schema that is not readable at all is refused before this.
///
/// The whole document is walked, so a reference nested anywhere in it is found; the walk is
/// bounded by the document, which is itself bounded at
/// [`crate::ops::create_model::MAX_MODEL_SCHEMA_CHARS`] and parsed by a reader with its own
/// nesting limit.
pub fn unresolved_schema_reference(
    schema: &str,
    rest_api_id: &str,
    holds_model: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let document = ::serde_json::from_str::<::serde_json::Value>(schema).ok()?;
    first_unresolved(&document, &document, rest_api_id, holds_model)
}

/// One step of [`unresolved_schema_reference`]: the first unresolved reference under `value`.
///
/// `document` is the whole schema, which is what a fragment reference is addressed against.
fn first_unresolved(
    document: &::serde_json::Value,
    value: &::serde_json::Value,
    rest_api_id: &str,
    holds_model: &dyn Fn(&str) -> bool,
) -> Option<String> {
    match value {
        ::serde_json::Value::Object(members) => {
            if let Some(reference) = members
                .get(REFERENCE_KEY)
                .and_then(::serde_json::Value::as_str)
                .filter(|reference| !resolves(document, reference, rest_api_id, holds_model))
            {
                return Some(reference.to_string());
            }
            members
                .values()
                .find_map(|member| first_unresolved(document, member, rest_api_id, holds_model))
        }
        ::serde_json::Value::Array(entries) => entries
            .iter()
            .find_map(|entry| first_unresolved(document, entry, rest_api_id, holds_model)),
        _ => None,
    }
}

/// Whether `reference` resolves to a schema this API can address.
///
/// A reference whose URI is a FRAGMENT of this document (`#`, `#/definitions/Pet`) addresses
/// it by JSON Pointer, so it resolves exactly when the document holds that node — the whole
/// document for the empty pointer. A reference naming a MODEL of this API resolves when the
/// API holds it. Anything else names no schema this service addresses, which is not a
/// resolution this write decides.
fn resolves(
    document: &::serde_json::Value,
    reference: &str,
    rest_api_id: &str,
    holds_model: &dyn Fn(&str) -> bool,
) -> bool {
    if let Some(pointer) = reference.strip_prefix('#') {
        return pointer.is_empty() || document.pointer(pointer).is_some();
    }
    match referenced_model(reference, rest_api_id) {
        Some(model) => holds_model(model),
        None => true,
    }
}

/// The member a JSON Schema names a reference with (JSON Schema draft-04 section 7).
const REFERENCE_KEY: &str = "$ref";

/// How many of the api's model schemas one read copies out for the references a schema
/// carries.
///
/// A model reference names another model of the api, and that model's schema may reference
/// further ones, so the set is closed over — bounded here, as the inlining a flattened read
/// performs is bounded, so one request copies a bounded number of stored schemas however the
/// api's models reference each other.
const MAX_REFERENCED_MODELS: usize = 16;

/// The schemas of the models of `api` that `schema` references, by model name — directly, or
/// through the models it references.
///
/// What a read that has to RESOLVE a model reference needs copied out from under the state
/// guard: the referenced schemas themselves, so the resolution runs on owned records and
/// holds no lock. Bounded by [`MAX_REFERENCED_MODELS`]; a reference past the bound is left
/// unresolved, exactly as a flattened read leaves one.
pub fn referenced_model_schemas(
    api: &super::RestApiEntry,
    rest_api_id: &str,
    schema: &str,
) -> ::std::collections::BTreeMap<String, String> {
    let mut copied: ::std::collections::BTreeMap<String, String> =
        ::std::collections::BTreeMap::new();
    let mut pending: Vec<String> = vec![schema.to_string()];
    while let Some(document) = pending.pop() {
        if copied.len() >= MAX_REFERENCED_MODELS {
            break;
        }
        let Ok(document) = ::serde_json::from_str::<::serde_json::Value>(&document) else {
            continue;
        };
        for name in model_references(&document, rest_api_id) {
            if copied.contains_key(&name) || copied.len() >= MAX_REFERENCED_MODELS {
                continue;
            }
            if let Some(referenced) = api.models.get(&name).and_then(|model| model.schema.clone()) {
                pending.push(referenced.clone());
                copied.insert(name, referenced);
            }
        }
    }
    copied
}

/// The models of the api `rest_api_id` that `document` references, by name.
fn model_references(document: &::serde_json::Value, rest_api_id: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    collect_model_references(document, rest_api_id, &mut names);
    names
}

/// One step of [`model_references`].
fn collect_model_references(
    value: &::serde_json::Value,
    rest_api_id: &str,
    names: &mut Vec<String>,
) {
    match value {
        ::serde_json::Value::Object(members) => {
            if let Some(name) = members
                .get(REFERENCE_KEY)
                .and_then(::serde_json::Value::as_str)
                .and_then(|reference| referenced_model(reference, rest_api_id))
            {
                names.push(name.to_string());
            }
            for member in members.values() {
                collect_model_references(member, rest_api_id, names);
            }
        }
        ::serde_json::Value::Array(entries) => {
            for entry in entries {
                collect_model_references(entry, rest_api_id, names);
            }
        }
        _ => {}
    }
}

/// The `BadRequestException` message for a model whose SCHEMA does not validate.
///
/// API Gateway validates a model's schema when the caller writes it and answers the result
/// of that validation: the fixed framing is the validation result with its empty warnings
/// list and its errors list, and the one error names the MODEL the schema specified that the
/// validation could not make sense of ([`reference_model_name`]). Used wherever a caller's
/// schema is written, so a client classifying this refusal reads one sentence whether a
/// create or an update made it.
///
/// It is not [`crate::state::INVALID_MODEL_SCHEMA`], which is the same framing around the
/// error API Gateway reports for a document that is no schema document at all.
pub fn invalid_model_specified(model_name: &str) -> String {
    format!(
        "Invalid model specified: Validation Result: warnings : [], errors : [Invalid model \
         specified: {model_name}]"
    )
}

/// The model a reference STANDS FOR, as API Gateway names it in a diagnostic: the last token
/// of the reference, which is the name the document declares that model or definition under
/// (`#/definitions/<name>`, `#/components/schemas/<name>` in an OpenAPI document,
/// `.../models/<name>`). A reference carrying no separator at all names that model directly.
///
/// The one reading of that name for every operation that reports an unresolved reference: a
/// write of a model's schema refuses one by it, and an import WARNS about one by it
/// (`crate::ops::put_rest_api`'s `Reference to model '<name>' not found. Ignoring.`), so one
/// unresolved reference is named one way whichever operation met it.
pub fn reference_model_name(reference: &str) -> &str {
    reference
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API the references below are read against, and one spelled for another API.
    const API: &str = "abc1234567";

    fn model_url(api: &str, model: &str) -> String {
        format!("https://apigateway.amazonaws.com/restapis/{api}/models/{model}")
    }

    #[test]
    fn a_reference_resolves_against_the_document_itself_or_the_models_of_its_own_api() {
        // The rows span what the rule generalises over: which KIND of reference the schema
        // states (a fragment of the document, a model of this api), where the reference sits
        // in the document, whether what it names is there, and the forms that name no schema
        // this service addresses (another api's model, a URL of something else), which this
        // reading leaves alone.
        let held = |name: &str| matches!(name, "Leg" | "Pet");
        let rows: &[(&str, ::serde_json::Value, Option<String>)] = &[
            (
                "a schema carrying no reference",
                ::serde_json::json!({ "type": "object", "properties": { "id": { "type": "string" } } }),
                None,
            ),
            (
                "a property referencing a held model",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "front": { "$ref": model_url(API, "Leg") } }
                }),
                None,
            ),
            (
                "a property referencing a model the api does not hold",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "front": { "$ref": model_url(API, "Absent") } }
                }),
                Some(model_url(API, "Absent")),
            ),
            (
                "a reference inside a list, to a model the api does not hold",
                ::serde_json::json!({
                    "type": "object",
                    "properties": {
                        "legs": { "type": "array", "items": { "$ref": model_url(API, "Gone") } }
                    }
                }),
                Some(model_url(API, "Gone")),
            ),
            (
                "a reference nested under a composition",
                ::serde_json::json!({
                    "allOf": [{ "$ref": model_url(API, "Pet") }, { "$ref": model_url(API, "Nope") }]
                }),
                Some(model_url(API, "Nope")),
            ),
            (
                "a reference at the document root",
                ::serde_json::json!({ "$ref": model_url(API, "Missing") }),
                Some(model_url(API, "Missing")),
            ),
            (
                "a fragment reference the document resolves",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "pet": { "$ref": "#/definitions/Pet" } },
                    "definitions": { "Pet": { "type": "object" } }
                }),
                None,
            ),
            (
                "a fragment reference the document does not hold",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "pet": { "$ref": "#/definitions/Pet" } }
                }),
                Some("#/definitions/Pet".to_string()),
            ),
            (
                "a fragment reference naming a definition beside the ones declared",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "pet": { "$ref": "#/definitions/Absent" } },
                    "definitions": { "Pet": { "type": "object" } }
                }),
                Some("#/definitions/Absent".to_string()),
            ),
            (
                "a reference to the document itself",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "child": { "$ref": "#" } }
                }),
                None,
            ),
            (
                "a model of ANOTHER api, which names no model of this one",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "front": { "$ref": model_url("zzzzzzzzzz", "Leg") } }
                }),
                None,
            ),
            (
                "a reference that is no model URL at all",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "front": { "$ref": "http://127.0.0.1/schema.json" } }
                }),
                None,
            ),
            (
                "a reference that is not even a string",
                ::serde_json::json!({ "properties": { "front": { "$ref": 7 } } }),
                None,
            ),
        ];
        for (case, document, expected) in rows {
            let unresolved = unresolved_schema_reference(&document.to_string(), API, &held);
            assert_eq!(
                unresolved.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected unresolved={expected:?}, got {unresolved:?}"
            );
        }
    }

    #[test]
    fn a_schema_that_is_not_a_json_document_carries_no_reference_to_resolve() {
        let unresolved = unresolved_schema_reference("<xs:schema/>", API, &|_| false);
        assert!(
            unresolved.is_none(),
            "case a schema that is not JSON: expected no reference, got {unresolved:?}"
        );
    }
}
