// SPDX-License-Identifier: CC0-1.0

//! The FORM an `APIKey` value takes, end to end over the wire.
//!
//! `CreateAPIKey` "creates an API key that contains a set of token domains" and
//! answers "the generated, encrypted API key", and `GetDecryptedAPIKey` "returns
//! your API key in decrypted form" so a caller "can check the token domains that you
//! have defined for the key". The RULE asserted here is what that makes the value:
//! an opaque sealed token, handed out in ONE rendering and at ONE width for every
//! key, whose definition is read back from the key's own record rather than spelled
//! into the token. Concretely, for every definition a key can be minted over:
//!
//! * the value satisfies `APIKey`'s own modelled form (`length 1..2048`,
//!   `pattern \S`) and the form this service mints a key in;
//! * it is rendered in the standard base64 alphabet (`A-Za-z0-9+/`) with that
//!   alphabet's `=` padding, and carries no character outside it;
//! * it is the same width for every definition — one domain or the documented
//!   maximum of five, the shortest domain the model allows or the longest — so a
//!   key's width tells a reader nothing about what it was defined over;
//! * it reads back to exactly that definition, through `GetDecryptedAPIKey`;
//! * two different definitions are two different keys, and no two mints answer one
//!   value by accident.
//!
//! Each suite asserts that over a TABLE spanning the class: both scopes, one to the
//! documented maximum of five token domains, the shortest and the longest domain the
//! model allows, and the punctuation `TokenDomain`'s pattern permits. A table keeps
//! within [`MAX_API_KEYS_PER_SCOPE`] keys per scope, the bound a mint is refused
//! past.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::{
    is_api_key_shaped, API_KEY_ENVELOPE_LEN, MAX_API_KEYS_PER_SCOPE,
    MAX_TOKEN_DOMAINS_PER_API_KEY, SCOPE_CLOUDFRONT, SCOPE_REGIONAL,
};

use crate::web_acl_lifecycle::Waf;

/// The bounds `APIKey` allows (`smithy_query(query="shape",shape="APIKey")` →
/// `length 1..2048`, `pattern \S`).
const MAX_API_KEY_LENGTH: usize = 2048;

/// The longest value `TokenDomain` allows
/// (`smithy_query(query="shape",shape="TokenDomain")` → `length 1..253`, pattern
/// `^[\w\.\-/]+$`).
const MAX_TOKEN_DOMAIN_LENGTH: usize = 253;

/// The padding character base64 completes a rendering with.
const BASE64_PAD: char = '=';

/// One definition a key is minted over.
struct Definition {
    /// What the row is called in an assertion.
    case: String,
    /// The scope the key is defined in.
    scope: &'static str,
    /// The token domains the key is defined over.
    domains: Vec<String>,
}

/// A domain of exactly `width` characters, within `TokenDomain`'s own pattern, and
/// distinct per `label` so two rows are two definitions.
fn domain_of_width(label: &str, width: usize) -> String {
    let mut domain = format!("w{width}x{label}");
    domain.truncate(width);
    let padding = width.saturating_sub(domain.len());
    format!("{domain}{}", "d".repeat(padding))
}

/// The table: every way a definition can differ — how many domains it names, how
/// long each is, which punctuation they use, and which scope the key lives in.
fn definitions() -> Vec<Definition> {
    let longest = format!(
        "{}.example.com",
        "d".repeat(MAX_TOKEN_DOMAIN_LENGTH - ".example.com".len())
    );
    vec![
        Definition {
            case: "one domain, REGIONAL".to_string(),
            scope: SCOPE_REGIONAL,
            domains: vec!["example.com".to_string()],
        },
        Definition {
            case: "one domain, CLOUDFRONT".to_string(),
            scope: SCOPE_CLOUDFRONT,
            domains: vec!["example.com".to_string()],
        },
        Definition {
            case: "the shortest domain the model allows".to_string(),
            scope: SCOPE_REGIONAL,
            domains: vec!["a".to_string()],
        },
        Definition {
            case: "the longest domain the model allows".to_string(),
            scope: SCOPE_REGIONAL,
            domains: vec![longest.clone()],
        },
        Definition {
            case: "two domains".to_string(),
            scope: SCOPE_REGIONAL,
            domains: vec!["one.example.com".to_string(), "two.example.com".to_string()],
        },
        Definition {
            case: "the punctuation the domain pattern permits".to_string(),
            scope: SCOPE_REGIONAL,
            domains: vec!["a_b-c.example.com/path".to_string()],
        },
        Definition {
            case: "the documented maximum of five domains".to_string(),
            scope: SCOPE_CLOUDFRONT,
            domains: (0..MAX_TOKEN_DOMAINS_PER_API_KEY)
                .map(|index| format!("host{index}.example.com"))
                .collect(),
        },
        Definition {
            case: "five domains, each as long as the model allows".to_string(),
            scope: SCOPE_REGIONAL,
            domains: (0..MAX_TOKEN_DOMAINS_PER_API_KEY)
                .map(|index| format!("{index}{}", &longest[1..]))
                .collect(),
        },
    ]
}

/// The table the width rule is asserted over: the two dimensions a definition can
/// grow along — how long one domain is, from `TokenDomain`'s shortest to its
/// longest, and how many domains there are, up to the documented maximum of five —
/// spread over both scopes so no scope is filled past its bound.
fn width_cases() -> Vec<Definition> {
    let mut table: Vec<Definition> = Vec::new();
    for (index, width) in [1_usize, 2, 17, 64, MAX_TOKEN_DOMAIN_LENGTH]
        .into_iter()
        .enumerate()
    {
        table.push(Definition {
            case: format!("a single domain of {width} characters"),
            scope: if index % 2 == 0 {
                SCOPE_REGIONAL
            } else {
                SCOPE_CLOUDFRONT
            },
            domains: vec![domain_of_width("one", width)],
        });
    }
    for count in 1..=MAX_TOKEN_DOMAINS_PER_API_KEY {
        table.push(Definition {
            case: format!("{count} domains"),
            scope: if count % 2 == 0 {
                SCOPE_REGIONAL
            } else {
                SCOPE_CLOUDFRONT
            },
            domains: (0..count)
                .map(|index| format!("c{count}-host{index}.example.com"))
                .collect(),
        });
    }
    // The largest definition a key can be minted over at all: the documented
    // maximum of domains, each as long as the model allows.
    table.push(Definition {
        case: format!(
            "{MAX_TOKEN_DOMAINS_PER_API_KEY} domains of \
             {MAX_TOKEN_DOMAIN_LENGTH} characters"
        ),
        scope: SCOPE_REGIONAL,
        domains: (0..MAX_TOKEN_DOMAINS_PER_API_KEY)
            .map(|index| domain_of_width(&format!("max{index}"), MAX_TOKEN_DOMAIN_LENGTH))
            .collect(),
    });
    for scope in [SCOPE_REGIONAL, SCOPE_CLOUDFRONT] {
        let mints = table.iter().filter(|case| case.scope == scope).count();
        assert!(
            mints <= MAX_API_KEYS_PER_SCOPE,
            "the table must stay within {MAX_API_KEYS_PER_SCOPE} keys per scope, got \
             {mints} in {scope}"
        );
    }
    table
}

/// Mint one key; the call must succeed and answer a key value.
async fn mint(waf: &Waf, definition: &Definition) -> String {
    let answered = waf
        .ok(
            "CreateAPIKey",
            json!({ "Scope": definition.scope, "TokenDomains": definition.domains }),
        )
        .await;
    answered["APIKey"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: a mint must answer an APIKey, got {answered}",
                definition.case
            )
        })
        .to_string()
}

/// The token domains `GetDecryptedAPIKey` reports for `api_key`.
async fn decrypted_domains(waf: &Waf, scope: &str, api_key: &str) -> Vec<String> {
    let decrypted: Value = waf
        .ok(
            "GetDecryptedAPIKey",
            json!({ "Scope": scope, "APIKey": api_key }),
        )
        .await;
    decrypted["TokenDomains"]
        .as_array()
        .expect("GetDecryptedAPIKey must report TokenDomains")
        .iter()
        .map(|domain| domain.as_str().unwrap_or_default().to_string())
        .collect()
}

/// How many bytes a key's value carries, decoded by the padded standard-alphabet
/// coder a key is rendered with: the size of the token itself rather than of its
/// rendering. That coder requires canonical padding, so a value it decodes at all is
/// a padded standard-base64 rendering.
fn carried_bytes(api_key: &str, case: &str) -> usize {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD
        .decode(api_key)
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: a key must be a padded standard-base64 token, got \
                 {api_key:?} ({error})"
            )
        })
        .len()
}

/// Every minted key satisfies `APIKey`'s modelled form, is rendered in the standard
/// base64 alphabet with that alphabet's padding, and carries the form this service
/// mints a key in — whatever definition it was minted over.
#[tokio::test]
async fn a_minted_key_has_the_modelled_form() {
    let waf = Waf::new();
    for definition in definitions() {
        let case = definition.case.clone();
        let api_key = mint(&waf, &definition).await;
        assert!(
            (1..=MAX_API_KEY_LENGTH).contains(&api_key.len()),
            "case {case:?}: the key must satisfy APIKey's length 1..{MAX_API_KEY_LENGTH}, \
             got {} characters",
            api_key.len()
        );
        assert!(
            !api_key.chars().any(char::is_whitespace),
            "case {case:?}: the key must satisfy APIKey's pattern, got {api_key:?}"
        );
        assert!(
            is_api_key_shaped(&api_key),
            "case {case:?}: the key must carry the form this service mints, got \
             {api_key:?}"
        );

        // The standard alphabet, and nothing outside it: no URL-safe substitute
        // characters, and the padding only where padding goes.
        let body = api_key.trim_end_matches(BASE64_PAD);
        assert!(
            body.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '+' || character == '/'
            }),
            "case {case:?}: the key must be rendered over A-Za-z0-9+/, got {api_key:?}"
        );
        assert_eq!(
            api_key.len() % 4,
            0,
            "case {case:?}: a padded rendering must be a multiple of four characters, \
             got {} in {api_key:?}",
            api_key.len()
        );

        // The padding a rendering of this many bytes takes, which is what makes the
        // value the padded standard form rather than the unpadded one.
        let carried = carried_bytes(&api_key, &case);
        let expected_padding = (3 - carried % 3) % 3;
        assert_eq!(
            api_key.len() - body.len(),
            expected_padding,
            "case {case:?}: a rendering of {carried} bytes must carry \
             {expected_padding} padding characters, got {api_key:?}"
        );
    }
}

/// The key's WIDTH is a property of the key, not of the definition it was minted
/// for: every key this service mints carries the one envelope width the service
/// renders a key at and is the same number of characters, so neither how many token
/// domains a request named nor how long they are can be read off the value.
#[tokio::test]
async fn a_minted_keys_width_does_not_depend_on_its_definition() {
    let waf = Waf::new();
    let mut first: Option<(String, usize, usize)> = None;
    for definition in width_cases() {
        let case = definition.case.clone();
        let api_key = mint(&waf, &definition).await;
        let widths = (api_key.len(), carried_bytes(&api_key, &case));
        // The absolute width, not only agreement between rows: a key carries the one
        // envelope every key is sealed in.
        assert_eq!(
            widths.1, API_KEY_ENVELOPE_LEN,
            "case {case:?}: a key must carry the one envelope width \
             ({API_KEY_ENVELOPE_LEN} bytes), got {} from {api_key:?}",
            widths.1
        );
        match &first {
            None => first = Some((case, widths.0, widths.1)),
            Some((first_case, rendered, carried)) => {
                assert_eq!(
                    widths,
                    (*rendered, *carried),
                    "case {case:?}: a key must be the same width as the key of case \
                     {first_case:?} ({rendered} characters, {carried} bytes), got \
                     {widths:?}"
                );
            }
        }
    }
    assert!(
        first.is_some(),
        "the table must mint at least one key, got none"
    );
}

/// A key reads back to exactly the definition it was minted over — the set of token
/// domains, in the one order a key's record stores them in — and no two definitions
/// share a key.
#[tokio::test]
async fn a_key_decrypts_back_to_its_own_definition() {
    let waf = Waf::new();
    let mut minted: Vec<String> = Vec::new();
    for definition in definitions() {
        let case = definition.case.clone();
        let api_key = mint(&waf, &definition).await;
        let mut expected = definition.domains.clone();
        expected.sort();
        expected.dedup();
        let reported = decrypted_domains(&waf, definition.scope, &api_key).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: the key must read back to the domains it was defined \
             over, got {reported:?}"
        );
        assert!(
            !minted.contains(&api_key),
            "case {case:?}: two definitions must not share a key, got {api_key:?}"
        );
        minted.push(api_key);
    }
}
