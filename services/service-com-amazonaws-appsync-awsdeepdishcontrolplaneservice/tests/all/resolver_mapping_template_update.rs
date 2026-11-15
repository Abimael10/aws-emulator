// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateResolver` leaves behind on the two mapping-template members, end to end
//! over HTTP.
//!
//! One rule is asserted here, over a class of inputs rather than over the one request
//! that exposed the defect:
//!
//! a write of a resolver stores the mapping templates the request carries VERBATIM, and
//! the members it omits leave the resolver carrying none — a mapping template is opaque
//! text (`MappingTemplate`, `@pattern ^.*$`, `@length` 1..65536), so nothing about its
//! content is interpreted, normalised or re-encoded — so every later reader of that field
//! (`UpdateResolver`'s own answer, `GetResolver`, `ListResolvers`) answers with exactly
//! the characters that were submitted, and the write that submitted them last is the one
//! a read reports.
//!
//! The rows differ along what the rule generalises over — the alphabet (printable ASCII,
//! accented Latin, Greek, Cyrillic, CJK, emoji, mathematical symbols), the characters
//! JSON has to escape (double quotes, backslashes, tabs, line terminators), the line
//! count and the line terminator, the size from the modelled minimum to the modelled
//! maximum, and which of the two members the request carries at all. The recorded case —
//! a non-ASCII template replaced by another non-ASCII template — is one row of that
//! table.
//!
//! Because the rule is a ROUND TRIP, the last suite below states it over templates nobody
//! chose: the characters and the length are DRAWN, a bounded number of cases per run, the
//! seed carried in every assertion message so a failure replays exactly, and nothing
//! written to disk.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handlers
//! and the shared engine together. The harness (`call`, `router`, `Answer`) is the
//! error-contract suite's and the API, data source and resolver helpers are the resolver
//! suite's — reused rather than copied.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, router, Answer};
// `MappingTemplate`'s modelled `@length` bounds and the generator of a template of a
// given length are the free-text suite's, which states the bound rule over the same
// shape: the bounds are quoted from the model there (`smithy_query(query="shape",
// shape="MappingTemplate")` reports `@length(min: 1, max: 65536)`) and read here rather
// than restated.
use crate::free_text_members::{template_of, TEMPLATE_MAX_LEN, TEMPLATE_MIN_LEN};
use crate::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, DATA_SOURCE, TYPE,
};

/// The request template every resolver below is CREATED with: a value no row of the
/// update table submits, so a member that still reports it after an update is a lost
/// write rather than a coincidence.
const CREATED_REQUEST: &str = "## created: héllo 世界 🎉 \"quoted\" \\backslash\\ Ω≈ç√\n{\n  \
                               \"version\": \"2018-05-29\",\n  \"operation\": \"Invoke\",\n  \
                               \"payload\": $util.toJson($context.arguments)\n}";

/// The response template every resolver below is created with ([`CREATED_REQUEST`]).
const CREATED_RESPONSE: &str = "## created response\n$util.toJson($context.result)";

/// How many drawn template pairs one run of the property-based suite asserts the round
/// trip over, beside the two cases sitting on the modelled bounds. Bounded so the suite
/// stays a test.
const DRAWN_PAIRS: usize = 6;

/// The Unicode blocks a drawn template's characters come from: a mapping template is
/// opaque text, so the generator spans the alphabets a real one is written in rather than
/// one script. Line terminators and tabs are drawn separately ([`DRAWN_WHITESPACE`]).
const DRAWN_BLOCKS: &[std::ops::RangeInclusive<u32>] = &[
    // Printable ASCII: the alphabet of a plain Velocity template, `"` and `\` included.
    0x20..=0x7e,
    // Latin-1 supplement: accented Latin text.
    0xa0..=0xff,
    // Greek and Coptic through Cyrillic.
    0x0370..=0x04ff,
    // Mathematical operators.
    0x2200..=0x22ff,
    // CJK unified ideographs.
    0x4e00..=0x9fff,
    // Supplemental symbols and pictographs: emoji, which are outside the basic plane.
    0x1f300..=0x1f9ff,
];

/// The whitespace a drawn template is broken into lines with: a real template is
/// line-oriented, and these are the characters JSON escapes rather than carries.
const DRAWN_WHITESPACE: &[char] = &['\n', '\r', '\t', ' '];

/// One row of the template table: the pair of templates an update submits, each `None`
/// where the request carries the member not at all.
#[derive(Debug, Clone)]
struct Row {
    /// What the row exercises, named in every assertion of it.
    label: String,
    request: Option<String>,
    response: Option<String>,
}

/// The update table, spanning the class the rule generalises over.
///
/// The two rows sitting on the modelled `@length` bounds are written in ASCII, which is
/// what [`template_of`] builds; a template of the maximum length written in several
/// scripts at once is drawn by the generative suite below.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a single-line JSON request template and a VTL response template".to_string(),
            request: Some("{\"version\": \"2017-02-28\", \"operation\": \"GetItem\"}".to_string()),
            response: Some("$util.toJson($ctx.result)".to_string()),
        },
        Row {
            label: "templates carrying accented Latin, CJK, Cyrillic and emoji text"
                .to_string(),
            request: Some(
                "## updated: ñøπ 🚀 üñíçødé\n{\n  \"version\": \"2018-05-29\",\n  \
                 \"operation\": \"Invoke\"\n}"
                    .to_string(),
            ),
            response: Some("## updated response ключ\n$util.toJson($context.result)".to_string()),
        },
        Row {
            label: "templates carrying embedded double quotes and backslashes".to_string(),
            request: Some(
                "## \"double\\\"quote\" back\\\\slash \\n not a newline\n$util.toJson($ctx.args)"
                    .to_string(),
            ),
            response: Some("\"\\\"\\\\\"".to_string()),
        },
        Row {
            label: "a pretty-printed template whose #foreach/#end directives sit on their \
                    own lines"
                .to_string(),
            request: Some(
                "{\n  \"version\": \"2018-05-29\",\n  \"operation\": \"BatchPutItem\",\n  \
                 \"tables\": {\n    \"items\": [\n      #foreach($item in $ctx.args.items)\n  \
                 $util.dynamodb.toMapValues($item)#if($foreach.hasNext),#end\n      #end\n    \
                 ]\n  }\n}"
                    .to_string(),
            ),
            response: Some("#if($ctx.error)\n  $util.error($ctx.error.message)\n#end\n".to_string()),
        },
        Row {
            label: "templates with CRLF line endings and tabs".to_string(),
            request: Some("{\r\n\t\"version\": \"2017-02-28\"\r\n}".to_string()),
            response: Some("\t$util.toJson($ctx.result)\r\n".to_string()),
        },
        Row {
            label: "templates one character long, the modelled minimum".to_string(),
            request: Some(template_of(TEMPLATE_MIN_LEN)),
            response: Some("\n".to_string()),
        },
        Row {
            label: "templates of exactly the modelled maximum length".to_string(),
            request: Some(template_of(TEMPLATE_MAX_LEN)),
            response: Some(template_of(TEMPLATE_MAX_LEN)),
        },
        Row {
            label: "a template of blank lines only".to_string(),
            request: Some("\n\n\n".to_string()),
            response: Some("   ".to_string()),
        },
        Row {
            label: "an update that carries no response template".to_string(),
            request: Some("## request only\n$util.toJson($ctx.args)".to_string()),
            response: None,
        },
        Row {
            label: "an update that carries no request template".to_string(),
            request: None,
            response: Some("## response only\n$util.toJson($ctx.result)".to_string()),
        },
        Row {
            label: "an update that carries neither template".to_string(),
            request: None,
            response: None,
        },
        Row {
            label: "an update restating the templates the resolver was created with"
                .to_string(),
            request: Some(CREATED_REQUEST.to_string()),
            response: Some(CREATED_RESPONSE.to_string()),
        },
    ]
}

/// The value a resolver's template member is owed for `template`: the submitted
/// characters, or the absent member a request that carried none leaves.
fn member(template: Option<&String>) -> Value {
    template.map_or(Value::Null, |text| json!(text))
}

/// How a template member is named in a failure message: its size and the characters it
/// starts with, so a row at the modelled maximum does not print 65,536 of them.
fn summary(value: &Value) -> String {
    match value.as_str() {
        None => format!("the member absent ({value})"),
        Some(text) => {
            let head: String = text.chars().take(48).collect();
            format!(
                "{} character(s) / {} byte(s) beginning {head:?}",
                text.chars().count(),
                text.len()
            )
        }
    }
}

/// The `CreateResolver` body every resolver below is attached with: a UNIT resolver on
/// `field`, carrying the create-time template pair.
fn created_body(field: &str) -> Value {
    json!({
        "fieldName": field,
        "dataSourceName": DATA_SOURCE,
        "requestMappingTemplate": CREATED_REQUEST,
        "responseMappingTemplate": CREATED_RESPONSE,
    })
}

/// The `UpdateResolver` body `row` asks for. An update states the resolver in full, so the
/// data source it resolves against is named again; the templates are the row's.
fn update_body(row: &Row) -> Value {
    let mut body = json!({"dataSourceName": DATA_SOURCE});
    let object = body.as_object_mut().expect("the request body is an object");
    if let Some(template) = &row.request {
        object.insert("requestMappingTemplate".to_string(), json!(template));
    }
    if let Some(template) = &row.response {
        object.insert("responseMappingTemplate".to_string(), json!(template));
    }
    body
}

/// Send one `UpdateResolver` restating the resolver on `field` as `row` asks.
async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    row: &Row,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
        Some(update_body(row)),
    )
    .await
}

/// The entry `ListResolvers` reports for `field`, or a panic naming the case: the listing
/// is the third reader of the same stored record.
async fn listed_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    case: &str,
) -> Value {
    let listing = call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        listing.status, 200,
        "case {case:?}: expected ListResolvers 200, got status {}",
        listing.status
    );
    let entry = listing.body["resolvers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|resolver| resolver["fieldName"] == json!(field))
        .cloned();
    entry.unwrap_or_else(|| {
        panic!("case {case:?}: expected the listing of {TYPE} to hold the resolver on {field}, got none")
    })
}

/// The resolver record with its two template members removed, so a whole-record
/// comparison's failure message stays readable while the templates are compared on their
/// own, with [`summary`].
fn without_templates(resolver: &Value) -> Value {
    let mut value = resolver.clone();
    if let Some(object) = value.as_object_mut() {
        object.remove("requestMappingTemplate");
        object.remove("responseMappingTemplate");
    }
    value
}

/// Assert `resolver`, as `source` answered it, carries exactly `request` and `response` on
/// the two template members — and, where one of them is `None`, that the member is not in
/// the answered document at all rather than written as a null, which is the shape a
/// resolver carrying no template has on the wire.
fn assert_templates(
    case: &str,
    source: &str,
    resolver: &Value,
    request: Option<&String>,
    response: Option<&String>,
) {
    let members = resolver
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected {source} to answer a resolver object, got {resolver}"));
    for (name, submitted) in [
        ("requestMappingTemplate", request),
        ("responseMappingTemplate", response),
    ] {
        let expected = member(submitted);
        let actual = &resolver[name];
        assert!(
            *actual == expected,
            "case {case:?}: expected {source} to answer {name} as {}, got {}",
            summary(&expected),
            summary(actual)
        );
        assert_eq!(
            members.contains_key(name),
            submitted.is_some(),
            "case {case:?}: expected {source} to carry {name} only where one was submitted \
             (submitted: {}), got the member {}",
            submitted.is_some(),
            if members.contains_key(name) {
                "present"
            } else {
                "absent"
            }
        );
    }
}

/// Attach the resolver an update addresses, with the create-time templates, and assert
/// those are what a read of it answers — so each row below starts from a resolver whose
/// stored templates are known and differ from the ones it submits.
async fn settled_resolver(router: &MultiServiceRouter, api_id: &str, field: &str, case: &str) {
    let created = create_resolver_body(router, api_id, TYPE, created_body(field)).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected CreateResolver 200, got {created:?}"
    );
    let created_request = CREATED_REQUEST.to_string();
    let created_response = CREATED_RESPONSE.to_string();
    assert_templates(
        case,
        "CreateResolver",
        &created.body["resolver"],
        Some(&created_request),
        Some(&created_response),
    );
    let before = get_resolver(router, api_id, TYPE, field).await;
    assert_eq!(
        before.status, 200,
        "case {case:?}: expected the attached resolver to be served, got status {}",
        before.status
    );
    assert_templates(
        case,
        "GetResolver before the update",
        &before.body["resolver"],
        Some(&created_request),
        Some(&created_response),
    );
}

/// Update the resolver on `field` as `row` asks and assert every reader of that field
/// answers the templates the update submitted.
async fn assert_update_is_persisted(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    case: &str,
    request: Option<&String>,
    response: Option<&String>,
) {
    let row = Row {
        label: case.to_string(),
        request: request.cloned(),
        response: response.cloned(),
    };
    let updated = update_resolver(router, api_id, field, &row).await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected UpdateResolver 200, got status {} and error type {:?}: {:?}",
        updated.status, updated.error_type, updated.message
    );
    assert_templates(
        case,
        "UpdateResolver",
        &updated.body["resolver"],
        request,
        response,
    );

    // The answer above is the STORED record, so an independent read answers the same
    // resolver — member for member, not only on the templates.
    let read = get_resolver(router, api_id, TYPE, field).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetResolver 200 after the update, got status {}",
        read.status
    );
    assert_templates(case, "GetResolver", &read.body["resolver"], request, response);
    assert_eq!(
        without_templates(&read.body["resolver"]),
        without_templates(&updated.body["resolver"]),
        "case {case:?}: expected GetResolver to answer the record UpdateResolver stored"
    );

    // And so does the listing the field's type is paged through.
    let listed = listed_resolver(router, api_id, field, case).await;
    assert_templates(case, "ListResolvers", &listed, request, response);
    assert_eq!(
        without_templates(&listed),
        without_templates(&read.body["resolver"]),
        "case {case:?}: expected ListResolvers to report the record GetResolver answers"
    );
}

/// Rule: an update's mapping templates are stored verbatim, each row against a resolver
/// of its own, so no row's outcome depends on another's.
#[tokio::test]
async fn an_update_stores_the_mapping_templates_it_submits_verbatim() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_resolver_template_update").await;

    for (index, row) in rows().iter().enumerate() {
        let field = format!("updated_{index}");
        settled_resolver(&router, &api_id, &field, &row.label).await;
        assert_update_is_persisted(
            &router,
            &api_id,
            &field,
            &row.label,
            row.request.as_ref(),
            row.response.as_ref(),
        )
        .await;
    }
}

/// Rule, over one resolver written again and again: the write that submitted a template
/// LAST is the one every reader answers, so no update is lost behind an earlier one and
/// none leaves an earlier row's text behind.
#[tokio::test]
async fn successive_updates_each_supersede_the_stored_mapping_templates() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_resolver_template_restated").await;
    let field = "restated";
    settled_resolver(&router, &api_id, field, "the resolver successive updates restate").await;

    for row in &rows() {
        let case = format!("{} restated in sequence", row.label);
        assert_update_is_persisted(
            &router,
            &api_id,
            field,
            &case,
            row.request.as_ref(),
            row.response.as_ref(),
        )
        .await;
    }
}

/// A template of exactly `length` drawn characters, from the blocks a mapping template may
/// be written in ([`DRAWN_BLOCKS`]) with drawn line terminators and tabs between them.
fn drawn_template(rng: &mut StdRng, length: usize) -> String {
    let mut text = String::new();
    let mut count = 0usize;
    while count < length {
        // Roughly one character in six breaks the template into lines, so a drawn value
        // is line-oriented the way a real template is.
        let character = if rng.random_range(0..6) == 0 {
            DRAWN_WHITESPACE[rng.random_range(0..DRAWN_WHITESPACE.len())]
        } else {
            let block = DRAWN_BLOCKS[rng.random_range(0..DRAWN_BLOCKS.len())].clone();
            let scalar = rng.random_range(block);
            match char::from_u32(scalar) {
                Some(character) => character,
                // Every range above holds assigned scalar values only, so this is
                // unreachable; drawing again rather than panicking keeps the generator
                // total.
                None => continue,
            }
        };
        text.push(character);
        count += 1;
    }
    text
}

/// Rule, over templates nobody chose: whatever characters and length a drawn template has,
/// the update that submits it is the text every reader answers, byte for byte.
#[tokio::test]
async fn drawn_mapping_templates_round_trip_through_an_update_unchanged() {
    let seed: u64 = rand::random();
    let mut rng = StdRng::seed_from_u64(seed);

    // The lengths one run asserts: both published bounds of the modelled `@length`, and
    // DRAWN_PAIRS lengths drawn from between them.
    let mut lengths = vec![TEMPLATE_MIN_LEN, TEMPLATE_MAX_LEN];
    for _ in 0..DRAWN_PAIRS {
        lengths.push(rng.random_range(TEMPLATE_MIN_LEN..=4_096));
    }

    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_resolver_template_drawn").await;
    let field = "drawn";
    settled_resolver(&router, &api_id, field, &format!("seed {seed}: the drawn resolver")).await;

    for length in lengths {
        let case = format!("seed {seed}: a drawn {length}-character template pair");
        let request = drawn_template(&mut rng, length);
        let response = drawn_template(&mut rng, length);
        assert_update_is_persisted(
            &router,
            &api_id,
            field,
            &case,
            Some(&request),
            Some(&response),
        )
        .await;

        // Verbatim means the BYTES survive as well as the characters: a template
        // re-encoded or normalised would still compare equal above only if it were
        // unchanged, so this states the size the round trip preserved.
        let read = get_resolver(&router, &api_id, TYPE, field).await;
        for (name, submitted) in [
            ("requestMappingTemplate", &request),
            ("responseMappingTemplate", &response),
        ] {
            let answered = read.body["resolver"][name].as_str().unwrap_or_default();
            assert_eq!(
                (answered.len(), answered.chars().count()),
                (submitted.len(), submitted.chars().count()),
                "case {case:?}: expected {name} to keep the submitted size, got {}",
                summary(&read.body["resolver"][name])
            );
        }
    }
}
