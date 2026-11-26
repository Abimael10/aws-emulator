// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of HOW a dynamic instrumentation configuration's `LocationHash` is
//! DERIVED, over HTTP.
//!
//! The rule under test: the hash is the 64-bit FNV-1a digest of the code location rendered as
//! one compact JSON object — the members `CodeLocation` declares, under their lowerCamel
//! names, in the order the shape declares them, string values as JSON string literals,
//! `LineNumber` as a JSON number, and a member the location does not carry contributing no key
//! at all — written as 16 lowercase hex characters. Nothing but the location's own members
//! feeds it, so the value is a pure function of the code location: a client can compute it
//! from its own source coordinates BEFORE any create and address the configuration by it
//! (`LocationIdentifier.LocationHash`, "The pre-computed location hash (16-character hex
//! string)"), and the hash the create answers — the one its ARN's last segment carries — is
//! the same value.
//!
//! Every row below derives its expected hash in this suite, from the request body it sends,
//! rather than reading it off the response: that is what makes the assertions a statement
//! about the derivation rather than about whatever the service happened to answer. One further
//! case anchors that derivation to the service being emulated: a code location an AWS account
//! was observed at, with the hash AWS answered for it
//! ([`the_rule_reproduces_the_hash_observed_from_aws`]).
//!
//! The rows differ along what the rule generalises over: WHICH of `CodeLocation`'s optional
//! members the location carries (all of them, each one left out, none of them), the
//! programming LANGUAGE (the whole modelled catalogue —
//! `smithy_query(query="shape",shape="ProgrammingLanguage")` -> `["Java", "Python",
//! "Javascript"]`), the LINE NUMBER (the modelled `range min 1` bound and the largest value
//! the modelled `Integer` holds), the LENGTH of a value (the modelled maxima of `CodeUnit`,
//! 128, and of `MethodName`, 80), and the CHARACTERS a value carries (a quote, a backslash and
//! non-ASCII text, plus a pair of locations a separator-joined rendering would collide on).
//!
//! What the hash identifies, which scope it is independent of, and the ARN that embeds it are
//! `instrumentation_configurations.rs` and `instrumentation_configuration_arn.rs`; this suite
//! owns the derivation itself.

use std::collections::BTreeMap;
use std::hash::Hasher;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::slo_lifecycle::{call, router};

/// The only `DynamicInstrumentationSignalType` the model declares
/// (`smithy_query(query="shape",shape="DynamicInstrumentationSignalType")` -> `["SNAPSHOT"]`).
const SNAPSHOT: &str = "SNAPSHOT";

/// Two of the `InstrumentationType` values
/// (`smithy_query(query="shape",shape="InstrumentationType")` -> `["BREAKPOINT", "PROBE"]`).
const BREAKPOINT: &str = "BREAKPOINT";
const PROBE: &str = "PROBE";

/// The only `DynamicInstrumentationDeletionStatus` the model declares
/// (`smithy_query(query="shape",shape="DynamicInstrumentationDeletionStatus")` ->
/// `["DELETED"]`).
const DELETED: &str = "DELETED";

/// `CodeLocation`'s members, in the order the shape declares them, each paired with the
/// lowerCamel key the digest input carries it under
/// (`smithy_query(query="shape",shape="CodeLocation")` -> `Language`, `CodeUnit`, `ClassName`,
/// `MethodName`, `FilePath`, `LineNumber`).
const DIGEST_MEMBERS: [(&str, &str); 6] = [
    ("Language", "language"),
    ("CodeUnit", "codeUnit"),
    ("ClassName", "className"),
    ("MethodName", "methodName"),
    ("FilePath", "filePath"),
    ("LineNumber", "lineNumber"),
];

/// The `LocationHash` the rule gives `code_location`, derived here from the request body:
/// the 64-bit FNV-1a digest of the compact JSON object of the members the location carries,
/// as 16 lowercase hex characters.
fn expected_location_hash(code_location: &Value) -> String {
    let mut members: Vec<String> = Vec::new();
    for (member, key) in DIGEST_MEMBERS {
        let value = &code_location[member];
        if value.is_null() {
            continue;
        }
        // `Value::to_string` is the compact JSON rendering of one value: a string with its
        // quotes and escapes, a number bare.
        members.push(format!("{}:{}", Value::from(key), value));
    }
    let rendered = format!("{{{}}}", members.join(","));

    let mut hasher = fnv::FnvHasher::default();
    hasher.write(rendered.as_bytes());
    format!("{:016x}", hasher.finish())
}

/// Whether `value` is spelled the way a `LocationHash` is: 16 lowercase hex characters, both
/// the member's modelled `length 16..16` and the `InstrumentationConfigurationArn` pattern's
/// `[0-9a-f]{16}` suffix.
fn is_location_hash(value: &str) -> bool {
    value.len() == 16
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `POST /create-instrumentation-configuration`.
async fn create(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(
        router,
        "POST",
        "/create-instrumentation-configuration",
        Some(body),
    )
    .await
}

/// `POST /get-instrumentation-configuration`.
async fn get(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(
        router,
        "POST",
        "/get-instrumentation-configuration",
        Some(body),
    )
    .await
}

/// `POST /delete-instrumentation-configuration`.
async fn delete(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(
        router,
        "POST",
        "/delete-instrumentation-configuration",
        Some(body),
    )
    .await
}

/// One row: a code location, and the scope the configuration at it is created in.
#[derive(Clone, Debug)]
struct Case {
    label: &'static str,
    service: String,
    instrumentation_type: &'static str,
    /// The `CodeLocation` body of the `Location` union.
    location: Value,
}

impl Case {
    /// The create request this row sends.
    fn create_body(&self) -> Value {
        json!({
            "InstrumentationType": self.instrumentation_type,
            "Service": self.service,
            "Environment": ENVIRONMENT,
            "SignalType": SNAPSHOT,
            "Location": { "CodeLocation": self.location.clone() },
            // `CaptureConfiguration`'s required union member, with only its own required
            // member (`smithy_query(query="shape",shape="CodeCaptureConfiguration")`).
            "CaptureConfiguration": { "CodeCapture": { "CaptureLimits": {} } },
        })
    }

    /// A request naming this row's configuration by a `LocationHash` — the hash a client holds
    /// before it has ever read a response, in the rows below.
    fn by_hash(&self, location_hash: &str) -> Value {
        json!({
            "InstrumentationType": self.instrumentation_type,
            "Service": self.service,
            "Environment": ENVIRONMENT,
            "SignalType": SNAPSHOT,
            "LocationIdentifier": { "LocationHash": location_hash },
        })
    }

    /// A request naming this row's configuration by its full code location, which the service
    /// hashes itself ("The full code location specification (will be hashed internally)").
    fn by_location(&self) -> Value {
        json!({
            "InstrumentationType": self.instrumentation_type,
            "Service": self.service,
            "Environment": ENVIRONMENT,
            "SignalType": SNAPSHOT,
            "LocationIdentifier": { "CodeLocation": self.location.clone() },
        })
    }
}

/// The environment every row's configuration lives in: the hash is a function of the location
/// alone, so one environment is enough and each row takes a service of its own to keep the
/// identities apart.
const ENVIRONMENT: &str = "eks:cluster-prod/namespace";

/// The rows the rule is asserted over.
fn cases() -> Vec<Case> {
    // The modelled maxima of two members whose length the digest input carries verbatim
    // (`smithy_query(query="shape",shape="CodeLocation")`: `CodeUnit` length 1..128,
    // `MethodName` length 1..80).
    let longest_code_unit = "u".repeat(128);
    let longest_method_name = "m".repeat(80);

    vec![
        Case {
            label: "Java, every member present",
            service: "every-member".to_string(),
            instrumentation_type: BREAKPOINT,
            location: json!({
                "Language": "Java",
                "CodeUnit": "com.amazon.payment",
                "ClassName": "PaymentProcessor",
                "MethodName": "validateCreditCard",
                "FilePath": "src/payment/PaymentProcessor.java",
                "LineNumber": 42,
            }),
        },
        Case {
            label: "Java, the same location one line further down",
            service: "every-member-next-line".to_string(),
            instrumentation_type: BREAKPOINT,
            location: json!({
                "Language": "Java",
                "CodeUnit": "com.amazon.payment",
                "ClassName": "PaymentProcessor",
                "MethodName": "validateCreditCard",
                "FilePath": "src/payment/PaymentProcessor.java",
                "LineNumber": 43,
            }),
        },
        Case {
            label: "Java, no CodeUnit, no MethodName, no LineNumber",
            service: "class-only".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Java",
                "ClassName": "PaymentProcessor",
                "FilePath": "src/payment/PaymentProcessor.java",
            }),
        },
        Case {
            label: "Python, no ClassName, no LineNumber",
            service: "module-function".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                // A Python location names the module its function lives in.
                "Language": "Python",
                "CodeUnit": "app.pricing.quote",
                "MethodName": "quote",
                "FilePath": "app/pricing/quote.py",
            }),
        },
        Case {
            label: "Javascript, the required members only",
            service: "required-only".to_string(),
            instrumentation_type: BREAKPOINT,
            location: json!({
                "Language": "Javascript",
                "FilePath": "src/checkout/cart.js",
            }),
        },
        Case {
            label: "Javascript, the modelled smallest LineNumber",
            service: "first-line".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Javascript",
                "MethodName": "total",
                "FilePath": "src/checkout/cart.js",
                // The member's modelled `range min 1`.
                "LineNumber": 1,
            }),
        },
        Case {
            label: "Javascript, the largest LineNumber the modelled Integer holds",
            service: "last-line".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Javascript",
                "MethodName": "total",
                "FilePath": "src/checkout/cart.js",
                "LineNumber": i32::MAX,
            }),
        },
        Case {
            label: "Python, the modelled longest CodeUnit and MethodName",
            service: "longest-values".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Python",
                "CodeUnit": longest_code_unit,
                "MethodName": longest_method_name,
                "FilePath": "app/pricing/quote.py",
                "LineNumber": 7,
            }),
        },
        Case {
            label: "Python, a path carrying a quote, a backslash and non-ASCII text",
            service: "escaped-path".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Python",
                "CodeUnit": "app.caf\u{e9}",
                "FilePath": "app/\"caf\u{e9}\"\\quote.py",
                "LineNumber": 9,
            }),
        },
        // The next two locations are the pair a separator-joined rendering of the members
        // would collide on: the first's `CodeUnit` is `x` and its `ClassName` is `y`, the
        // second's single `CodeUnit` spells both of them with the rendering's own punctuation
        // in between. They are two instrumentation points and must hash differently.
        Case {
            label: "Python, a code unit and a class name",
            service: "unit-and-class".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Python",
                "CodeUnit": "x",
                "ClassName": "y",
                "FilePath": "app/collide.py",
            }),
        },
        Case {
            label: "Python, one code unit spelling both of them",
            service: "unit-spelling-both".to_string(),
            instrumentation_type: PROBE,
            location: json!({
                "Language": "Python",
                "CodeUnit": "x\",\"className\":\"y",
                "FilePath": "app/collide.py",
            }),
        },
    ]
}

/// One code location an AWS account was observed at, with the `LocationHash` AWS answered for
/// it: the ground truth the rule above is checked against, so a derivation that drifted from
/// the service's own — a different key spelling, member order, rendering or digest — fails here
/// even though it would still be self-consistent.
///
/// Both halves are properties of the LOCATION, not of that one call: the hash carries nothing
/// from the account, the region, the clock or the request that observed it, which is what makes
/// the pair assertable at all.
const OBSERVED_LOCATION_HASH: &str = "fc14dd5a04207b41";

/// The code location [`OBSERVED_LOCATION_HASH`] was observed for.
fn observed_location() -> Value {
    json!({
        "Language": "Python",
        "CodeUnit": "app.handler",
        "ClassName": "Handler",
        "MethodName": "process",
        "FilePath": "/src/app/handler.py",
        "LineNumber": 10,
    })
}

/// The rule reproduces the hash AWS answered for the observed location, and so does the
/// service: the derivation is the service's, not merely a stable one.
#[tokio::test]
async fn the_rule_reproduces_the_hash_observed_from_aws() {
    let location = observed_location();
    assert_eq!(
        expected_location_hash(&location),
        OBSERVED_LOCATION_HASH,
        "expected the rule to reproduce the observed hash of {location}"
    );

    let router = router();
    let (status, created) = create(
        &router,
        json!({
            "InstrumentationType": PROBE,
            "Service": "observed",
            "Environment": ENVIRONMENT,
            "SignalType": SNAPSHOT,
            "Location": { "CodeLocation": location.clone() },
            "CaptureConfiguration": { "CodeCapture": { "CaptureLimits": {} } },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from the create, got {status} with {created}"
    );
    assert_eq!(
        created["LocationHash"].as_str(),
        Some(OBSERVED_LOCATION_HASH),
        "expected the service to answer the observed hash of {location}, got {:?}",
        created["LocationHash"]
    );
    let arn = created["ARN"].as_str().unwrap_or_default();
    assert_eq!(
        arn.rsplit('/').next(),
        Some(OBSERVED_LOCATION_HASH),
        "expected the ARN to end in the observed hash, got {arn:?}"
    );
}

/// The hash a create answers is the digest the rule gives the submitted code location, the ARN
/// it mints ends in that same hash, and the configuration is addressable by the hash a client
/// derived for itself — before the create, where it resolves nothing, and after it, where it
/// resolves this record and deletes it.
#[tokio::test]
async fn the_location_hash_is_the_rules_digest_of_the_submitted_code_location() {
    let router = router();
    let cases = cases();
    let mut by_hash: BTreeMap<String, &'static str> = BTreeMap::new();

    for case in &cases {
        let label = case.label;
        let expected = expected_location_hash(&case.location);
        assert!(
            is_location_hash(&expected),
            "case {label:?}: expected the rule to give 16 lowercase hex characters, got \
             {expected:?}"
        );

        // A client that computed the hash itself addresses nothing while no configuration is
        // stored at that location, which is what makes the lookups below meaningful.
        let (status, missing) = get(&router, case.by_hash(&expected)).await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected the pre-computed hash to resolve nothing before the \
             create, got {status} with {missing}"
        );

        let (status, created) = create(&router, case.create_body()).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the create, got {status} with {created}"
        );
        let answered = created["LocationHash"].as_str().unwrap_or_default();
        assert_eq!(
            answered, expected,
            "case {label:?}: expected the rule's digest {expected:?} of {}, got {answered:?}",
            case.location
        );
        let arn = created["ARN"].as_str().unwrap_or_default();
        assert_eq!(
            arn.rsplit('/').next(),
            Some(expected.as_str()),
            "case {label:?}: expected the ARN to end in the location hash {expected:?}, got \
             {arn:?}"
        );
        let clash = by_hash.insert(expected.clone(), label);
        assert!(
            clash.is_none(),
            "case {label:?}: expected a distinct location to hash distinctly, got {expected:?} \
             for {clash:?} as well"
        );

        // The pre-computed hash now resolves the stored record, and so does the full code
        // location the service hashes itself: one derivation serves the create and the reads.
        for (how, body) in [
            ("the pre-computed hash", case.by_hash(&expected)),
            ("the full code location", case.by_location()),
        ] {
            let (status, fetched) = get(&router, body).await;
            assert_eq!(
                status, 200,
                "case {label:?} read by {how}: expected HTTP 200 from the get, got {status} with \
                 {fetched}"
            );
            assert_eq!(
                fetched["Configuration"], created,
                "case {label:?} read by {how}: expected the stored record {created}, got {:?}",
                fetched["Configuration"]
            );
        }

        // And the delete resolves it by that same pre-computed hash, which is how a client
        // tears down what it created.
        let (status, deleted) = delete(&router, case.by_hash(&expected)).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the delete, got {status} with {deleted}"
        );
        assert_eq!(
            deleted["DeletionStatus"].as_str(),
            Some(DELETED),
            "case {label:?}: expected the modelled deletion status, got {deleted}"
        );
        let (status, gone) = get(&router, case.by_hash(&expected)).await;
        assert_eq!(
            status, 404,
            "case {label:?}: expected the deleted configuration to resolve nothing, got {status} \
             with {gone}"
        );
    }

    assert_eq!(
        by_hash.len(),
        cases.len(),
        "expected one hash per location, got {by_hash:?}"
    );
}

/// The digest covers the location and nothing else: one code location created under different
/// services, environments and instrumentation types hashes to the one value the rule gives it.
#[tokio::test]
async fn the_digest_covers_the_location_and_nothing_else() {
    let location = json!({
        "Language": "Java",
        "CodeUnit": "com.amazon.payment",
        "ClassName": "PaymentProcessor",
        "MethodName": "validateCreditCard",
        "FilePath": "src/payment/PaymentProcessor.java",
        "LineNumber": 42,
    });
    let expected = expected_location_hash(&location);

    // The scopes differ in every dimension that is NOT the location: the service, the
    // environment and the instrumentation type.
    let scopes: [(&str, &str, &str); 3] = [
        ("checkout", "ec2:production", BREAKPOINT),
        ("pricing", "eks:cluster-staging/pricing", PROBE),
        ("a", "b", BREAKPOINT),
    ];

    for (service, environment, instrumentation_type) in scopes {
        let label = format!("{service}/{environment}/{instrumentation_type}");
        // A fresh store per scope, so each create is the first at this location.
        let router = router();
        let (status, created) = create(
            &router,
            json!({
                "InstrumentationType": instrumentation_type,
                "Service": service,
                "Environment": environment,
                "SignalType": SNAPSHOT,
                "Location": { "CodeLocation": location.clone() },
                "CaptureConfiguration": { "CodeCapture": { "CaptureLimits": {} } },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the create, got {status} with {created}"
        );
        assert_eq!(
            created["LocationHash"].as_str(),
            Some(expected.as_str()),
            "case {label:?}: expected one location to hash to {expected:?} in every scope, got \
             {:?}",
            created["LocationHash"]
        );
    }
}
