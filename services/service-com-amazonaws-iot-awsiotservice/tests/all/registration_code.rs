// SPDX-License-Identifier: CC0-1.0

//! The account's CA-CERTIFICATE REGISTRATION CODE, end to end over HTTP.
//!
//! Every test drives real requests through the `MultiServiceRouter` this crate's service is
//! mounted in, so routing, the restJson1 `GET /registrationcode` and
//! `DELETE /registrationcode` bindings, the handlers and the response serialization are all
//! exercised.
//!
//! The rules under test, none of them specific to one code:
//!
//! * `GetRegistrationCode` CREATES the account's registration code when it holds none and
//!   answers the held one otherwise — "IoT will create a registration code as part of this
//!   API call if the registration code doesn't exist or has been deleted. If you already
//!   have a registration code, this API call will return the same registration code"
//!   (`smithy_query(query="operation",operation="GetRegistrationCode")`) — so repeating the
//!   call, however many times, answers ONE value;
//! * the value satisfies the modelled `RegistrationCode` constraints (len 64..64, pattern
//!   `^(0x)?[a-fA-F0-9]+$`), whatever region the request was signed for;
//! * the code is DRAWN, not derived from the account or the region: a code deleted and asked
//!   for again is a NEW value, over any number of rounds, and two accounts that never shared
//!   a store hold different codes;
//! * `DeleteRegistrationCode` deletes the held code and answers 200, and an account that
//!   holds none is refused with the modelled `ResourceNotFoundException` (404) — the delete
//!   is the only one of the two operations that models a not-found error, because the read
//!   creates what it reports — leaving the account able to mint a code again.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_iot_awsiotservice::state::{
    MISSING_REGISTRATION_CODE_MESSAGE, REGISTRATION_CODE_LENGTH,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The URI the model binds both operations of this resource to.
const REGISTRATION_CODE_PATH: &str = "/registrationcode";

/// The error code a request for a resource this account does not hold is refused with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    router: MultiServiceRouter,
    region: String,
}

impl Emulator {
    /// An emulator serving requests signed for `region`.
    fn in_region(region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self {
            router,
            region: region.to_string(),
        }
    }

    /// An emulator in the region the rest of the suite uses when the region is not what a
    /// case varies.
    fn new() -> Self {
        Self::in_region("us-east-1")
    }

    async fn dispatch(&self, method: &str) -> Reply {
        let region = &self.region;
        let host = format!("iot.{region}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{REGISTRATION_CODE_PATH}"))
            .header("host", host)
            // The credential scope names the `iot` signing name the model's `aws.auth#sigv4`
            // trait declares, which is what routes the request.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::new())
            .expect("the registration-code request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn get_registration_code(&self) -> Reply {
        self.dispatch("GET").await
    }

    async fn delete_registration_code(&self) -> Reply {
        self.dispatch("DELETE").await
    }

    /// The code a successful read answers, with its modelled shape checked: every case that
    /// reads a code goes through here, so no row can assert on a value the model would have
    /// refused.
    async fn code(&self, case: &str) -> String {
        let reply = self.get_registration_code().await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected GetRegistrationCode to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        let code = reply.json()["registrationCode"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no registrationCode in {}", reply.text()))
            .to_string();
        assert!(
            is_modelled_registration_code(&code),
            "case {case}: expected a registration code of {REGISTRATION_CODE_LENGTH} \
             hexadecimal digits, got {code:?}"
        );
        code
    }

    /// Delete the held code and require the call to succeed.
    async fn delete(&self, case: &str) {
        let reply = self.delete_registration_code().await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected DeleteRegistrationCode to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Require a delete to be refused as a miss of the account's registration code.
    async fn expect_delete_not_found(&self, case: &str) {
        let reply = self.delete_registration_code().await;
        assert_eq!(
            reply.status,
            404,
            "case {case}: expected DeleteRegistrationCode to answer 404, got {} body {}",
            reply.status,
            reply.text()
        );
        let code = reply.error_code();
        assert!(
            code.ends_with(NOT_FOUND),
            "case {case}: expected error code {NOT_FOUND}, got {code:?}"
        );
        let message = reply.error_message();
        assert_eq!(
            message, MISSING_REGISTRATION_CODE_MESSAGE,
            "case {case}: expected message {MISSING_REGISTRATION_CODE_MESSAGE:?}, got {message:?}"
        );
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

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The `message` member a modelled error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// Whether `code` satisfies the modelled `RegistrationCode` constraints: `length 64..64`
/// and `pattern ^(0x)?[a-fA-F0-9]+$`
/// (`smithy_query(query="operation",operation="GetRegistrationCode")`).
fn is_modelled_registration_code(code: &str) -> bool {
    let digits = code.strip_prefix("0x").unwrap_or(code);
    code.len() == REGISTRATION_CODE_LENGTH
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_hexdigit())
}

#[tokio::test]
async fn a_first_read_mints_a_modelled_code_whatever_region_signed_it() {
    // The regions differ in length and in form (two-part and three-part names, and one AWS
    // partition's own prefix), because the code is the account's and nothing about it is
    // selected by the region a request was signed for.
    for region in [
        "us-east-1",
        "eu-west-2",
        "ap-southeast-4",
        "us-gov-east-1",
        "me-central-1",
    ] {
        let emulator = Emulator::in_region(region);
        let code = emulator.code(region).await;
        assert!(
            is_modelled_registration_code(&code),
            "case {region}: expected a modelled registration code, got {code:?}"
        );
    }
}

#[tokio::test]
async fn repeating_the_read_answers_one_code() {
    // The rows differ in HOW MANY times the read is repeated, which is what "this API call
    // will return the same registration code" generalises over.
    for reads in [2_usize, 3, 8] {
        let case = format!("{reads} reads");
        let emulator = Emulator::new();
        let first = emulator.code(&case).await;
        for read in 1..reads {
            let later = emulator.code(&case).await;
            assert_eq!(
                later, first,
                "case {case}: read {read} expected the held code {first:?}, got {later:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_deleted_code_is_replaced_by_a_new_one() {
    // The rows differ in HOW MANY delete-and-read rounds the account goes through: each
    // round's code is well formed and none of them repeats an earlier one, which is what
    // makes the code drawn rather than derived from the account.
    for rounds in [1_usize, 2, 5] {
        let case = format!("{rounds} rounds");
        let emulator = Emulator::new();
        let mut seen = BTreeSet::new();
        seen.insert(emulator.code(&case).await);
        for round in 1..=rounds {
            emulator.delete(&case).await;
            let minted = emulator.code(&case).await;
            assert!(
                seen.insert(minted.clone()),
                "case {case}: round {round} expected a code the account has not held before, \
                 got {minted:?} among {seen:?}"
            );
        }
        assert_eq!(
            seen.len(),
            rounds + 1,
            "case {case}: expected one distinct code per round plus the first, got {seen:?}"
        );
    }
}

#[tokio::test]
async fn two_separate_accounts_hold_different_codes() {
    // Stores that never shared an engine: a code derived from the account or the region
    // would repeat here, a drawn one does not. The rows differ in the region each store
    // serves, including two stores serving the SAME region.
    let codes: BTreeSet<String> = {
        let mut codes = BTreeSet::new();
        for (case, region) in [
            ("first store", "us-east-1"),
            ("second store, same region", "us-east-1"),
            ("third store, other region", "eu-central-1"),
        ] {
            let emulator = Emulator::in_region(region);
            codes.insert(emulator.code(case).await);
        }
        codes
    };
    assert_eq!(
        codes.len(),
        3,
        "case separate stores: expected one code per store, got {codes:?}"
    );
}

#[tokio::test]
async fn deleting_a_code_the_account_does_not_hold_is_refused() {
    // The rows differ in HOW the account came to hold no code: one that was never asked for
    // a code, one whose code was deleted, and one that was refused a delete already — the
    // refusal changes nothing, so it is still a miss.
    let never_asked = Emulator::new();
    never_asked.expect_delete_not_found("never asked for a code").await;
    never_asked
        .expect_delete_not_found("refused a delete already")
        .await;

    let deleted = Emulator::new();
    let first = deleted.code("deleted its code").await;
    deleted.delete("deleted its code").await;
    deleted.expect_delete_not_found("deleted its code").await;

    // A refused delete leaves the account able to mint again, and what it mints is new.
    let minted = deleted.code("deleted its code").await;
    assert_ne!(
        minted, first,
        "case deleted its code: expected a new code after the delete, got {minted:?}"
    );
}
