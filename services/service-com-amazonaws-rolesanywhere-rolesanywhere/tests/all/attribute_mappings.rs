// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the attribute mapping rules a profile enforces
//! (`PutAttributeMapping`, `DeleteAttributeMapping`, read back through `GetProfile`).
//!
//! The rule under test: one profile holds at most one mapping per certificate field, and a
//! write names ONE field and changes only the specifiers mapped for it — a put adds the
//! submitted specifiers to the ones that field already maps (a specifier already mapped
//! keeps its single entry, which is what makes the write `@idempotent` as the model
//! declares), a delete removes the specifiers it names, or every specifier of the field
//! when it names none, and a field left mapping nothing keeps no entry, because a mapping
//! "specifies a certificate field and one or more specifiers"
//! (`smithy_query(query="doc", shape="PutAttributeMapping")`). Both writes answer with the
//! stored profile, so the next read agrees with them, and both refuse an id that resolves
//! to no profile with the modelled 404 rather than writing anything, carrying no `message`
//! member at all, which the model allows because `ResourceNotFoundException$message` is
//! optional (`smithy_query(query="shape", shape="ResourceNotFoundException")`).
//!
//! And a put maps only specifiers the field it names can CARRY: a mapping is "a single
//! mapping entry for each supported specifier or sub-field" (`MappingRule` doc), so a
//! distinguished-name field (the subject, the issuer) takes an attribute type while the
//! subject-alternative name takes one of the general-name forms that extension holds. A
//! specifier the field does not carry is refused 400 `ValidationException` naming the
//! specifier and the field's target, and nothing is mapped. The rule holds for EACH entry
//! of the `mappingRules` list one call carries, so one unusable entry refuses the whole
//! call and the entries beside it are not mapped either.
//!
//! Every expectation below is the rule applied to the mappings the profile held BEFORE the
//! call, read back over the wire — never a literal list of mappings — so the suite states
//! the rule rather than one store's starting point.
//!
//! The tables differ along what the rule generalises over: WHICH certificate field is
//! written (every value the modelled enum accepts), how many specifiers one call carries,
//! whether the call repeats or overlaps specifiers the field already maps, which specifiers
//! a delete names (some, all, none, one that was never mapped), and how long a specifier is
//! (the modelled bound and one past it).
//!
//! The harness below is local to this suite, as in each sibling suite.
//!
//! The last two suites here are about how a refusal of this service QUOTES the requirement
//! a value failed, rather than about mappings: a `@pattern` is quoted as the expression the
//! service publishes (the model's text without the whole-value anchors it wraps it in) and
//! an enumerated type as the values it admits in the order this service states them. Both
//! are properties of the SHAPE a member targets, so the tables span this model's patterned
//! shapes and enumerated types — `PutAttributeMapping$certificateField`, the recorded one,
//! among them — over several operations, bindings and depths.

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region the suite signs for unless a case varies it.
const REGION: &str = "us-west-2";

/// The account every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// Every value the modelled `CertificateField` enum accepts
/// (`smithy_query(query="shape", shape="CertificateField")` reports
/// `["x509Subject", "x509Issuer", "x509SAN"]`).
const CERTIFICATE_FIELDS: [&str; 3] = ["x509Subject", "x509Issuer", "x509SAN"];

/// The longest specifier the model accepts: `MappingRule$specifier` is `@length(max 60)`
/// (`smithy_query(query="shape", shape="MappingRule")`).
const MAX_SPECIFIER_LEN: usize = 60;

/// Ids shaped like the modelled `Uuid` (`@length(36, 36)`,
/// `@pattern("[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}")`) that no profile is created
/// under, so the boundary validator accepts them and the handler has to resolve them.
const UNMINTED_IDS: [(&str, &str); 3] = [
    ("all-zero-id", "00000000-0000-0000-0000-000000000000"),
    ("hex-words-id", "deadbeef-dead-beef-dead-beefdeadbeef"),
    ("all-f-id", "ffffffff-ffff-ffff-ffff-ffffffffffff"),
];

/// One profile's attribute mappings as `(certificateField, [specifier])` pairs, in the
/// order the response lists them.
type Mappings = Vec<(String, Vec<String>)>;

/// A client over one emulator: one engine, one mounted service, so every request in a test
/// observes the state the previous one wrote.
struct Client {
    router: MultiServiceRouter,
}

/// One HTTP answer: the status, the parsed JSON body and the wire error code.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    error_code: String,
}

impl Answer {
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }

    /// The `profile` member of a response.
    fn profile(&self) -> &Value {
        self.body.get("profile").unwrap_or(&Value::Null)
    }
}

impl Client {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_rolesanywhere_rolesanywhere::EmulatedService::new(engine),
        );
        Self { router }
    }

    async fn send_in_region(
        &self,
        method: &str,
        uri: &str,
        body: Option<Value>,
        region: &str,
    ) -> Answer {
        let payload = body
            .map(|value| bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/rolesanywhere/aws4_request, \
                     SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(payload)
            .expect("build request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error_code = if header_code.is_empty() {
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .rsplit(['#', '/'])
                .next()
                .unwrap_or_default()
                .to_string()
        } else {
            header_code
        };
        Answer {
            status,
            body,
            error_code,
        }
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        self.send_in_region(method, uri, body, REGION).await
    }

    async fn create_profile(&self, name: &str) -> Answer {
        self.send(
            "POST",
            "/profiles",
            Some(json!({
                "name": name,
                "roleArns": [format!("arn:aws:iam::{ACCOUNT}:role/DemoRole")],
            })),
        )
        .await
    }

    /// `PUT /profiles/{profileId}/mappings` — `certificateField` and `mappingRules` travel
    /// in the body (neither member carries an HTTP binding).
    async fn put_mapping_in_region(
        &self,
        profile_id: &str,
        certificate_field: &str,
        specifiers: &[String],
        region: &str,
    ) -> Answer {
        let rules: Vec<Value> = specifiers
            .iter()
            .map(|specifier| json!({"specifier": specifier}))
            .collect();
        self.send_in_region(
            "PUT",
            &format!("/profiles/{profile_id}/mappings"),
            Some(json!({"certificateField": certificate_field, "mappingRules": rules})),
            region,
        )
        .await
    }

    async fn put_mapping(
        &self,
        profile_id: &str,
        certificate_field: &str,
        specifiers: &[String],
    ) -> Answer {
        self.put_mapping_in_region(profile_id, certificate_field, specifiers, REGION)
            .await
    }

    /// `DELETE /profiles/{profileId}/mappings` — `certificateField` and `specifiers` are
    /// `@httpQuery` members, and a list-typed one repeats its name once per element.
    async fn delete_mapping_in_region(
        &self,
        profile_id: &str,
        certificate_field: &str,
        specifiers: Option<&[String]>,
        region: &str,
    ) -> Answer {
        let mut uri =
            format!("/profiles/{profile_id}/mappings?certificateField={certificate_field}");
        for specifier in specifiers.unwrap_or_default() {
            uri.push_str("&specifiers=");
            uri.push_str(specifier);
        }
        self.send_in_region("DELETE", &uri, None, region).await
    }

    async fn delete_mapping(
        &self,
        profile_id: &str,
        certificate_field: &str,
        specifiers: Option<&[String]>,
    ) -> Answer {
        self.delete_mapping_in_region(profile_id, certificate_field, specifiers, REGION)
            .await
    }

    async fn get_profile_in_region(&self, profile_id: &str, region: &str) -> Answer {
        self.send_in_region("GET", &format!("/profile/{profile_id}"), None, region)
            .await
    }

    async fn get_profile(&self, profile_id: &str) -> Answer {
        self.get_profile_in_region(profile_id, REGION).await
    }

    async fn delete_profile(&self, profile_id: &str) -> Answer {
        self.send("DELETE", &format!("/profile/{profile_id}"), None)
            .await
    }

    /// The mappings a stored profile holds right now, read over the wire.
    async fn mappings(&self, case: &str, profile_id: &str) -> Mappings {
        let read = self.get_profile(profile_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetProfile 200, got {read:?}"
        );
        mappings_of(read.profile())
    }
}

/// Create a profile and answer its id.
async fn profile_id(client: &Client, name: &str) -> String {
    let created = client.create_profile(name).await;
    assert_eq!(
        created.status, 201,
        "profile {name:?}: expected CreateProfile 201, got {created:?}"
    );
    created
        .body
        .pointer("/profile/profileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("profile {name:?}: no profileId in {created:?}"))
        .to_string()
}

/// The ARN a profile id carries in `region`, which is the ARN a refusal naming that id
/// words: `ProfileArn` is
/// `@pattern("^arn:aws(-[^:]+)?:rolesanywhere(:.*){2}(:profile.*)$")`
/// (`smithy_query(query="shape", shape="ProfileDetail")`, member `profileArn`).
fn profile_arn(region: &str, id: &str) -> String {
    format!("arn:aws:rolesanywhere:{region}:{ACCOUNT}:profile/{id}")
}

/// The wording an ARN-keyed miss carries.
fn arn_not_found_message(resource_arn: &str) -> String {
    format!("The item doesn't exist. arn: {resource_arn} ; aborting.")
}

/// One profile's attribute mappings, out of a `ProfileDetail` on the wire.
fn mappings_of(profile: &Value) -> Mappings {
    profile
        .get("attributeMappings")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let field = entry
                        .get("certificateField")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let specifiers = entry
                        .get("mappingRules")
                        .and_then(Value::as_array)
                        .map(|rules| {
                            rules
                                .iter()
                                .map(|rule| {
                                    rule.get("specifier")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string()
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (field, specifiers)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The specifiers one field maps, or `None` when the profile holds no entry for it.
fn specifiers_in(mappings: &Mappings, certificate_field: &str) -> Option<Vec<String>> {
    mappings
        .iter()
        .find(|(field, _)| field == certificate_field)
        .map(|(_, specifiers)| specifiers.clone())
}

/// The mappings every field OTHER than `certificate_field` holds: what a write naming that
/// one field may not change.
fn other_fields(mappings: &Mappings, certificate_field: &str) -> Mappings {
    mappings
        .iter()
        .filter(|(field, _)| field != certificate_field)
        .cloned()
        .collect()
}

/// The specifiers the rule says a field maps after a put: the ones it already mapped, in
/// the order it mapped them, then each submitted specifier that was not mapped yet, in the
/// order the request carried them, counted once however often it is repeated.
fn specifiers_after_put(before: Option<&Vec<String>>, submitted: &[String]) -> Vec<String> {
    let mut after = before.cloned().unwrap_or_default();
    for specifier in submitted {
        if !after.contains(specifier) {
            after.push(specifier.clone());
        }
    }
    after
}

/// The specifiers the rule says a field maps after a delete, or `None` when the field is
/// left mapping nothing and so keeps no entry: a request that names specifiers removes
/// exactly those, one that names none removes every specifier of the field.
fn specifiers_after_delete(
    before: Option<&Vec<String>>,
    named: Option<&[String]>,
) -> Option<Vec<String>> {
    let named = named.filter(|named| !named.is_empty())?;
    let after: Vec<String> = before
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|specifier| !named.contains(specifier))
        .collect();
    (!after.is_empty()).then_some(after)
}

/// Assert a write's own answer and the next read report the same mappings, and answer them.
async fn written_mappings(
    case: &str,
    client: &Client,
    profile_id: &str,
    written: &Answer,
) -> Mappings {
    assert_eq!(
        written.status, 200,
        "case {case:?}: expected status 200, got {written:?}"
    );
    let from_write = mappings_of(written.profile());
    let from_read = client.mappings(case, profile_id).await;
    assert_eq!(
        from_write, from_read,
        "case {case:?}: the write answered {from_write:?} but the read reports {from_read:?}"
    );
    from_write
}

/// One date-time member of a `ProfileDetail`, as nanoseconds since the epoch.
///
/// `ProfileDetail$createdAt`/`updatedAt` carry `@timestampFormat("date-time")`, so each is
/// an ISO-8601 string on this service's restJson1 wire.
fn instant(case: &str, profile: &Value, member: &str) -> i128 {
    let text = profile
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no string {member} in {profile:?}"));
    DateTime::from_str(text, Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {member} {text:?} is not an ISO-8601 date-time: {error}")
        })
        .as_nanos()
}

/// Owned specifiers, so a table row can hold them.
fn owned(specifiers: &[&str]) -> Vec<String> {
    specifiers.iter().map(|s| (*s).to_string()).collect()
}

/// A put adds the specifiers it carries to the field it names, counting a specifier the
/// field already maps once, and changes no other field.
///
/// The table spans every value the modelled `CertificateField` enum accepts, the number of
/// specifiers one call carries, a call that repeats a specifier, specifiers that name a
/// sub-field path, and the longest specifier the model accepts.
#[tokio::test]
async fn put_adds_the_specifiers_it_carries_to_the_field_it_names() {
    // The longest specifier the model accepts that the subject-alternative name can also
    // carry: a directory-name path, padded to the bound.
    let longest = format!(
        "Name/{}",
        "s".repeat(MAX_SPECIFIER_LEN - "Name/".len())
    );
    let cases: Vec<(&str, &str, Vec<String>)> = vec![
        ("one-specifier-on-subject", "x509Subject", owned(&["CN"])),
        (
            "three-specifiers-on-subject",
            "x509Subject",
            owned(&["CN", "OU", "UID"]),
        ),
        (
            "repeated-specifier-on-issuer",
            "x509Issuer",
            owned(&["CN", "CN", "O"]),
        ),
        (
            "sub-field-paths-on-san",
            "x509SAN",
            owned(&["Name/CN", "Name/OU"]),
        ),
        (
            "longest-modelled-specifier-on-san",
            "x509SAN",
            vec![longest.clone()],
        ),
    ];

    let client = Client::new();
    for (case, field, submitted) in cases {
        let id = profile_id(&client, case).await;
        let before = client.mappings(case, &id).await;

        let written = client.put_mapping(&id, field, &submitted).await;
        let after = written_mappings(case, &client, &id, &written).await;

        let expected = specifiers_after_put(specifiers_in(&before, field).as_ref(), &submitted);
        assert_eq!(
            specifiers_in(&after, field).as_ref(),
            Some(&expected),
            "case {case:?}: expected {field:?} to map {expected:?}, got {after:?}"
        );
        // Each submitted specifier is mapped exactly once, however often it was carried.
        for specifier in &submitted {
            let occurrences = expected.iter().filter(|held| *held == specifier).count();
            assert_eq!(
                occurrences, 1,
                "case {case:?}: expected {specifier:?} to be mapped once, got {occurrences} in \
                 {expected:?}"
            );
        }
        // A put names one field, so no other field's mapping changed.
        assert_eq!(
            other_fields(&after, field),
            other_fields(&before, field),
            "case {case:?}: a put on {field:?} changed another field, before {before:?}, after \
             {after:?}"
        );
        for other in CERTIFICATE_FIELDS.iter().filter(|other| **other != field) {
            assert_eq!(
                specifiers_in(&after, other),
                specifiers_in(&before, other),
                "case {case:?}: {other:?} was not named by the put, so it must be unchanged, got \
                 {after:?}"
            );
        }
    }
}

/// Putting a field again ADDS to the specifiers it already maps, so a disjoint put grows
/// the field, an overlapping put adds only what was missing, and an identical put changes
/// nothing — one profile walked through a sequence of puts.
#[tokio::test]
async fn putting_a_field_again_adds_to_it_and_repeats_change_nothing() {
    let client = Client::new();
    let id = profile_id(&client, "union").await;

    // (case, field, the specifiers this put carries)
    let steps: Vec<(&str, &str, Vec<String>)> = vec![
        ("first-put", "x509Subject", owned(&["CN"])),
        ("disjoint-put-adds", "x509Subject", owned(&["OU"])),
        (
            "overlapping-put-adds-only-the-new-one",
            "x509Subject",
            owned(&["CN", "UID"]),
        ),
        (
            "identical-put-is-idempotent",
            "x509Subject",
            owned(&["CN", "UID"]),
        ),
        (
            "another-field-is-its-own-entry",
            "x509Issuer",
            owned(&["O", "C"]),
        ),
        (
            "a-put-of-what-is-mapped-changes-nothing",
            "x509Subject",
            owned(&["CN"]),
        ),
    ];

    for (case, field, submitted) in steps {
        let before = client.mappings(case, &id).await;
        let written = client.put_mapping(&id, field, &submitted).await;
        let after = written_mappings(case, &client, &id, &written).await;

        let expected = specifiers_after_put(specifiers_in(&before, field).as_ref(), &submitted);
        assert_eq!(
            specifiers_in(&after, field).as_ref(),
            Some(&expected),
            "case {case:?}: expected {field:?} to map {expected:?}, got {after:?}"
        );
        // Nothing the field mapped before is dropped by a put.
        for specifier in specifiers_in(&before, field).unwrap_or_default() {
            assert!(
                expected.contains(&specifier),
                "case {case:?}: a put must not drop {specifier:?}, got {expected:?}"
            );
        }
        assert_eq!(
            other_fields(&after, field),
            other_fields(&before, field),
            "case {case:?}: a put on {field:?} changed another field, got {after:?}"
        );
    }

    // One entry per written field, never two for one field.
    let held = client.mappings("one-entry-per-field", &id).await;
    let subject_entries = held
        .iter()
        .filter(|(field, _)| field == "x509Subject")
        .count();
    assert_eq!(
        subject_entries, 1,
        "case \"one-entry-per-field\": expected a single x509Subject entry, got {held:?}"
    );
}

/// One row of the delete table: the case name, the certificate field the delete names, and
/// the specifiers it names (`None` when it names none at all).
type DeleteCase = (&'static str, &'static str, Option<Vec<String>>);

/// A delete unmaps exactly the specifiers it names — every specifier of the field when it
/// names none — leaves the specifiers it does not name mapped, and drops the field's entry
/// once the field maps nothing.
///
/// Each row starts from a profile with two fields written, so the table also pins that a
/// delete touches only the field it names, and each delete is then repeated to pin that the
/// `@idempotent` operation changes nothing the second time.
#[tokio::test]
async fn delete_unmaps_only_the_specifiers_it_names() {
    let cases: Vec<DeleteCase> = vec![
        ("one-of-three", "x509Subject", Some(owned(&["OU"]))),
        ("two-of-three", "x509Subject", Some(owned(&["CN", "UID"]))),
        (
            "every-written-specifier",
            "x509Subject",
            Some(owned(&["CN", "OU", "UID"])),
        ),
        ("no-specifier-named", "x509Subject", None),
        (
            "a-specifier-the-field-never-mapped",
            "x509Subject",
            Some(owned(&["DC"])),
        ),
        (
            "another-field-entirely",
            "x509SAN",
            Some(owned(&["SAN/DNS"])),
        ),
    ];

    let client = Client::new();
    for (case, field, named) in cases {
        let id = profile_id(&client, case).await;
        for (written_field, specifiers) in [
            ("x509Subject", owned(&["CN", "OU", "UID"])),
            ("x509SAN", owned(&["Name/CN"])),
        ] {
            let seeded = client.put_mapping(&id, written_field, &specifiers).await;
            assert_eq!(
                seeded.status, 200,
                "case {case:?}: expected the seeding put for {written_field:?} to answer 200, \
                 got {seeded:?}"
            );
        }
        let before = client.mappings(case, &id).await;

        let deleted = client.delete_mapping(&id, field, named.as_deref()).await;
        let after = written_mappings(case, &client, &id, &deleted).await;

        let expected = specifiers_after_delete(
            specifiers_in(&before, field).as_ref(),
            named.as_deref(),
        );
        assert_eq!(
            specifiers_in(&after, field),
            expected,
            "case {case:?}: expected {field:?} to map {expected:?}, got {after:?}"
        );
        // Every specifier the request named is gone — every specifier of the field when it
        // named none — and every one it did not name is still mapped.
        let deletes_every_specifier = named.as_deref().is_none_or(|named| named.is_empty());
        let held = specifiers_in(&after, field).unwrap_or_default();
        for specifier in named.clone().unwrap_or_default() {
            assert!(
                !held.contains(&specifier),
                "case {case:?}: {specifier:?} was named for deletion, got {held:?}"
            );
        }
        for specifier in specifiers_in(&before, field).unwrap_or_default() {
            let deleted_now = deletes_every_specifier
                || named
                    .as_deref()
                    .is_some_and(|named| named.contains(&specifier));
            assert_eq!(
                held.contains(&specifier),
                !deleted_now,
                "case {case:?}: {specifier:?} was {}deleted by this call, got {held:?}",
                if deleted_now { "" } else { "not " }
            );
        }
        // The field the delete did not name keeps everything it mapped.
        assert_eq!(
            other_fields(&after, field),
            other_fields(&before, field),
            "case {case:?}: a delete on {field:?} changed another field, before {before:?}, \
             after {after:?}"
        );

        // Repeating the delete changes nothing: the operation is `@idempotent`.
        let repeated = client.delete_mapping(&id, field, named.as_deref()).await;
        let twice = written_mappings(&format!("{case}/repeated"), &client, &id, &repeated).await;
        assert_eq!(
            twice, after,
            "case {case:?}: repeating the delete changed the mappings from {after:?} to {twice:?}"
        );
    }
}

/// A specifier longer than the modelled bound is refused and stores nothing; one at the
/// bound is stored. The bound is `MappingRule$specifier`'s `@length(max 60)`.
#[tokio::test]
async fn a_specifier_past_the_modelled_length_bound_is_refused() {
    let client = Client::new();
    let id = profile_id(&client, "specifier-bound").await;

    for (case, len, expected_status) in [
        ("one-character", 1, 200),
        ("at-the-bound", MAX_SPECIFIER_LEN, 200),
        ("one-past-the-bound", MAX_SPECIFIER_LEN + 1, 400),
    ] {
        let specifier = "s".repeat(len);
        let answer = client
            .put_mapping(&id, "x509Subject", std::slice::from_ref(&specifier))
            .await;
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected status {expected_status}, got {answer:?}"
        );
        if expected_status == 400 {
            assert_eq!(
                answer.error_code, "ValidationException",
                "case {case:?}: expected ValidationException, got {:?}",
                answer.error_code
            );
        }

        let held = client.mappings(case, &id).await;
        let mapped = specifiers_in(&held, "x509Subject").unwrap_or_default();
        assert_eq!(
            mapped.contains(&specifier),
            expected_status == 200,
            "case {case:?}: a refused put must store nothing and an accepted one must store its \
             specifier, got {mapped:?}"
        );
    }
}

/// A put bumps the profile's `updatedAt` — "The ISO-8601 timestamp when the profile was
/// last updated" — and leaves `createdAt` as the create set it.
#[tokio::test]
async fn a_put_bumps_the_profiles_updated_at() {
    let client = Client::new();
    let created = client.create_profile("updated-at").await;
    assert_eq!(
        created.status, 201,
        "case \"create\": expected CreateProfile 201, got {created:?}"
    );
    let id = created
        .body
        .pointer("/profile/profileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"create\": no profileId in {created:?}"))
        .to_string();
    let created_at = instant("create", created.profile(), "createdAt");
    let created_updated_at = instant("create", created.profile(), "updatedAt");

    let written = client
        .put_mapping(&id, "x509Subject", &owned(&["CN"]))
        .await;
    assert_eq!(
        written.status, 200,
        "case \"put\": expected 200, got {written:?}"
    );
    let put_updated_at = instant("put", written.profile(), "updatedAt");
    assert!(
        put_updated_at > created_updated_at,
        "case \"put\": expected updatedAt to advance past {created_updated_at}, got \
         {put_updated_at}"
    );
    assert_eq!(
        instant("put", written.profile(), "createdAt"),
        created_at,
        "case \"put\": createdAt must not move, got {:?}",
        written.profile()
    );

    // The read agrees with the write, so the bump is stored rather than only echoed.
    let read = client.get_profile(&id).await;
    assert_eq!(
        instant("read-back", read.profile(), "updatedAt"),
        put_updated_at,
        "case \"read-back\": expected the stored updatedAt {put_updated_at}, got {:?}",
        read.profile()
    );
}

/// One field maps at most `MAX_MAPPING_RULES_PER_CERTIFICATE_FIELD` specifiers: a put that
/// would carry it past that is refused with the operation's modelled `ValidationException`
/// and stores nothing, while a put that fills the field exactly to the bound is stored.
#[tokio::test]
async fn a_put_past_the_specifier_ceiling_is_refused_and_stores_nothing() {
    let ceiling = service_com_amazonaws_rolesanywhere_rolesanywhere::state::MAX_MAPPING_RULES_PER_CERTIFICATE_FIELD;
    let client = Client::new();
    let id = profile_id(&client, "ceiling").await;

    // The specifiers the field maps before the first put — the service's own defaults —
    // count towards the ceiling, so the fill carries only the remainder.
    let seeded = specifiers_in(&client.mappings("ceiling", &id).await, "x509Subject")
        .unwrap_or_default()
        .len();
    // Distinct specifiers, each within `MappingRule$specifier`'s `@length(max 60)`.
    let to_the_bound: Vec<String> = (0..ceiling - seeded)
        .map(|index| format!("s{index}"))
        .collect();
    let filled = client.put_mapping(&id, "x509Subject", &to_the_bound).await;
    assert_eq!(
        filled.status, 200,
        "case \"fill-to-the-bound\": expected 200, got status {}",
        filled.status
    );
    let held = client.mappings("fill-to-the-bound", &id).await;
    let mapped = specifiers_in(&held, "x509Subject").unwrap_or_default();
    assert_eq!(
        mapped.len(),
        ceiling,
        "case \"fill-to-the-bound\": expected {ceiling} specifiers mapped, got {}",
        mapped.len()
    );

    // One specifier past it: refused, and the field still maps exactly what it did.
    let past = "one-too-many".to_string();
    let refused = client
        .put_mapping(&id, "x509Subject", std::slice::from_ref(&past))
        .await;
    assert_eq!(
        refused.status, 400,
        "case \"one-past-the-bound\": expected 400, got status {}",
        refused.status
    );
    assert_eq!(
        refused.error_code, "ValidationException",
        "case \"one-past-the-bound\": expected ValidationException, got {:?}",
        refused.error_code
    );
    let after = client.mappings("one-past-the-bound", &id).await;
    assert_eq!(
        after, held,
        "case \"one-past-the-bound\": a refused put must store nothing"
    );

    // A put of specifiers the field ALREADY maps stays at the bound rather than being
    // refused: the ceiling counts the distinct specifiers the field would map, not the
    // ones the request carries.
    let repeat = client.put_mapping(&id, "x509Subject", &to_the_bound).await;
    assert_eq!(
        repeat.status, 200,
        "case \"repeat-at-the-bound\": expected 200, got status {}",
        repeat.status
    );
}

/// Both writes refuse an id that resolves to no stored profile with the modelled 404, and
/// nothing in the store changes. The refusal's BODY carries no `message` member at all, so
/// it discloses nothing — not even the ARN the id would carry — about a profile that does
/// not exist.
///
/// The table spans why the id does not resolve (never minted, minted for a profile that was
/// deleted) and the Region the request is signed for, because an ARN-keyed wording would
/// follow the request: none of it may reach the body.
#[tokio::test]
async fn attribute_mapping_writes_refuse_an_unknown_profile() {
    let client = Client::new();

    let sibling = profile_id(&client, "sibling").await;
    let seeded = client
        .put_mapping(&sibling, "x509Subject", &owned(&["CN"]))
        .await;
    assert_eq!(
        seeded.status, 200,
        "case \"seed-the-sibling\": expected 200, got {seeded:?}"
    );
    let untouched = client.mappings("seed-the-sibling", &sibling).await;

    let gone = profile_id(&client, "deleted").await;
    let removed = client.delete_profile(&gone).await;
    assert_eq!(
        removed.status, 200,
        "case \"delete-the-fixture\": expected DeleteProfile 200, got {removed:?}"
    );

    let mut cases: Vec<(String, String, &str)> = Vec::new();
    for (label, id) in UNMINTED_IDS {
        cases.push((format!("unminted-{label}"), id.to_string(), REGION));
    }
    cases.push(("deleted-profile-id".to_string(), gone.clone(), REGION));
    for region in ["eu-central-1", "ap-southeast-2", "us-east-1"] {
        cases.push((
            format!("unminted-in-{region}"),
            UNMINTED_IDS[1].1.to_string(),
            region,
        ));
    }

    for (case, id, region) in cases {
        for (path, answer) in [
            (
                "PutAttributeMapping",
                client
                    .put_mapping_in_region(&id, "x509Subject", &owned(&["CN"]), region)
                    .await,
            ),
            (
                "DeleteAttributeMapping",
                client
                    .delete_mapping_in_region(&id, "x509Subject", Some(&owned(&["CN"])), region)
                    .await,
            ),
        ] {
            let labelled = format!("{path}/{case}");
            assert_eq!(
                answer.status, 404,
                "case {labelled:?}: expected status 404, got {answer:?}"
            );
            assert_eq!(
                answer.error_code, "ResourceNotFoundException",
                "case {labelled:?}: expected ResourceNotFoundException, got {:?}",
                answer.error_code
            );
            // Absence is the requirement: no `message` member on the wire, so the refusal
            // carries no ARN for a profile that does not exist.
            assert_eq!(
                answer.message(),
                None,
                "case {labelled:?}: the refusal must carry no message member, got {:?}",
                answer.message()
            );
            assert!(
                answer.body.get("message").is_none(),
                "case {labelled:?}: the refusal must carry no message member, got {:?}",
                answer.body
            );
            let rendered = answer.body.to_string();
            // Not even the ARN-keyed wording the profile READ paths answer a miss with:
            // the body carries no message, so nothing about the absent profile is
            // disclosed.
            let withheld = arn_not_found_message(&profile_arn(region, &id));
            assert!(
                !rendered.contains(&withheld)
                    && !rendered.contains("arn:")
                    && !rendered.contains(&id)
                    && !rendered.contains(region),
                "case {labelled:?}: the refusal must disclose no ARN, id or Region, got \
                 {rendered:?}"
            );
        }

        // The refusals wrote nothing: the sibling maps exactly what it mapped before them,
        // and the deleted profile is still gone.
        let still_there = client.mappings(&case, &sibling).await;
        assert_eq!(
            still_there, untouched,
            "case {case:?}: the sibling's mappings changed from {untouched:?} to {still_there:?}"
        );
        let missing = client.get_profile_in_region(&gone, region).await;
        assert_eq!(
            missing.status, 404,
            "case {case:?}: a refused write must not re-create the deleted profile, got \
             {missing:?}"
        );
    }
}

/// The X.509 field each `CertificateField` value names, which is how a refusal of a
/// specifier addresses the field: the certificate's subject, its issuer and its
/// subject-alternative-name extension (RFC 5280 sections 4.1.2.4, 4.1.2.6 and 4.2.1.6).
const FIELD_TARGETS: [(&str, &str); 3] = [
    ("x509Subject", "SUBJECT"),
    ("x509Issuer", "ISSUER"),
    ("x509SAN", "SUBJECT_ALTERNATIVE_NAME"),
];

/// The target name a refusal addresses `certificate_field` by.
fn target_of(certificate_field: &str) -> &'static str {
    FIELD_TARGETS
        .iter()
        .find(|(field, _)| *field == certificate_field)
        .map(|(_, target)| *target)
        .unwrap_or_else(|| panic!("{certificate_field:?} is not a modelled certificate field"))
}

/// The wording a specifier the named field cannot carry is refused with: the submitted
/// specifier, and the field's target name, each in parentheses of its own.
fn unvalidatable_specifier_message(specifier: &str, target: &str) -> String {
    format!("Unable to validate specifier ({specifier}) on the target ({target})")
}

/// One row of the specifier-compatibility table: the certificate field written, the
/// specifier submitted, and whether that field carries it.
type SpecifierCase = (&'static str, &'static str, bool);

/// Every specifier a mapping names has to be one the certificate field it is written on
/// carries: the subject and the issuer are distinguished names, so their parts are
/// addressed by attribute type (RFC 4514's short name or numeric OID form — "such as CN,
/// OU, or UID from the Subject field", `MappingRule$specifier` doc), while the
/// subject-alternative name holds general names, so its parts are addressed by name form
/// (`DNS`, `URI`, `Name`, and a `Name/<attribute type>` path into the directory name,
/// which is what this service maps for the field by default). A specifier the field does
/// not carry is refused 400 `ValidationException` naming the specifier and the field's
/// target, and is not mapped.
///
/// The rows differ along which field is written, which form the specifier takes (a short
/// attribute type, a numeric OID, a general-name form, a sub-field path, the whole-field
/// wildcard), its case, and how it is malformed.
fn specifier_cases() -> Vec<SpecifierCase> {
    vec![
        // A distinguished-name field carries attribute types and the whole-field wildcard.
        ("x509Subject", "CN", true),
        ("x509Subject", "OU", true),
        ("x509Subject", "UID", true),
        ("x509Subject", "STREET", true),
        ("x509Subject", "x500UniqueIdentifier", true),
        // `2.5.4.3` is commonName in numeric form, which RFC 4514 writes a type with no
        // short name as.
        ("x509Subject", "2.5.4.3", true),
        ("x509Subject", "*", true),
        ("x509Issuer", "O", true),
        ("x509Issuer", "C", true),
        ("x509Issuer", "2.5.4.10", true),
        ("x509Issuer", "*", true),
        // And nothing that is not an attribute type at all.
        ("x509Subject", "-CN", false),
        ("x509Subject", "CN,OU", false),
        ("x509Subject", "cn=example", false),
        ("x509Subject", "2..5", false),
        // A path addresses a part of a directory name, which the subject itself is not
        // reached through.
        ("x509Subject", "Name/CN", false),
        ("x509Issuer", "Name/CN", false),
        // The subject-alternative name carries its general-name forms, the directory
        // name's parts, and the whole-field wildcard.
        ("x509SAN", "DNS", true),
        ("x509SAN", "URI", true),
        ("x509SAN", "Name", true),
        ("x509SAN", "Name/CN", true),
        ("x509SAN", "Name/OU", true),
        ("x509SAN", "Name/*", true),
        ("x509SAN", "*", true),
        // And not a distinguished name's attribute types: the extension has no such
        // parts. The first of these is the recorded refusal.
        ("x509SAN", "CN", false),
        ("x509SAN", "OU", false),
        ("x509SAN", "UID", false),
        ("x509SAN", "2.5.4.3", false),
        // A name form is named as the extension names it, so another spelling is not one.
        ("x509SAN", "dns", false),
        ("x509SAN", "IP", false),
        // Only the directory name has parts, and a path into it addresses ONE of them.
        ("x509SAN", "DNS/CN", false),
        ("x509SAN", "URI/CN", false),
        ("x509SAN", "Name/CN/extra", false),
    ]
}

/// A put stores only specifiers the certificate field it names can carry, and refuses the
/// rest with the service's specifier wording while mapping nothing.
#[tokio::test]
async fn a_put_refuses_a_specifier_the_certificate_field_cannot_carry() {
    let client = Client::new();
    for (field, specifier, carried) in specifier_cases() {
        let case = format!("{field}/{specifier}");
        let case = case.as_str();
        let id = profile_id(&client, "specifier compatibility").await;
        let before = client.mappings(case, &id).await;

        let submitted = owned(&[specifier]);
        let answer = client.put_mapping(&id, field, &submitted).await;
        if !carried {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: a specifier the field cannot carry must be refused with \
                 HTTP 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code, "ValidationException",
                "case {case:?}: the refusal must be the operation's modelled error, got \
                 {answer:?}"
            );
            assert_eq!(
                answer.message(),
                Some(unvalidatable_specifier_message(specifier, target_of(field)).as_str()),
                "case {case:?}: expected the service's specifier wording, got {answer:?}"
            );
            let after = client.mappings(case, &id).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused put must map nothing, mappings went from \
                 {before:?} to {after:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case:?}: a specifier the field carries must be stored, got {answer:?}"
        );
        let after = written_mappings(case, &client, &id, &answer).await;
        let expected = specifiers_after_put(specifiers_in(&before, field).as_ref(), &submitted);
        assert_eq!(
            specifiers_in(&after, field).as_ref(),
            Some(&expected),
            "case {case:?}: expected {field:?} to map {expected:?}, got {after:?}"
        );
    }
}

/// One row of the multi-rule table: the certificate field written, the specifiers the ONE
/// request carries, and the specifier the field cannot carry that decides the refusal —
/// `None` when the field carries every specifier of the row.
type MultiRuleCase = (&'static str, &'static str, Vec<String>, Option<String>);

/// A request carries a LIST of mapping rules, and the specifier rule holds for each entry
/// of it: one entry the field cannot carry refuses the whole request, so the rules beside
/// it are not mapped either, and the first such entry in request order is the one the
/// refusal words.
///
/// The rows differ along how many rules the request carries, where the unusable entry sits
/// in it (alone, first, last, between usable ones), how many entries are unusable, whether
/// an unusable entry repeats, which field is written, and how long the specifier is (the
/// modelled `MappingRule$specifier` bound of 60).
fn multi_rule_cases() -> Vec<MultiRuleCase> {
    // An attribute type at the modelled length bound: a distinguished-name field carries
    // it, the subject-alternative name does not.
    let longest_attribute_type = "C".repeat(MAX_SPECIFIER_LEN);
    vec![
        // The recorded refusal: one rule, naming a distinguished name's attribute type on
        // the subject-alternative name.
        ("unusable-alone-on-san", "x509SAN", owned(&["CN"]), Some("CN".to_string())),
        (
            "usable-then-unusable-on-san",
            "x509SAN",
            owned(&["DNS", "CN"]),
            Some("CN".to_string()),
        ),
        (
            "unusable-then-usable-on-san",
            "x509SAN",
            owned(&["CN", "DNS"]),
            Some("CN".to_string()),
        ),
        // Several unusable entries: the FIRST in request order words the refusal.
        (
            "two-unusable-first-decides-on-san",
            "x509SAN",
            owned(&["OU", "CN"]),
            Some("OU".to_string()),
        ),
        (
            "unusable-between-usable-on-issuer",
            "x509Issuer",
            owned(&["O", "Name/CN", "C"]),
            Some("Name/CN".to_string()),
        ),
        (
            "repeated-unusable-on-subject",
            "x509Subject",
            owned(&["CN", "cn=example", "cn=example"]),
            Some("cn=example".to_string()),
        ),
        (
            "longest-unusable-on-san",
            "x509SAN",
            vec![longest_attribute_type.clone()],
            Some(longest_attribute_type.clone()),
        ),
        // The same shape of request with every specifier usable is mapped in full.
        (
            "all-usable-on-san",
            "x509SAN",
            owned(&["DNS", "URI", "Name/CN"]),
            None,
        ),
        (
            "all-usable-on-subject",
            "x509Subject",
            owned(&["CN", "2.5.4.3", "*"]),
            None,
        ),
        (
            "longest-usable-on-issuer",
            "x509Issuer",
            vec![longest_attribute_type],
            None,
        ),
    ]
}

/// One unusable rule refuses the whole request: nothing it carried is mapped, what the
/// field already mapped is untouched, and the profile is not touched at all — while a
/// request whose every rule is usable maps all of them.
#[tokio::test]
async fn one_unusable_rule_refuses_the_whole_request_and_maps_nothing() {
    let client = Client::new();
    for (case, field, submitted, unusable) in multi_rule_cases() {
        let id = profile_id(&client, case).await;
        // Seed the addressed field with the whole-field specifier, which every field
        // carries, so a refusal is shown to leave what the field already maps alone.
        let seeded = client.put_mapping(&id, field, &owned(&["*"])).await;
        assert_eq!(
            seeded.status, 200,
            "case {case:?}: expected the seeding put to be stored, got {seeded:?}"
        );
        let read_before = client.get_profile(&id).await;
        assert_eq!(
            read_before.status, 200,
            "case {case:?}: expected GetProfile 200, got {read_before:?}"
        );
        let before = mappings_of(read_before.profile());
        let updated_before = instant(case, read_before.profile(), "updatedAt");

        let answer = client.put_mapping(&id, field, &submitted).await;

        let Some(unusable) = unusable else {
            let after = written_mappings(case, &client, &id, &answer).await;
            let expected =
                specifiers_after_put(specifiers_in(&before, field).as_ref(), &submitted);
            assert_eq!(
                specifiers_in(&after, field).as_ref(),
                Some(&expected),
                "case {case:?}: expected {field:?} to map {expected:?}, got {after:?}"
            );
            continue;
        };

        assert_eq!(
            answer.status, 400,
            "case {case:?}: one rule the field cannot carry must refuse the request with \
             HTTP 400, got {answer:?}"
        );
        assert_eq!(
            answer.error_code, "ValidationException",
            "case {case:?}: the refusal must be the operation's modelled error, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            Some(unvalidatable_specifier_message(&unusable, target_of(field)).as_str()),
            "case {case:?}: expected the refusal to word {unusable:?} on {field:?}, got \
             {answer:?}"
        );

        let read_after = client.get_profile(&id).await;
        assert_eq!(
            read_after.status, 200,
            "case {case:?}: expected GetProfile 200, got {read_after:?}"
        );
        let after = mappings_of(read_after.profile());
        assert_eq!(
            after, before,
            "case {case:?}: a refused put must map nothing, mappings went from {before:?} to \
             {after:?}"
        );
        // Not even the usable rules the SAME request carried beside the refused one: a
        // specifier the field did not map before the call is not mapped after it.
        let mapped_before = specifiers_in(&before, field).unwrap_or_default();
        let mapped = specifiers_in(&after, field).unwrap_or_default();
        for specifier in submitted.iter().filter(|s| !mapped_before.contains(s)) {
            assert!(
                !mapped.contains(specifier),
                "case {case:?}: a refused put must not map {specifier:?}, got {mapped:?}"
            );
        }
        // The profile was not written at all, so the instant it was last updated stands.
        let updated_after = instant(case, read_after.profile(), "updatedAt");
        assert_eq!(
            updated_after, updated_before,
            "case {case:?}: a refused put must not update the profile, updatedAt went from \
             {updated_before} to {updated_after}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// How this service QUOTES the requirement a refused value failed.
//
// The rule: a value a modelled constraint refuses is answered 400 `ValidationException`
// whose clause quotes the requirement as this service PUBLISHES it — a `@pattern` as the
// expression without the whole-value anchors the model wraps it in, and an enumerated type
// as the values it admits in the order the front door's own table of them is walked
// (ascending bucket of each value's string hash) rather than the order the model declares
// them in. Both are properties of the SHAPE a member targets, so they hold for every
// member targeting it, in any operation, in whatever binding carried the value and at any
// depth of the request; the envelope, the member path and the echoed value are the
// framework's.
// ---------------------------------------------------------------------------------------

/// The expression `ResourceName` publishes: the model declares
/// `@pattern("^[ a-zA-Z0-9-_]*$")` (`smithy_query(query="shape", shape="ResourceName")`)
/// and the pair of whole-value anchors is the model's wrapping, not part of the expression.
const PUBLISHED_RESOURCE_NAME_PATTERN: &str = "[ a-zA-Z0-9-_]*";

/// The expression `TrustAnchorArn` publishes: the model declares
/// `@pattern("^arn:aws(-[^:]+)?:rolesanywhere(:.*){2}(:trust-anchor.*)$")`
/// (`smithy_query(query="operation", operation="ImportCrl")`), the same wrapping dropped.
const PUBLISHED_TRUST_ANCHOR_ARN_PATTERN: &str =
    r"arn:aws(-[^:]+)?:rolesanywhere(:.*){2}(:trust-anchor.*)";

/// The expression `Uuid` publishes, which the model declares UNANCHORED
/// (`@pattern("[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}")`,
/// `smithy_query(query="operation", operation="PutAttributeMapping")`): nothing to drop, so
/// it is quoted exactly as declared.
const PUBLISHED_UUID_PATTERN: &str = "[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}";

/// A value outside the modelled `Uuid` form that no profile or trust anchor is created
/// under: it breaks the pattern and nothing else, so a refusal of it states one clause.
const UPPERCASE_UUID: &str = "0123ABCD-4567-89EF-0123-456789ABCDEF";

/// A `Uuid` nothing in the store is created under, used where the request has to carry one
/// and the refusal is about ANOTHER member: the boundary refuses before the handler would
/// resolve it.
const UNMINTED_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// The admitted values of `CertificateField` (declared `x509Subject, x509Issuer, x509SAN`,
/// `smithy_query(query="shape", shape="CertificateField")`) in the order this service
/// states them: ascending bucket of each value's string hash, which is neither the declared
/// nor the sorted order.
const STATED_CERTIFICATE_FIELD_SET: &str = "[x509Subject, x509SAN, x509Issuer]";

/// The admitted values of `TrustAnchorType` (declared `AWS_ACM_PCA, CERTIFICATE_BUNDLE,
/// SELF_SIGNED_REPOSITORY`, `smithy_query(query="shape", shape="TrustAnchorType")`) in that
/// same stated order.
const STATED_TRUST_ANCHOR_TYPE_SET: &str =
    "[CERTIFICATE_BUNDLE, AWS_ACM_PCA, SELF_SIGNED_REPOSITORY]";

/// The admitted values of `NotificationEvent` (declared `CA_CERTIFICATE_EXPIRY,
/// END_ENTITY_CERTIFICATE_EXPIRY`, `smithy_query(query="shape",
/// shape="NotificationEvent")`) in that same stated order, which here REVERSES the
/// declaration.
const STATED_NOTIFICATION_EVENT_SET: &str =
    "[END_ENTITY_CERTIFICATE_EXPIRY, CA_CERTIFICATE_EXPIRY]";

/// The admitted values of `NotificationChannel`, which the model declares as the single
/// value `ALL` (`smithy_query(query="shape", shape="NotificationChannel")`).
const STATED_NOTIFICATION_CHANNEL_SET: &str = "[ALL]";

/// AWS's one-error validation envelope over one clause: the value as the request carried
/// it, the member path, and the requirement that value failed.
fn one_error_clause(value: &str, member_path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member_path}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// The requirement clause a `@pattern` violation states.
fn pattern_requirement(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The requirement clause a violation of an enumerated type states.
fn enum_requirement(value_set: &str) -> String {
    format!("Member must satisfy enum value set: {value_set}")
}

/// A certificate bundle source whose certificate data is not read at all on these paths:
/// every row below is refused by the boundary walk, before any handler reads the source.
fn unread_source(source_type: &str) -> Value {
    json!({"sourceType": source_type, "sourceData": {"x509CertificateData": "not-a-pem"}})
}

/// One refused request: where it goes, what it carries, and the three things its clause
/// must state.
struct RefusalCase {
    label: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    value: String,
    member_path: &'static str,
    requirement: String,
}

/// Every request whose refusal quotes a `@pattern` requirement.
///
/// The rows differ along what the rule generalises over: which shape's expression is quoted
/// (two the model anchors, one it leaves unanchored), which operation and member carried the
/// value (a create, an update, a path label, a body member, an ARN member), and which end of
/// the anchored expression the value broke.
fn pattern_cases() -> Vec<RefusalCase> {
    let crl_data = aws_smithy_types::base64::encode("x".as_bytes());
    let trust_anchor_arn =
        format!("arn:aws:rolesanywhere:{REGION}:{ACCOUNT}:trust-anchor/{UNMINTED_UUID}");
    vec![
        RefusalCase {
            label: "the name of a created profile",
            method: "POST",
            uri: "/profiles".to_string(),
            body: Some(json!({
                "name": "no!punctuation",
                "roleArns": [format!("arn:aws:iam::{ACCOUNT}:role/DemoRole")],
            })),
            value: "no!punctuation".to_string(),
            member_path: "name",
            requirement: pattern_requirement(PUBLISHED_RESOURCE_NAME_PATTERN),
        },
        RefusalCase {
            label: "the name of an updated profile",
            method: "PATCH",
            uri: format!("/profile/{UNMINTED_UUID}"),
            body: Some(json!({"name": "tab\tseparated"})),
            value: "tab\tseparated".to_string(),
            member_path: "name",
            requirement: pattern_requirement(PUBLISHED_RESOURCE_NAME_PATTERN),
        },
        RefusalCase {
            label: "the name of a created trust anchor",
            method: "POST",
            uri: "/trustanchors".to_string(),
            body: Some(json!({
                "name": "trust/anchor",
                "source": unread_source("CERTIFICATE_BUNDLE"),
            })),
            value: "trust/anchor".to_string(),
            member_path: "name",
            requirement: pattern_requirement(PUBLISHED_RESOURCE_NAME_PATTERN),
        },
        RefusalCase {
            label: "the name of an imported certificate revocation list",
            method: "POST",
            uri: "/crls".to_string(),
            body: Some(json!({
                "name": "crl#1",
                "crlData": crl_data.clone(),
                "trustAnchorArn": trust_anchor_arn,
            })),
            value: "crl#1".to_string(),
            member_path: "name",
            requirement: pattern_requirement(PUBLISHED_RESOURCE_NAME_PATTERN),
        },
        RefusalCase {
            label: "an ARN member whose expression opens with a literal",
            method: "POST",
            uri: "/crls".to_string(),
            body: Some(json!({
                "name": "importable crl",
                "crlData": crl_data,
                "trustAnchorArn": "not-a-trust-anchor-arn",
            })),
            value: "not-a-trust-anchor-arn".to_string(),
            member_path: "trustAnchorArn",
            requirement: pattern_requirement(PUBLISHED_TRUST_ANCHOR_ARN_PATTERN),
        },
        RefusalCase {
            label: "a path label whose expression the model leaves unanchored",
            method: "PUT",
            uri: format!("/profiles/{UPPERCASE_UUID}/mappings"),
            body: Some(json!({
                "certificateField": "x509Subject",
                "mappingRules": [{"specifier": "CN"}],
            })),
            value: UPPERCASE_UUID.to_string(),
            member_path: "profileId",
            requirement: pattern_requirement(PUBLISHED_UUID_PATTERN),
        },
    ]
}

/// Every request whose refusal quotes an enumerated type's admitted set.
///
/// The rows differ along what the rule generalises over: which enumerated type the value
/// belongs to (four of this model's, two of them stated in neither their declared nor their
/// sorted order), which operation and member carried it (a body member, a member of a
/// nested structure, a member of a structure inside a list), how many values the type
/// admits (one, two, three), and how the refused value was spelled (a name no value
/// carries, an admitted value miscased, an admitted value of ANOTHER type).
fn enum_cases(profile_id: &str) -> Vec<RefusalCase> {
    vec![
        RefusalCase {
            label: "the certificate field of a put mapping",
            method: "PUT",
            uri: format!("/profiles/{profile_id}/mappings"),
            body: Some(json!({
                "certificateField": "x509subject",
                "mappingRules": [{"specifier": "CN"}],
            })),
            value: "x509subject".to_string(),
            member_path: "certificateField",
            requirement: enum_requirement(STATED_CERTIFICATE_FIELD_SET),
        },
        RefusalCase {
            label: "the source type NESTED in a created trust anchor's source",
            method: "POST",
            uri: "/trustanchors".to_string(),
            body: Some(json!({
                "name": "enum anchor",
                "source": unread_source("CERTIFICATE_CHAIN"),
            })),
            value: "CERTIFICATE_CHAIN".to_string(),
            member_path: "source.sourceType",
            requirement: enum_requirement(STATED_TRUST_ANCHOR_TYPE_SET),
        },
        RefusalCase {
            label: "the source type of an updated trust anchor",
            method: "PATCH",
            uri: format!("/trustanchor/{UNMINTED_UUID}"),
            body: Some(json!({"source": unread_source("certificate_bundle")})),
            value: "certificate_bundle".to_string(),
            member_path: "source.sourceType",
            requirement: enum_requirement(STATED_TRUST_ANCHOR_TYPE_SET),
        },
        RefusalCase {
            label: "the event of a notification setting carried in a LIST",
            method: "PATCH",
            uri: "/put-notifications-settings".to_string(),
            body: Some(json!({
                "trustAnchorId": UNMINTED_UUID,
                "notificationSettings": [
                    {"enabled": true, "event": "CERTIFICATE_EXPIRY", "threshold": 7},
                ],
            })),
            value: "CERTIFICATE_EXPIRY".to_string(),
            member_path: "notificationSettings.1.member.event",
            requirement: enum_requirement(STATED_NOTIFICATION_EVENT_SET),
        },
        RefusalCase {
            label: "the single-valued channel of a notification setting",
            method: "PATCH",
            uri: "/put-notifications-settings".to_string(),
            body: Some(json!({
                "trustAnchorId": UNMINTED_UUID,
                "notificationSettings": [{
                    "enabled": true,
                    "event": "CA_CERTIFICATE_EXPIRY",
                    "channel": "EMAIL",
                }],
            })),
            value: "EMAIL".to_string(),
            member_path: "notificationSettings.1.member.channel",
            requirement: enum_requirement(STATED_NOTIFICATION_CHANNEL_SET),
        },
    ]
}

/// Drive one refused request and hold its answer to the clause the rule states.
async fn assert_refused(client: &Client, case: &RefusalCase) {
    let answer = client
        .send(case.method, &case.uri, case.body.clone())
        .await;
    assert_eq!(
        (answer.status, answer.error_code.as_str()),
        (400, "ValidationException"),
        "case {:?}: a value outside a modelled constraint must be refused 400 \
         ValidationException, got {answer:?}",
        case.label
    );
    let expected = one_error_clause(&case.value, case.member_path, &case.requirement);
    assert_eq!(
        answer.message(),
        Some(expected.as_str()),
        "case {:?}: expected {expected:?}, got {:?}",
        case.label,
        answer.message()
    );
}

/// A `@pattern` requirement is quoted as the expression the service publishes: the model's
/// text with the whole-value anchor pair dropped, and an expression the model leaves
/// unanchored quoted exactly as declared.
#[tokio::test]
async fn a_pattern_requirement_quotes_the_expression_without_the_whole_value_anchors() {
    let client = Client::new();
    for case in pattern_cases() {
        assert_refused(&client, &case).await;
    }
}

/// Every refusal of a value outside an enumerated type lists that type's admitted values in
/// the order this service states them, whatever member, operation, binding or depth carried
/// the value — and lists exactly the values the model declares, nothing added or dropped.
#[tokio::test]
async fn an_enum_requirement_lists_the_admitted_values_in_the_order_the_service_states_them() {
    let client = Client::new();
    let id = profile_id(&client, "enum value sets").await;
    for case in enum_cases(&id) {
        assert_refused(&client, &case).await;
    }

    // And the stated sets are permutations of what the model declares: the order moves, the
    // membership does not.
    let declared: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "CertificateField",
            STATED_CERTIFICATE_FIELD_SET,
            CERTIFICATE_FIELDS.to_vec(),
        ),
        (
            "TrustAnchorType",
            STATED_TRUST_ANCHOR_TYPE_SET,
            vec!["AWS_ACM_PCA", "CERTIFICATE_BUNDLE", "SELF_SIGNED_REPOSITORY"],
        ),
        (
            "NotificationEvent",
            STATED_NOTIFICATION_EVENT_SET,
            vec!["CA_CERTIFICATE_EXPIRY", "END_ENTITY_CERTIFICATE_EXPIRY"],
        ),
        (
            "NotificationChannel",
            STATED_NOTIFICATION_CHANNEL_SET,
            vec!["ALL"],
        ),
    ];
    for (shape, stated, modelled) in declared {
        let mut listed: Vec<&str> = stated
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(", ")
            .collect();
        listed.sort_unstable();
        let mut sorted = modelled.clone();
        sorted.sort_unstable();
        assert_eq!(
            listed, sorted,
            "case {shape:?}: the stated set must list exactly the values the model declares \
             {sorted:?}, got {listed:?}"
        );
    }
}

/// The expression `TagKey` and `TagValue` publish: both are modelled
/// `@pattern("^[ a-zA-Z0-9_.:/=+@-]*$")` and `@sensitive`
/// (`smithy_query(query="shape", shape="Tag")`), so the pair of whole-value anchors is
/// dropped from the quoted expression while the VALUE the member carried is not disclosed.
const PUBLISHED_TAG_PATTERN: &str = "[ a-zA-Z0-9_.:/=+@-]*";

/// A refused `@sensitive` member states the published expression and withholds the value:
/// the requirement is quoted the same way as for every other patterned shape, and nothing
/// the caller sent in a member the model marks sensitive comes back in the message.
#[tokio::test]
async fn a_refused_sensitive_member_states_the_published_pattern_without_its_value() {
    let client = Client::new();
    let secretish = "tag key!";
    for (case, tag) in [
        ("the tag key", json!({"key": secretish, "value": "allowed"})),
        ("the tag value", json!({"key": "allowed", "value": secretish})),
    ] {
        let answer = client
            .send(
                "POST",
                "/TagResource",
                Some(json!({
                    "resourceArn": profile_arn(REGION, UNMINTED_UUID),
                    "tags": [tag],
                })),
            )
            .await;
        assert_eq!(
            (answer.status, answer.error_code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a value outside the published tag expression must be refused 400 \
             ValidationException, got {answer:?}"
        );
        let said = answer.message().unwrap_or_default();
        assert!(
            said.contains(&pattern_requirement(PUBLISHED_TAG_PATTERN)),
            "case {case:?}: the clause must quote the published expression {:?}, got {said:?}",
            PUBLISHED_TAG_PATTERN
        );
        assert!(
            !said.contains(secretish),
            "case {case:?}: a sensitive member's value must not be disclosed, got {said:?}"
        );
    }
}

/// A put is judged before anything is written, so one unusable specifier beside usable
/// ones maps NONE of them — whichever position it is carried in.
#[tokio::test]
async fn a_put_carrying_one_unusable_specifier_maps_none_of_them() {
    let client = Client::new();
    for (case, submitted, unusable) in [
        ("unusable-first", owned(&["CN", "DNS", "URI"]), "CN"),
        ("unusable-last", owned(&["DNS", "URI", "CN"]), "CN"),
        ("unusable-in-the-middle", owned(&["DNS", "OU", "URI"]), "OU"),
    ] {
        let id = profile_id(&client, "partial specifier set").await;
        let before = client.mappings(case, &id).await;

        let answer = client.put_mapping(&id, "x509SAN", &submitted).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            Some(
                unvalidatable_specifier_message(unusable, target_of("x509SAN"))
                    .as_str()
            ),
            "case {case:?}: the refusal must name the unusable specifier, got {answer:?}"
        );
        let after = client.mappings(case, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused put must map none of the specifiers it carried, \
             mappings went from {before:?} to {after:?}"
        );
    }
}
