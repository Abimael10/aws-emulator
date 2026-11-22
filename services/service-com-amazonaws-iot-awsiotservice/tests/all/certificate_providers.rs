// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of CERTIFICATE PROVIDERS: `CreateCertificateProvider`, the
//! `DescribeCertificateProvider` reader that proves what it persisted,
//! `ListCertificateProviders`, `UpdateCertificateProvider`,
//! `DeleteCertificateProvider`, and the rule that an operation has ONE account default.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a provider is created under its name, with the function it calls and the
//!   operations it is the account's default for, and a later read reports that record
//!   member for member;
//! * an operation has ONE account default: a provider claiming one another provider
//!   holds is refused `InvalidRequestException` (400) with nothing created, and the
//!   claim is free again as soon as the provider holding it is deleted;
//! * a name the account holds no provider under is `ResourceNotFoundException` (404);
//! * an update rewrites exactly the members the request names, leaves every other member
//!   of the stored record as it was, and is refused `ResourceNotFoundException` (404) for
//!   a name the account holds no provider under;
//! * a name the account already holds a provider under is
//!   `ResourceAlreadyExistsException` (409), worded with this family's own already-exists
//!   sentence, and the stored provider is left as it was;
//! * a listing reports every provider the account holds — name and ARN, the ones the
//!   create minted — in the direction `isAscendingOrder` names, answers an account that
//!   holds none with an empty list, changes nothing a later caller sees, and refuses a
//!   `nextToken` it did not issue with `InvalidRequestException` (400).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::CERTIFICATE_PROVIDER_OPERATIONS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "us-west-1";
/// The function a provider calls: its form is the caller's, so one well-formed ARN is
/// used throughout.
const PROVIDER_FUNCTION: &str = "arn:aws:lambda:us-west-1:000000000000:function:issuer";
/// A second well-formed function ARN: a create refused for a taken name must not replace
/// the stored provider's function with this one.
const REPLACEMENT_FUNCTION: &str = "arn:aws:lambda:us-west-1:000000000000:function:other-issuer";

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the certificate-provider request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_provider(&self, name: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            &format!("/certificate-providers/{name}"),
            Some(body),
        )
        .await
    }

    async fn describe_provider(&self, name: &str) -> Reply {
        self.dispatch("GET", &format!("/certificate-providers/{name}"), None)
            .await
    }

    async fn delete_provider(&self, name: &str) -> Reply {
        self.dispatch("DELETE", &format!("/certificate-providers/{name}"), None)
            .await
    }

    /// `UpdateCertificateProvider`: the model binds it to `PUT
    /// /certificate-providers/{certificateProviderName}`
    /// (`smithy_query(query="operation",operation="UpdateCertificateProvider")`).
    async fn update_provider(&self, name: &str, body: &Value) -> Reply {
        self.dispatch("PUT", &format!("/certificate-providers/{name}"), Some(body))
            .await
    }

    /// `ListCertificateProviders` with whatever query string the case asks for: the
    /// operation binds `ascendingOrder` to `isAscendingOrder` and `nextToken` to
    /// `nextToken`
    /// (`smithy_query(query="operation",operation="ListCertificateProviders")`).
    async fn list_providers(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/certificate-providers".to_string()
        } else {
            format!("/certificate-providers?{query}")
        };
        self.dispatch("GET", &path, None).await
    }

    /// The request body creating a provider that claims `operations`.
    fn claim(operations: &[&str]) -> Value {
        json!({
            "lambdaFunctionArn": PROVIDER_FUNCTION,
            "accountDefaultForOperations": operations,
        })
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// RULE: a created provider is read back member for member, under whichever name and
/// operation it claims.
#[tokio::test]
async fn a_created_certificate_provider_is_read_back_member_for_member() {
    // One row per operation a provider may be the account default for, which is the
    // catalog the model declares
    // (`smithy_query(query="shape",shape="CertificateProviderOperation")`, reached
    // through `CERTIFICATE_PROVIDER_OPERATIONS`): each is created in its own account,
    // because the account holds one default per operation.
    for operation in CERTIFICATE_PROVIDER_OPERATIONS {
        let emulator = Emulator::new();
        // The names span the form the model admits (`^[\w=,@-]+$`): a plain name, one
        // using every character class the pattern allows, and the shortest legal one.
        for name in ["issuer", "issuer_2-A=,@", "a"] {
            let created = emulator
                .create_provider(name, &Emulator::claim(&[operation]))
                .await;
            assert_eq!(
                created.status,
                200,
                "case {name} claiming {operation}: expected 200, got {} body {}",
                created.status,
                created.text()
            );
            let expected_arn =
                format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:certificateprovider/{name}");
            assert_eq!(
                created.json()["certificateProviderArn"],
                json!(expected_arn),
                "case {name}: expected the minted ARN, got {}",
                created.text()
            );

            let read = emulator.describe_provider(name).await;
            assert_eq!(
                read.status,
                200,
                "case {name}: expected the read to answer 200, got {} body {}",
                read.status,
                read.text()
            );
            let described = read.json();
            assert_eq!(
                described["certificateProviderName"],
                json!(name),
                "case {name}: the read must name the provider, got {described}"
            );
            assert_eq!(
                described["certificateProviderArn"], created.json()["certificateProviderArn"],
                "case {name}: the read must report the ARN the create minted, got {described}"
            );
            assert_eq!(
                described["lambdaFunctionArn"],
                json!(PROVIDER_FUNCTION),
                "case {name}: the read must report the stored function, got {described}"
            );
            assert_eq!(
                described["accountDefaultForOperations"],
                json!([operation]),
                "case {name}: the read must report the claim, got {described}"
            );
            assert!(
                described["creationDate"].is_number()
                    && described["lastModifiedDate"].is_number(),
                "case {name}: the read must report when the provider was created, got {described}"
            );

            // The claim is freed by the delete, which is what lets the next row take it.
            assert_eq!(
                emulator.delete_provider(name).await.status,
                200,
                "case {name}: expected the delete to answer 200"
            );
        }
    }
}

/// RULE: a create under a name the account already holds a certificate provider under is
/// refused with this family's already-exists sentence — the name BRACKETED and the
/// collision scoped to the account — and the provider that holds the name is left exactly
/// as it was.
#[tokio::test]
async fn a_taken_certificate_provider_name_is_refused_with_this_familys_sentence() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    // The rows span the form `CertificateProviderName` admits
    // (`smithy_query(query="operation",operation="CreateCertificateProvider")`: len 1..128,
    // pattern `^[\w=,@-]+$`): a plain name, one using every character class the pattern
    // allows, the shortest legal one, and the longest.
    let longest = "p".repeat(128);
    for name in ["issuer", "issuer_2-A=,@", "a", longest.as_str()] {
        // Each row gets its own account: a provider claims an operation's account default,
        // and an account holds one provider per operation.
        let emulator = Emulator::new();
        let created = emulator
            .create_provider(name, &Emulator::claim(&[operation]))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {name}: expected the first create to answer 200, got {} body {}",
            created.status,
            created.text()
        );

        // The second create asks for a DIFFERENT function, so a create that overwrote the
        // record instead of refusing would show up in the read below.
        let duplicate = emulator
            .create_provider(
                name,
                &json!({
                    "lambdaFunctionArn": REPLACEMENT_FUNCTION,
                    "accountDefaultForOperations": [operation],
                }),
            )
            .await;
        assert_eq!(
            duplicate.status,
            409,
            "case {name}: expected 409 for a name already taken, got {} body {}",
            duplicate.status,
            duplicate.text()
        );
        assert_eq!(
            duplicate.error_code(),
            "ResourceAlreadyExistsException",
            "case {name}: expected ResourceAlreadyExistsException, got {}",
            duplicate.text()
        );
        assert_eq!(
            duplicate.error_message(),
            format!("Certificate provider [{name}] already exists for this account"),
            "case {name}: expected this family's already-exists sentence, got {}",
            duplicate.text()
        );

        let read = emulator.describe_provider(name).await;
        let described = read.json();
        assert_eq!(
            described["lambdaFunctionArn"],
            json!(PROVIDER_FUNCTION),
            "case {name}: the refused create must leave the stored provider as it was, got \
             {described}"
        );
    }
}

/// RULE: an operation has ONE account default — a second claim is refused and leaves
/// no provider behind, and the claim is reclaimable once the holder is deleted.
#[tokio::test]
async fn one_provider_at_a_time_holds_an_operations_account_default() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    let emulator = Emulator::new();

    let first = emulator
        .create_provider("holder", &Emulator::claim(&[operation]))
        .await;
    assert_eq!(
        first.status,
        200,
        "case the first claim: expected 200, got {} body {}",
        first.status,
        first.text()
    );

    // A second provider claiming the same operation is refused, and nothing is created
    // for it: the read of the name it asked for is a 404.
    let second = emulator
        .create_provider("contender", &Emulator::claim(&[operation]))
        .await;
    assert_eq!(
        second.status,
        400,
        "case a second claim: expected 400, got {} body {}",
        second.status,
        second.text()
    );
    assert_eq!(
        second.error_code(),
        "InvalidRequestException",
        "case a second claim: got {}",
        second.text()
    );
    assert_eq!(
        second.error_message(),
        format!(
            "Cannot set provider [contender] as default for operation {operation} because \
             another provider was set to be default for the operation"
        ),
        "case a second claim: got {}",
        second.text()
    );
    let missing = emulator.describe_provider("contender").await;
    assert_eq!(
        missing.status,
        404,
        "case a second claim: the refused create must leave no provider behind, got {} body {}",
        missing.status,
        missing.text()
    );
    assert_eq!(
        missing.error_code(),
        "ResourceNotFoundException",
        "case a second claim: got {}",
        missing.text()
    );
    assert_eq!(
        missing.error_message(),
        "Certificate provider with name [contender] not found",
        "case a second claim: got {}",
        missing.text()
    );
    // The provider that holds the claim is untouched.
    assert_eq!(
        emulator.describe_provider("holder").await.status,
        200,
        "case a second claim: the holder must be left as it was"
    );

    // The name is the provider's identity, so a repeat create under the holder's own
    // name is the conflict that names it, not the default-exclusivity refusal.
    let duplicate = emulator
        .create_provider("holder", &Emulator::claim(&[operation]))
        .await;
    assert_eq!(
        duplicate.status,
        409,
        "case a name already taken: expected 409, got {} body {}",
        duplicate.status,
        duplicate.text()
    );
    assert_eq!(
        duplicate.error_code(),
        "ResourceAlreadyExistsException",
        "case a name already taken: got {}",
        duplicate.text()
    );

    // Deleting the holder frees the claim: the create that was refused now succeeds.
    assert_eq!(
        emulator.delete_provider("holder").await.status,
        200,
        "case the holder deleted: expected 200"
    );
    let reclaimed = emulator
        .create_provider("contender", &Emulator::claim(&[operation]))
        .await;
    assert_eq!(
        reclaimed.status,
        200,
        "case the claim reclaimed: expected 200, got {} body {}",
        reclaimed.status,
        reclaimed.text()
    );
    assert_eq!(
        emulator.describe_provider("contender").await.status,
        200,
        "case the claim reclaimed: the new provider must be readable"
    );

    // A delete of a provider the account does not hold is the same 404 the read is.
    let deleted_twice = emulator.delete_provider("holder").await;
    assert_eq!(
        deleted_twice.status,
        404,
        "case deleting a provider the account does not hold: expected 404, got {} body {}",
        deleted_twice.status,
        deleted_twice.text()
    );
    assert_eq!(
        deleted_twice.error_message(),
        "Certificate provider with name [holder] not found",
        "case deleting a provider the account does not hold: got {}",
        deleted_twice.text()
    );
}

/// The ways a caller can ask for the listing's direction: the member is bound to the
/// `isAscendingOrder` query parameter and defaults to false when the request names none
/// (`smithy_query(query="operation",operation="ListCertificateProviders")`).
///
/// The direction cannot change the ORDER observed here, and that is a property of the
/// resource rather than of the test: a provider is `@required` to claim exactly one
/// operation's account default (`accountDefaultForOperations`, modelled `length 1..1`
/// over the single-valued `CertificateProviderOperation` enum) and an operation has one
/// default, so an account holds at most one certificate provider at a time. What the
/// rows below assert is that the answer is the same whichever way the direction is
/// asked for; the order the shared name ordering produces for many names is asserted
/// where that ordering lives (`state::name_listing`).
const LISTING_DIRECTIONS: [&str; 3] = ["", "isAscendingOrder=true", "isAscendingOrder=false"];

/// RULE: the listing reports every certificate provider the account holds — the name and
/// the ARN the create minted — and nothing else; an account holding none answers with an
/// empty list, and the answer does not depend on how the direction was asked for.
#[tokio::test]
async fn the_listing_reports_the_certificate_providers_the_account_holds() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    // The names span the form `CertificateProviderName` admits (len 1..128, pattern
    // `^[\w=,@-]+$`): a plain name, one using every character class the pattern allows,
    // the shortest legal one, and the longest.
    let longest = "p".repeat(128);
    for name in ["issuer", "issuer_2-A=,@", "a", longest.as_str()] {
        for query in LISTING_DIRECTIONS {
            // Each row gets its own account: a provider claims an operation's account
            // default, and an account holds one provider per operation.
            let emulator = Emulator::new();

            // An account that holds no provider: the member is present and empty, and the
            // listing is not truncated.
            let empty = emulator.list_providers(query).await;
            assert_eq!(
                empty.status,
                200,
                "case {name:?} {query:?} before the create: expected 200, got {} body {}",
                empty.status,
                empty.text()
            );
            assert_eq!(
                empty.json()["certificateProviders"],
                json!([]),
                "case {name:?} {query:?} before the create: expected an empty list, got {}",
                empty.text()
            );
            assert_eq!(
                empty.json()["nextToken"],
                Value::Null,
                "case {name:?} {query:?} before the create: a listing that holds every \
                 provider there is must not be truncated, got {}",
                empty.text()
            );

            let created = emulator
                .create_provider(name, &Emulator::claim(&[operation]))
                .await;
            assert_eq!(
                created.status,
                200,
                "case {name:?} {query:?}: expected the create to answer 200, got {} body {}",
                created.status,
                created.text()
            );

            // The entry is the stored record: the name the create took and the ARN it
            // minted, which is also the ARN the full read reports.
            let listed = emulator.list_providers(query).await;
            assert_eq!(
                listed.status,
                200,
                "case {name:?} {query:?}: expected 200, got {} body {}",
                listed.status,
                listed.text()
            );
            assert_eq!(
                listed.json()["certificateProviders"],
                json!([{
                    "certificateProviderName": name,
                    "certificateProviderArn": created.json()["certificateProviderArn"],
                }]),
                "case {name:?} {query:?}: expected the created provider's name and ARN, got {}",
                listed.text()
            );
            assert_eq!(
                listed.json()["nextToken"],
                Value::Null,
                "case {name:?} {query:?}: a listing that holds every provider there is must \
                 not be truncated, got {}",
                listed.text()
            );
            assert_eq!(
                emulator.describe_provider(name).await.json()["certificateProviderArn"],
                listed.json()["certificateProviders"][0]["certificateProviderArn"],
                "case {name:?} {query:?}: the listing and the full read must report one ARN, \
                 got {}",
                listed.text()
            );

            // A listing is a READ: asking again answers the same thing, and the provider it
            // reported is still there to be read.
            let again = emulator.list_providers(query).await;
            assert_eq!(
                again.json(),
                listed.json(),
                "case {name:?} {query:?}: a second listing must report what the first did, \
                 got {}",
                again.text()
            );
            assert_eq!(
                emulator.describe_provider(name).await.status,
                200,
                "case {name:?} {query:?}: listing a provider must leave it readable"
            );

            // The provider leaves the listing with the record: a deleted provider is no
            // longer one the account holds.
            assert_eq!(
                emulator.delete_provider(name).await.status,
                200,
                "case {name:?} {query:?}: expected the delete to answer 200"
            );
            let after_delete = emulator.list_providers(query).await;
            assert_eq!(
                after_delete.json()["certificateProviders"],
                json!([]),
                "case {name:?} {query:?} after the delete: expected an empty list, got {}",
                after_delete.text()
            );
        }
    }
}

/// RULE: a `nextToken` the listing itself did not issue is refused
/// `InvalidRequestException` (400), whatever the account holds.
#[tokio::test]
async fn a_next_token_the_listing_did_not_issue_is_refused() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    let emulator = Emulator::new();
    assert_eq!(
        emulator
            .create_provider("issuer", &Emulator::claim(&[operation]))
            .await
            .status,
        200,
        "the provider the refused listings are asked about must exist"
    );

    // Every row satisfies the modelled `Marker` pattern `^[A-Za-z0-9+/]+={0,2}$` — a token
    // that does not is refused before the handler runs — and none of them is a token this
    // listing handed out: the token is opaque, so the only token a caller can hold is one
    // a previous page of the SAME listing gave it. The rows that are well-formed tokens of
    // some other listing are built with the same encoder the handler decodes with.
    let cases = [
        ("a token of no listing at all", "ZZZZ".to_string()),
        ("a token that names nothing", hex::encode("nope")),
        (
            "a token of another listing of this service",
            hex::encode("authorizers/a/\u{1f}issuer"),
        ),
        (
            "a token of the other direction of this listing",
            hex::encode("certificateProviders/d\u{1f}issuer"),
        ),
    ];
    for (case, token) in cases {
        // The ascending direction is the one asked for, so the descending token above is a
        // token of a listing other than this one.
        let refused = emulator
            .list_providers(&format!("isAscendingOrder=true&nextToken={token}"))
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {case}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case}: expected InvalidRequestException, got {}",
            refused.text()
        );
    }

    // The refusals changed nothing: the provider is still listed.
    let listed = emulator.list_providers("isAscendingOrder=true").await;
    assert_eq!(
        listed.json()["certificateProviders"][0]["certificateProviderName"],
        json!("issuer"),
        "case after the refused tokens: the account must still hold its provider, got {}",
        listed.text()
    );
}

/// RULE: an update rewrites exactly the members the request names and leaves every other
/// member of the stored record as it was — the identity it was created under included —
/// and reports the provider it wrote.
#[tokio::test]
async fn an_update_rewrites_only_the_members_the_request_names() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    // One row per combination of the OPTIONAL members an update may carry
    // (`lambdaFunctionArn` and `accountDefaultForOperations`, both optional,
    // `smithy_query(query="operation",operation="UpdateCertificateProvider")`), with the
    // function the row expects to find stored afterwards. Re-stating the claim the
    // provider already holds is no conflict with itself.
    let rows: [(&str, Value, &str); 4] = [
        (
            "the function alone",
            json!({ "lambdaFunctionArn": REPLACEMENT_FUNCTION }),
            REPLACEMENT_FUNCTION,
        ),
        (
            "the claim alone",
            json!({ "accountDefaultForOperations": [operation] }),
            PROVIDER_FUNCTION,
        ),
        (
            "both members",
            json!({
                "lambdaFunctionArn": REPLACEMENT_FUNCTION,
                "accountDefaultForOperations": [operation],
            }),
            REPLACEMENT_FUNCTION,
        ),
        ("neither member", json!({}), PROVIDER_FUNCTION),
    ];
    // The names span the form `CertificateProviderName` admits (len 1..128, pattern
    // `^[\w=,@-]+$`): a plain name, one using every character class the pattern allows,
    // and the shortest legal one.
    for name in ["issuer", "issuer_2-A=,@", "a"] {
        for (case, body, expected_function) in &rows {
            // Each row gets its own account: a provider claims an operation's account
            // default, and an account holds one provider per operation.
            let emulator = Emulator::new();
            let created = emulator
                .create_provider(name, &Emulator::claim(&[operation]))
                .await;
            assert_eq!(
                created.status,
                200,
                "case {case} for {name}: expected the create to answer 200, got {} body {}",
                created.status,
                created.text()
            );

            let updated = emulator.update_provider(name, body).await;
            assert_eq!(
                updated.status,
                200,
                "case {case} for {name}: expected 200, got {} body {}",
                updated.status,
                updated.text()
            );
            assert_eq!(
                updated.json()["certificateProviderName"],
                json!(name),
                "case {case} for {name}: the update must name the provider it wrote, got {}",
                updated.text()
            );
            assert_eq!(
                updated.json()["certificateProviderArn"],
                created.json()["certificateProviderArn"],
                "case {case} for {name}: an update keeps the identity the create minted, got {}",
                updated.text()
            );

            // What the update wrote is what the reader hands back.
            let read = emulator.describe_provider(name).await;
            let described = read.json();
            assert_eq!(
                read.status,
                200,
                "case {case} for {name}: expected the read to answer 200, got {} body {}",
                read.status,
                read.text()
            );
            assert_eq!(
                described["lambdaFunctionArn"],
                json!(expected_function),
                "case {case} for {name}: expected {expected_function} stored, got {described}"
            );
            assert_eq!(
                described["accountDefaultForOperations"],
                json!([operation]),
                "case {case} for {name}: the claim must be the one the record holds, got \
                 {described}"
            );
            assert_eq!(
                described["certificateProviderArn"],
                created.json()["certificateProviderArn"],
                "case {case} for {name}: the read must report the ARN the create minted, got \
                 {described}"
            );
            let creation_date = described["creationDate"]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case} for {name}: no creationDate in {described}"));
            let last_modified = described["lastModifiedDate"].as_f64().unwrap_or_else(|| {
                panic!("case {case} for {name}: no lastModifiedDate in {described}")
            });
            assert!(
                last_modified >= creation_date,
                "case {case} for {name}: a written record is not modified before it was \
                 created, got creationDate {creation_date} lastModifiedDate {last_modified}"
            );

            // The listing reports the same identity after the update as before it.
            let listed = emulator.list_providers("").await;
            assert_eq!(
                listed.json()["certificateProviders"],
                json!([{
                    "certificateProviderName": name,
                    "certificateProviderArn": created.json()["certificateProviderArn"],
                }]),
                "case {case} for {name}: the account must still hold exactly its provider, got {}",
                listed.text()
            );
        }
    }
}

/// RULE: an update of a name the account holds no certificate provider under is refused
/// `ResourceNotFoundException` (404), with this family's own not-found sentence, and
/// creates nothing.
#[tokio::test]
async fn updating_a_certificate_provider_the_account_does_not_hold_is_not_found() {
    let operation = CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation");
    // The bodies span the optional members an update may carry, so the refusal does not
    // depend on what the request asked to change.
    let bodies = [
        ("the function alone", json!({ "lambdaFunctionArn": REPLACEMENT_FUNCTION })),
        (
            "the claim alone",
            json!({ "accountDefaultForOperations": [operation] }),
        ),
        ("neither member", json!({})),
    ];
    for (body_case, body) in &bodies {
        // Two ways an account can hold no provider under a name: one it never created,
        // and one whose provider it deleted.
        for (case, name, delete_first) in [
            ("a name the account never created", "absent", false),
            ("a name whose provider was deleted", "issuer", true),
        ] {
            let emulator = Emulator::new();
            if delete_first {
                assert_eq!(
                    emulator
                        .create_provider(name, &Emulator::claim(&[operation]))
                        .await
                        .status,
                    200,
                    "case {case} with {body_case}: the provider to be deleted must be created"
                );
                assert_eq!(
                    emulator.delete_provider(name).await.status,
                    200,
                    "case {case} with {body_case}: expected the delete to answer 200"
                );
            }

            let refused = emulator.update_provider(name, body).await;
            assert_eq!(
                refused.status,
                404,
                "case {case} with {body_case}: expected 404, got {} body {}",
                refused.status,
                refused.text()
            );
            assert_eq!(
                refused.error_code(),
                "ResourceNotFoundException",
                "case {case} with {body_case}: expected ResourceNotFoundException, got {}",
                refused.text()
            );
            assert_eq!(
                refused.error_message(),
                format!("Certificate provider with name [{name}] not found"),
                "case {case} with {body_case}: expected this family's not-found sentence, got {}",
                refused.text()
            );

            // A refused update creates nothing: the account still holds no provider under
            // that name.
            assert_eq!(
                emulator.describe_provider(name).await.status,
                404,
                "case {case} with {body_case}: a refused update must create nothing"
            );
            assert_eq!(
                emulator.list_providers("").await.json()["certificateProviders"],
                json!([]),
                "case {case} with {body_case}: a refused update must leave the account empty"
            );
        }
    }
}
