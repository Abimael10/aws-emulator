// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `Theme` a theme response projects, over the wire.
//!
//! Every case drives the real awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! Beside the members the Smithy model declares, AppStream reports six more on every theme
//! it answers with, and each is DERIVED rather than echoed. The rules under test, asserted
//! over a TABLE of inputs rather than over one recorded request:
//!
//! * the branding record is named after the stack it brands (`themeName`);
//! * the title is reported beside the modelled member that carries it (`titleText`);
//! * the colour theme the branding applies decides the name it is reported by
//!   (`stylingName`) and the colours it paints the catalog page with
//!   (`stylingProperty`) — a function of the `ThemeStyling` value, not of anything else
//!   the request carries — and an update that changes the colour theme repaints both;
//! * each branding asset is reported twice, under the modelled member and under the one
//!   AppStream reports beside it (`organizationLogoURL`, `faviconURL`), both addressing
//!   the S3 object the request named;
//! * every one of those members is projected identically by the create, the read and the
//!   update, because all three render one stored record.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::stack_theme::{
    assert_asset_url, base_theme_request, emulator_with_stack, ASSET_URL_MEMBERS,
};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The values `ThemeStyling` models, written out here so the table tracks the modelled set
/// rather than a copy of the service's own catalog: LIGHT_BLUE, BLUE, PINK and RED
/// (`smithy_query(query="shape", shape="ThemeStyling")`, also kept as
/// `THEME_STYLING_VALUES` in `src/ops/types/shared_types.rs`).
const STYLINGS: [&str; 4] = ["LIGHT_BLUE", "BLUE", "PINK", "RED"];

/// The colour theme the recorded AppStream response establishes the projection of, and the
/// two values it reports for it: the name, and the colour map with that accent.
const RECORDED_STYLING: &str = "BLUE";
const RECORDED_STYLING_NAME: &str = "Blue (#0070BA)";
fn recorded_styling_property() -> Value {
    json!({
        "backgroundColor": "#FFFFFF",
        "bodyTextColor": "#666666",
        "buttonColor": "#8AC53E",
        "linkColor": "#0070BA",
        "spinnerColor": "#666666",
    })
}

/// The part of the catalog page the colour theme's own accent paints, the one entry of the
/// colour map that differs between colour themes.
const ACCENT_PART: &str = "linkColor";

/// The theme `payload` reports, as a JSON object.
fn theme(case: &str, payload: &Value) -> Value {
    let theme = payload["Theme"].clone();
    assert!(
        theme.is_object(),
        "case {case:?}: expected a Theme, got {payload}"
    );
    theme
}

/// The string member `member` of `theme`.
fn member(case: &str, theme: &Value, member: &str) -> String {
    theme[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member}, got {theme}"))
        .to_string()
}

/// Assert the branding members of `theme` that do not depend on the colour theme: the
/// record's name, the title beside the modelled member, and each asset reported twice.
fn assert_record_members(case: &str, theme: &Value, stack: &str, request: &Value) {
    let expected_name = format!("theme_{stack}");
    assert_eq!(
        member(case, theme, "themeName"),
        expected_name,
        "case {case:?}: the branding record must be named after the stack it brands, \
         expected {expected_name:?}, got {theme}"
    );
    assert_eq!(
        member(case, theme, "titleText"),
        member(case, theme, "ThemeTitleText"),
        "case {case:?}: the title must be reported beside the modelled member that carries \
         it, got {theme}"
    );
    for (member_name, location) in ASSET_URL_MEMBERS {
        assert_asset_url(case, member_name, theme, &request[location]);
    }
    assert_eq!(
        member(case, theme, "organizationLogoURL"),
        member(case, theme, "ThemeOrganizationLogoURL"),
        "case {case:?}: the logo address must be reported beside the modelled member that \
         carries it, got {theme}"
    );
    assert_eq!(
        member(case, theme, "faviconURL"),
        member(case, theme, "ThemeFaviconURL"),
        "case {case:?}: the favicon address must be reported beside the modelled member \
         that carries it, got {theme}"
    );
}

/// The colour map `theme` reports, and the assertion that it is one: five entries, each a
/// six-digit hex colour, keyed by the part of the page it paints.
fn styling_property(case: &str, theme: &Value) -> BTreeMap<String, String> {
    let property = theme["stylingProperty"]
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a stylingProperty map, got {theme}"));
    let colours: BTreeMap<String, String> = property
        .iter()
        .map(|(part, colour)| {
            let colour = colour.as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected a colour for {part:?}, got {theme}")
            });
            (part.clone(), colour.to_string())
        })
        .collect();
    let parts: Vec<&str> = colours.keys().map(String::as_str).collect();
    assert_eq!(
        parts,
        recorded_styling_property()
            .as_object()
            .expect("a colour map")
            .keys()
            .map(String::as_str)
            .collect::<Vec<&str>>(),
        "case {case:?}: the colour map must be keyed by the parts of the page a colour \
         theme paints, got {theme}"
    );
    for (part, colour) in &colours {
        assert!(
            colour.len() == 7
                && colour.starts_with('#')
                && colour[1..]
                    .chars()
                    .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_lowercase()),
            "case {case:?}: {part} must be a six-digit hex colour, got {colour:?}"
        );
    }
    colours
}

/// Assert the two members the colour theme decides agree with each other: the name states
/// the accent the colour map carries.
fn assert_styling_members(case: &str, theme: &Value) -> BTreeMap<String, String> {
    let colours = styling_property(case, theme);
    let accent = colours
        .get(ACCENT_PART)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {ACCENT_PART}, got {theme}"));
    let name = member(case, theme, "stylingName");
    assert!(
        name.ends_with(&format!(" ({accent})")),
        "case {case:?}: the colour theme's name must state the accent {accent:?} the colour \
         map carries, got {name:?}"
    );
    assert!(
        name.len() > accent.len() + 3,
        "case {case:?}: the colour theme's name must name the colour before its accent, \
         got {name:?}"
    );
    colours
}

#[tokio::test]
async fn every_theme_response_carries_the_branding_members_beside_the_modelled_ones() {
    // Rows: the stack the branding belongs to, along the forms `Name`'s pattern
    // ^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$ admits — a plain name, a dotted and hyphenated
    // one, an underscored mixed-case one, and one that is only digits.
    for stack in ["branded", "brand.portal-01", "Brand_Portal_9", "0123456789"] {
        let emulator = emulator_with_stack(REGION, stack).await;
        let request = base_theme_request(stack);

        let created = theme(
            stack,
            &emulator
                .ok(stack, "CreateThemeForStack", request.clone())
                .await,
        );
        assert_record_members(stack, &created, stack, &request);

        // A read of the stored record projects the same members …
        let described = theme(
            stack,
            &emulator
                .ok(stack, "DescribeThemeForStack", json!({ "StackName": stack }))
                .await,
        );
        assert_record_members(stack, &described, stack, &request);

        // … and so does an update, with the title it just applied.
        let updated = theme(
            stack,
            &emulator
                .ok(
                    stack,
                    "UpdateThemeForStack",
                    json!({ "StackName": stack, "TitleText": "Retitled" }),
                )
                .await,
        );
        assert_record_members(stack, &updated, stack, &request);
        assert_eq!(
            member(stack, &updated, "titleText"),
            "Retitled",
            "case {stack:?}: the title reported beside the modelled member must follow the \
             update, got {updated}"
        );
    }
}

#[tokio::test]
async fn the_colour_theme_decides_the_name_and_the_colours_a_theme_reports() {
    let stack = "styled";
    let mut palettes: Vec<(&str, String, BTreeMap<String, String>)> = Vec::new();

    // Rows: every value `ThemeStyling` models, including the three the recording does not
    // cover.
    for styling in STYLINGS {
        let emulator = emulator_with_stack(REGION, stack).await;
        let mut request = base_theme_request(stack);
        request["ThemeStyling"] = json!(styling);

        let created = theme(styling, &emulator.ok(styling, "CreateThemeForStack", request).await);
        let colours = assert_styling_members(styling, &created);
        let name = member(styling, &created, "stylingName");

        // A read of the stored record reports the same palette: it follows the colour
        // theme the record holds, not the call that renders it.
        let described = theme(
            styling,
            &emulator
                .ok(styling, "DescribeThemeForStack", json!({ "StackName": stack }))
                .await,
        );
        assert_eq!(
            described["stylingName"], created["stylingName"],
            "case {styling:?}: the read must report the same colour theme name, got {described}"
        );
        assert_eq!(
            described["stylingProperty"], created["stylingProperty"],
            "case {styling:?}: the read must report the same colour map, got {described}"
        );

        if styling == RECORDED_STYLING {
            assert_eq!(
                name, RECORDED_STYLING_NAME,
                "case {styling:?}: expected the colour theme name {RECORDED_STYLING_NAME:?}, \
                 got {name:?}"
            );
            assert_eq!(
                created["stylingProperty"],
                recorded_styling_property(),
                "case {styling:?}: expected the colour map {}, got {created}",
                recorded_styling_property()
            );
        }
        palettes.push((styling, name, colours));
    }

    // Across the colour themes: each paints its own accent, and every other colour is the
    // same whichever theme is chosen.
    for (styling, name, colours) in &palettes {
        for (other, other_name, other_colours) in &palettes {
            if styling == other {
                continue;
            }
            assert_ne!(
                colours.get(ACCENT_PART),
                other_colours.get(ACCENT_PART),
                "case {styling:?}: the accent must differ from {other:?}'s, both report {:?}",
                colours.get(ACCENT_PART)
            );
            assert_ne!(
                name, other_name,
                "case {styling:?}: the colour theme name must differ from {other:?}'s"
            );
            for (part, colour) in colours {
                if part == ACCENT_PART {
                    continue;
                }
                assert_eq!(
                    Some(colour),
                    other_colours.get(part),
                    "case {styling:?}: {part} must be the same under {other:?}, got {:?}",
                    other_colours.get(part)
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_that_changes_the_colour_theme_repaints_what_it_decides() {
    let stack = "repainted";

    // Rows: the colour theme the branding is moved to, from the one the create applied.
    for styling in STYLINGS {
        let emulator = emulator_with_stack(REGION, stack).await;
        let created = theme(
            styling,
            &emulator
                .ok(styling, "CreateThemeForStack", base_theme_request(stack))
                .await,
        );

        let updated = theme(
            styling,
            &emulator
                .ok(
                    styling,
                    "UpdateThemeForStack",
                    json!({ "StackName": stack, "ThemeStyling": styling }),
                )
                .await,
        );
        let colours = assert_styling_members(styling, &updated);
        assert_eq!(
            updated["ThemeStyling"], json!(styling),
            "case {styling:?}: the update must apply the colour theme, got {updated}"
        );

        // The palette follows the NEW colour theme: it changed exactly when the colour
        // theme did.
        let same_theme = styling == base_theme_request(stack)["ThemeStyling"].as_str().unwrap();
        let changed = updated["stylingProperty"] != created["stylingProperty"]
            || updated["stylingName"] != created["stylingName"];
        assert_eq!(
            changed, !same_theme,
            "case {styling:?}: the branding members the colour theme decides must change \
             exactly when it does, created {created}, updated {updated}"
        );

        // And the change was persisted, not just reported.
        let described = theme(
            styling,
            &emulator
                .ok(styling, "DescribeThemeForStack", json!({ "StackName": stack }))
                .await,
        );
        assert_eq!(
            styling_property(styling, &described),
            colours,
            "case {styling:?}: the read must report the repainted colour map, got {described}"
        );
        assert_eq!(
            described["stylingName"], updated["stylingName"],
            "case {styling:?}: the read must report the repainted colour theme name, got \
             {described}"
        );
    }
}
