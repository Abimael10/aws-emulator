// SPDX-License-Identifier: CC0-1.0

//! The SDL document `ListTypes` reports a type's definition as, over object types DRAWN
//! rather than hand-picked.
//!
//! The invariant: a read of a type reports `Type$definition` in SDL as the service's own
//! rendering of the type system the API holds — the declaring keyword and the type's name
//! on a line of their own, a blank line, the body's opening brace on its own line, each
//! field written unindented and followed by a blank line, consecutive fields separated by
//! one further blank line, the closing brace on its own line, and the whole document
//! terminated by a newline — computed from the PARSED definition, so it depends neither on
//! the text the request wrote the definition in nor on the way the type got into the API.
//!
//! What this suite adds to the tables of [`crate::type_sdl_rendering`] and
//! [`crate::list_types_sdl_layout`] is that the layout holds for a type nobody chose: the
//! name, the number of fields, the field names and the type each field names (a drawn
//! built-in scalar under a drawn combination of GraphQL's list and non-null wrappers) are
//! DRAWN, and the text the request writes the very same type in is drawn too — one line,
//! space- or tab-indented, comma-separated or spread over extra blank lines. The expected
//! document is built here from the drawn model by the layout stated above rather than from
//! anything the service rendered, so a rendering that followed some other layout cannot
//! pass. The field count ranges from one field up past the two and three the tables cover.
//!
//! Bounded case count, nothing written to disk, and the drawn seed carried in every
//! assertion message so a failure replays exactly. The router, the API-creating helper and
//! the type calls are the crate's shared harness, reused rather than copied.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::Value;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::graphql_api_lifecycle::router;
use super::graphql_types::{create_api, create_type, get_type, Answer};
use super::list_types_sdl_layout::reported;
use super::type_sdl_rendering::{create_schema, definition_of, list_types};

/// The forms one GraphQL type REFERENCE is written in: a name on its own, a non-null one,
/// a list of them, and the combinations of the two wrappers.
///
/// Source: GraphQL's type-reference grammar, which `Type$definition`'s SDL is written in —
/// `[T]` wraps a reference in a list, `T!` makes one non-null. `{}` stands for the named
/// type the reference is rooted at.
const REFERENCE_FORMS: &[&str] = &["{}", "{}!", "[{}]", "[{}]!", "[{}!]", "[{}!]!"];

/// The words a drawn type name is built from, so the name under test is none the tables
/// already name.
const NAME_WORDS: &[&str] = &[
    "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta", "Eta", "Theta",
];

/// The words a drawn field name is built from.
const FIELD_WORDS: &[&str] = &[
    "id", "name", "title", "count", "tags", "owner", "score", "note",
];

/// How many fields the largest drawn body holds: past the three the hand-written tables
/// cover, and small enough to keep the suite a test.
const MAX_DRAWN_FIELDS: usize = 5;

/// One drawn object type: the name it declares and the fields it holds, each with the type
/// reference it names.
#[derive(Debug, Clone)]
struct Drawn {
    name: String,
    fields: Vec<(String, String)>,
}

impl Drawn {
    /// The SDL document a READ of this type must answer with: the layout this suite's
    /// module documentation states, filled with the drawn name, field names and field
    /// types.
    fn rendered(&self) -> String {
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|(name, value_type)| format!("{name}: {value_type}\n\n"))
            .collect();
        format!("type {}\n\n{{\n{}}}\n", self.name, fields.join("\n"))
    }
}

/// The ways a request can write one and the same type definition: the whitespace and the
/// separators GraphQL ignores between the tokens of a definition.
#[derive(Debug, Clone, Copy)]
enum Layout {
    /// The whole definition on one line.
    OneLine,
    /// The body indented with two spaces, one field per line.
    SpaceIndented,
    /// The body indented with a tab, one field per line.
    TabIndented,
    /// One field per line, each followed by the comma GraphQL reads as whitespace.
    Commas,
    /// The body spread over blank lines, with the opening brace on a line of its own.
    ExtraBlankLines,
}

/// Every layout a case writes a definition in.
const LAYOUTS: &[Layout] = &[
    Layout::OneLine,
    Layout::SpaceIndented,
    Layout::TabIndented,
    Layout::Commas,
    Layout::ExtraBlankLines,
];

impl Layout {
    /// How a case in this layout names itself in an assertion.
    fn label(self) -> &'static str {
        match self {
            Self::OneLine => "written on one line",
            Self::SpaceIndented => "written space-indented",
            Self::TabIndented => "written tab-indented",
            Self::Commas => "written with comma-separated fields",
            Self::ExtraBlankLines => "written over extra blank lines",
        }
    }

    /// `drawn` written out as a request in this layout.
    fn written(self, drawn: &Drawn) -> String {
        let fields: Vec<String> = drawn
            .fields
            .iter()
            .map(|(name, value_type)| format!("{name}: {value_type}"))
            .collect();
        match self {
            Self::OneLine => format!("type {} {{ {} }}", drawn.name, fields.join(" ")),
            Self::SpaceIndented => format!(
                "type {} {{\n{}\n}}\n",
                drawn.name,
                fields
                    .iter()
                    .map(|field| format!("  {field}"))
                    .collect::<Vec<String>>()
                    .join("\n")
            ),
            Self::TabIndented => format!(
                "type {} {{\n{}\n}}\n",
                drawn.name,
                fields
                    .iter()
                    .map(|field| format!("\t{field}"))
                    .collect::<Vec<String>>()
                    .join("\n")
            ),
            Self::Commas => format!(
                "type {} {{\n{}\n}}\n",
                drawn.name,
                fields
                    .iter()
                    .map(|field| format!("  {field},"))
                    .collect::<Vec<String>>()
                    .join("\n")
            ),
            Self::ExtraBlankLines => format!(
                "type {}\n\n{{\n\n{}\n\n}}\n",
                drawn.name,
                fields
                    .iter()
                    .map(|field| format!("   {field}"))
                    .collect::<Vec<String>>()
                    .join("\n\n\n")
            ),
        }
    }
}

/// Draw one object type holding `fields` fields, named apart from every other case of the
/// run by `index`.
fn draw(rng: &mut StdRng, index: usize, fields: usize) -> Drawn {
    let word = NAME_WORDS[rng.random_range(0..NAME_WORDS.len())];
    Drawn {
        name: format!("{word}Drawn{index}"),
        fields: (0..fields)
            .map(|position| {
                let word = FIELD_WORDS[rng.random_range(0..FIELD_WORDS.len())];
                // The named type a field resolves to is one of the scalars every schema
                // holds without defining it (`state::BUILT_IN_SCALARS`), so a drawn
                // definition resolves whether it arrives on its own or inside a schema
                // document.
                let scalars = state::BUILT_IN_SCALARS;
                let scalar = scalars[rng.random_range(0..scalars.len())];
                let form = REFERENCE_FORMS[rng.random_range(0..REFERENCE_FORMS.len())];
                (format!("{word}{position}"), form.replace("{}", scalar))
            })
            .collect(),
    }
}

/// The cases one run asserts: a body of one field, of two, and of a drawn number of
/// fields up to [`MAX_DRAWN_FIELDS`] — each in the layout of the same position in
/// [`LAYOUTS`], so every layout is exercised.
fn cases(rng: &mut StdRng) -> Vec<(Drawn, Layout)> {
    let mut counts = vec![1, 2];
    while counts.len() < LAYOUTS.len() + 1 {
        counts.push(rng.random_range(1..=MAX_DRAWN_FIELDS));
    }
    counts
        .into_iter()
        .enumerate()
        .map(|(index, count)| (draw(rng, index, count), LAYOUTS[index % LAYOUTS.len()]))
        .collect()
}

/// The type named `name` in a listing answer, found by the crate's shared lookup over the
/// types a listing reports.
fn listed(answer: &Answer, name: &str, case: &str) -> Value {
    let types: Vec<Value> = answer.body["types"].as_array().cloned().unwrap_or_default();
    reported(&types, name, case).clone()
}

/// Assert the listing and a single read report `drawn` with the document the layout states.
async fn assert_reported(
    router: &MultiServiceRouter,
    api_id: &str,
    drawn: &Drawn,
    written: &str,
    case: &str,
) {
    let expected = drawn.rendered();
    let listing = list_types(router, api_id, state::SDL_FORMAT).await;
    assert_eq!(
        listing.status, 200,
        "case {case}: expected ListTypes 200, got {listing:?}"
    );
    let reported = listed(&listing, &drawn.name, case);
    let definition = definition_of(&reported).to_string();
    assert_eq!(
        definition, expected,
        "case {case}: expected ListTypes to report {expected:?}, got {definition:?}"
    );
    assert!(
        definition.ends_with('\n'),
        "case {case}: expected the document to be newline-terminated, got {definition:?}"
    );
    assert_eq!(
        reported["format"].as_str(),
        Some(state::SDL_FORMAT),
        "case {case}: expected the SDL format reported back, got {reported:?}"
    );
    if written != expected {
        assert_ne!(
            definition, written,
            "case {case}: expected the submitted text NOT to be echoed, got {definition:?}"
        );
    }

    // A listing and a single read report one type with one document.
    let read = get_type(router, api_id, &drawn.name, state::SDL_FORMAT).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetType 200, got {read:?}"
    );
    assert_eq!(
        definition_of(&read.body["type"]),
        definition,
        "case {case}: expected GetType to report what the listing reports, got {:?}",
        definition_of(&read.body["type"])
    );
}

/// Every drawn type is listed with the service's own layout, whichever way it got into the
/// API and whichever text the request wrote it in.
///
/// The types of an even case are created on their own (`CreateType`); the ones of an odd
/// case are declared inside the one schema document a `StartSchemaCreation` installs —
/// which runs FIRST, because a creation installs the whole type system its document
/// declares.
#[tokio::test]
async fn every_drawn_type_is_listed_with_the_services_own_layout() {
    let seed: u64 = rand::random();
    let mut rng = StdRng::seed_from_u64(seed);
    let cases = cases(&mut rng);

    let router = router();
    let api_id = create_api(&router, "list-types-drawn-type-layout").await;

    let document: String = cases
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, (drawn, layout))| format!("{}\n", layout.written(drawn)))
        .collect();
    create_schema(&router, &api_id, &document).await;

    for (index, (drawn, layout)) in cases.iter().enumerate() {
        if index % 2 == 1 {
            continue;
        }
        let case = format!(
            "seed {seed}: {} fields {} and created on its own",
            drawn.fields.len(),
            layout.label()
        );
        let created =
            create_type(&router, &api_id, &layout.written(drawn), state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected CreateType 200, got {created:?}"
        );
    }

    for (index, (drawn, layout)) in cases.iter().enumerate() {
        let arrival = if index % 2 == 1 {
            "declared by a schema document"
        } else {
            "created on its own"
        };
        let case = format!(
            "seed {seed}: {} fields {} and {arrival}",
            drawn.fields.len(),
            layout.label()
        );
        assert_reported(&router, &api_id, drawn, &layout.written(drawn), &case).await;
    }
}

/// One drawn type written in every layout a request can write it in is listed with ONE
/// document: the rendering is a function of the type the definition declares, not of the
/// text that stated it.
#[tokio::test]
async fn the_listed_document_does_not_depend_on_the_submitted_text() {
    let seed: u64 = rand::random();
    let mut rng = StdRng::seed_from_u64(seed);
    let fields = rng.random_range(2..=MAX_DRAWN_FIELDS);
    let drawn = draw(&mut rng, 0, fields);

    let router = router();
    for layout in LAYOUTS {
        let case = format!(
            "seed {seed}: {} fields {}",
            drawn.fields.len(),
            layout.label()
        );
        // One API per layout, so every case states the same type under its own name and
        // none of them reads what another case stored.
        let name = format!("list-types-drawn-{}", layout.label().replace(' ', "-"));
        let api_id = create_api(&router, &name).await;
        let written = layout.written(&drawn);
        let created = create_type(&router, &api_id, &written, state::SDL_FORMAT).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected CreateType 200, got {created:?}"
        );
        assert_eq!(
            definition_of(&created.body["type"]),
            written,
            "case {case}: expected the create to answer the stated definition, got {:?}",
            definition_of(&created.body["type"])
        );

        assert_reported(&router, &api_id, &drawn, &written, &case).await;

        // Listing is a read: asking again reports the same document.
        let again = list_types(&router, &api_id, state::SDL_FORMAT).await;
        assert_eq!(
            definition_of(&listed(&again, &drawn.name, &case)),
            drawn.rendered(),
            "case {case}: expected a repeated listing to report the same document, got {again:?}"
        );
    }
}
