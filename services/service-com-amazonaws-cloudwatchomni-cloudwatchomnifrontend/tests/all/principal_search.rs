// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for CloudWatchOmni's `SearchPrincipals`.
//!
//! The operation "searches Identity Center for users and groups in a domain", so this
//! suite mounts CloudWatchOmni and the IAM Identity Center identity store on ONE shared
//! engine: the identities a search reports are created through the identity store's own
//! `CreateUser`/`CreateGroup` and read back through its own `DescribeUser`/`DescribeGroup`,
//! so nothing here asserts against a second copy of them kept by CloudWatchOmni.
//!
//! The RULES asserted, each over a table rather than one recorded request:
//!
//! * a search of a domain configured with Identity Center reports the Identity Center
//!   users and groups this emulator holds whose user name, display name or id matches the
//!   term, with `*` reporting every one of them;
//! * every reported member is the identity store's own record: `principalId`,
//!   `principalType`, `displayName`, `userName` (users only) and `description`;
//! * a domain NOT configured with Identity Center is refused with `ValidationException`
//!   (400) whatever its other providers, and a domain this account does not hold is
//!   `ResourceNotFoundException` (404);
//! * a page reports at most `maxResults` matches and a walk of the pages reports every
//!   match exactly once;
//! * a `nextToken` this service did not mint is refused rather than answered;
//! * a search WRITES nothing: the identity store reports the same records, at the same
//!   revisions, after one.
//!
//! The rows differ along the search term's form (the wildcard, an exact user name, a
//! substring, a different case, an id, a term nobody matches), along the kind of principal
//! matched (user, group), along the store the identity lives in, along the page size and
//! along the domain's provider set — so no row is the recorded case restated. Every
//! assertion names its case and the actual value.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::CborCodec;
use smithy_server_runtime::{
    EmbeddedEmulator, MultiServiceRouter, Operation, OperationContext, OperationError, Service,
};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;
use service_com_amazonaws_identitystore_awsidentitystore as identitystore;

use identitystore::ops::create_group::CreateGroup;
use identitystore::ops::create_user::CreateUser;
use identitystore::ops::describe_group::DescribeGroup;
use identitystore::ops::describe_user::DescribeUser;
use identitystore::ops::types::{
    CreateGroupInput, CreateUserInput, DescribeGroupInput, DescribeGroupOutput, DescribeUserInput,
    DescribeUserOutput,
};
use omni::ops::create_domain::CreateDomain;
use omni::ops::search_principals::{SearchPrincipals, SearchPrincipalsError};
use omni::ops::types::{
    CreateDomainInput, SearchPrincipalsInput, SearchPrincipalsOutput, SEARCH_PRINCIPALS_INPUT_SCHEMA,
};
use omni::state::{PRINCIPAL_TYPE_GROUP, PRINCIPAL_TYPE_USER, SEARCH_ALL_PRINCIPALS};

/// The region the suite signs for.
const REGION: &str = "us-west-2";

/// The access key id the suite's `Authorization` header carries.
const ACCESS_KEY_ID: &str = "AKIDEMULATOR";

/// The identity store the Identity Center instance of this account points a client at,
/// named by the service that owns it rather than transcribed here.
const STORE: &str = identitystore::state::SEED_IDENTITY_STORE_ID;

/// A SECOND identity store, in the `d-` form the identity store's own `IdentityStoreId`
/// pattern admits: a search covers every store this emulator holds, so a row lives here.
const OTHER_STORE: &str = "d-0123456789";

/// A well-formed `DomainId` (CloudWatchOmni's modelled `^d-[0-9a-z]{1,25}$`) that names no
/// domain this account holds.
const MISSING_DOMAIN_ID: &str = "d-0000000000";

/// A SigV4 `Authorization` header whose credential scope names a signing name and the
/// region the handlers record.
fn signed_request_parts() -> http::request::Parts {
    let (parts, ()) = http::Request::builder()
        .method("POST")
        .uri("/service/CloudWatchOmniFrontend/operation/Unused")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/cloudwatch/aws4_request, \
                 SignedHeaders=host, Signature=emulator"
            ),
        )
        .body(())
        .expect("a well-formed request")
        .into_parts();
    parts
}

/// One emulator: one shared engine with CloudWatchOmni and the identity store mounted on
/// it, so a user the identity store wrote is one CloudWatchOmni's search can read.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    omni_service: omni::EmulatedService<engine::InMemoryEngine>,
    identitystore_service: identitystore::EmulatedService<engine::InMemoryEngine>,
    router: MultiServiceRouter,
    parts: http::request::Parts,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(omni::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(identitystore::EmulatedService::new(Arc::clone(&engine)));
        Self {
            omni_service: omni::EmulatedService::new(Arc::clone(&engine)),
            identitystore_service: identitystore::EmulatedService::new(Arc::clone(&engine)),
            engine,
            router,
            parts: signed_request_parts(),
        }
    }

    /// The request metadata a CloudWatchOmni handler reads.
    fn omni_ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.omni_service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// The request metadata an identity store handler reads.
    fn identitystore_ctx(&self) -> OperationContext<'_> {
        OperationContext::new(
            &self.parts,
            self.identitystore_service.definition().identity(),
            EmbeddedEmulator::from_router(self.router.clone()),
        )
    }

    /// A domain configured with `identity_providers`, through the handler that owns it.
    ///
    /// The create carries the `clientToken` it is admitted only with, derived from the
    /// domain's own name so two domains never share one.
    async fn domain(&self, case: &str, name: &str, identity_providers: &[&str]) -> String {
        CreateDomain::new(Arc::clone(&self.engine))
            .handle(
                CreateDomainInput {
                    name: format!("omni-{name}"),
                    identity_providers: identity_providers
                        .iter()
                        .map(|provider| provider.to_string())
                        .collect(),
                    client_token: Some(format!("token-{name}")),
                    ..Default::default()
                },
                &self.omni_ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateDomain failed: {error:?}"))
            .domain
            .domain_id
    }

    /// An Identity Center user of `store`, through the identity store's own create.
    async fn user(
        &self,
        case: &str,
        store: &str,
        user_name: Option<&str>,
        display_name: Option<&str>,
    ) -> String {
        CreateUser::new(Arc::clone(&self.engine))
            .handle(
                CreateUserInput {
                    identity_store_id: store.to_string(),
                    user_name: user_name.map(str::to_string),
                    display_name: display_name.map(str::to_string),
                    ..Default::default()
                },
                &self.identitystore_ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateUser failed: {error:?}"))
            .user_id
    }

    /// An Identity Center group of `store`, through the identity store's own create.
    async fn group(
        &self,
        case: &str,
        store: &str,
        display_name: Option<&str>,
        description: Option<&str>,
    ) -> String {
        CreateGroup::new(Arc::clone(&self.engine))
            .handle(
                CreateGroupInput {
                    identity_store_id: store.to_string(),
                    display_name: display_name.map(str::to_string),
                    description: description.map(str::to_string),
                },
                &self.identitystore_ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: CreateGroup failed: {error:?}"))
            .group_id
    }

    /// What the identity store's OWN read reports for a user.
    async fn described_user(&self, case: &str, store: &str, user_id: &str) -> DescribeUserOutput {
        DescribeUser::new(Arc::clone(&self.engine))
            .handle(
                DescribeUserInput {
                    identity_store_id: store.to_string(),
                    user_id: user_id.to_string(),
                    extensions: None,
                },
                &self.identitystore_ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: DescribeUser failed: {error:?}"))
    }

    /// What the identity store's OWN read reports for a group.
    async fn described_group(&self, case: &str, store: &str, group_id: &str) -> DescribeGroupOutput {
        DescribeGroup::new(Arc::clone(&self.engine))
            .handle(
                DescribeGroupInput {
                    identity_store_id: store.to_string(),
                    group_id: group_id.to_string(),
                },
                &self.identitystore_ctx(),
            )
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: DescribeGroup failed: {error:?}"))
    }

    async fn search(
        &self,
        input: SearchPrincipalsInput,
    ) -> Result<SearchPrincipalsOutput, SearchPrincipalsError> {
        SearchPrincipals::new(Arc::clone(&self.engine))
            .handle(input, &self.omni_ctx())
            .await
    }

    /// A search that must succeed.
    async fn searched(&self, case: &str, input: SearchPrincipalsInput) -> SearchPrincipalsOutput {
        self.search(input)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: SearchPrincipals failed: {error:?}"))
    }

    /// Dispatch one rpcv2Cbor request over HTTP and return the status and body, so a
    /// refusal is read the way a client reads it.
    async fn over_the_wire<I: SerializableStruct>(
        &self,
        operation: &str,
        input_schema: &Schema,
        input: &I,
    ) -> (u16, bytes::Bytes) {
        let codec = CborCodec::default();
        let mut serializer = codec.create_serializer();
        serializer
            .write_struct(input_schema, input)
            .expect("the input shape serializes");
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!(
                "/service/CloudWatchOmniFrontend/operation/{operation}"
            ))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor");
        for (name, value) in &self.parts.headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::from(serializer.finish()))
            .expect("a well-formed rpcv2Cbor request");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.body().clone())
    }
}

/// Require a modelled failure with the status and error code the model declares for it.
fn expect_modelled_error<O: std::fmt::Debug, E: OperationError + std::fmt::Debug>(
    case: &str,
    result: Result<O, E>,
    expected_status: u16,
    expected_code: &str,
) {
    match result {
        Ok(output) => panic!(
            "case {case:?}: expected {expected_code} ({expected_status}), got Ok({output:?})"
        ),
        Err(error) => {
            assert_eq!(
                error.shape_name(),
                expected_code,
                "case {case:?}: expected error code {expected_code}, got {error:?}"
            );
            assert_eq!(
                error.http_status(),
                expected_status,
                "case {case:?}: expected HTTP {expected_status} for {expected_code}, got {}",
                error.http_status()
            );
        }
    }
}

/// A search input naming a domain and a term, with the paging members left out.
fn search_input(domain_id: &str, search_query: &str) -> SearchPrincipalsInput {
    SearchPrincipalsInput {
        domain_id: domain_id.to_string(),
        search_query: search_query.to_string(),
        ..Default::default()
    }
}

/// The identities one fixture writes into the identity stores, so every expectation below
/// is stated in terms of records the OWNING service holds.
struct Directory {
    /// A user with both a user name and a display name.
    named_user: String,
    /// A user with a user name and NO display name, so its `displayName` falls back.
    nameless_user: String,
    /// A user whose user name is mixed case, for the case-insensitivity row.
    mixed_case_user: String,
    /// A user of the SECOND identity store.
    other_store_user: String,
    /// A group with a description.
    described_group: String,
    /// A group with no description.
    plain_group: String,
}

impl Directory {
    /// Every identity the fixture wrote, as the ids a result reports them by.
    fn every_principal(&self) -> BTreeSet<String> {
        [
            &self.named_user,
            &self.nameless_user,
            &self.mixed_case_user,
            &self.other_store_user,
            &self.described_group,
            &self.plain_group,
        ]
        .into_iter()
        .cloned()
        .collect()
    }
}

/// Write the fixture's identities through the identity store's own creates.
async fn directory(emulator: &Emulator, case: &str) -> Directory {
    Directory {
        named_user: emulator
            .user(case, STORE, Some("aisha.khan"), Some("Aisha Khan"))
            .await,
        nameless_user: emulator.user(case, STORE, Some("bo.nguyen"), None).await,
        mixed_case_user: emulator
            .user(case, STORE, Some("Carla.Mendes"), Some("Carla Mendes"))
            .await,
        other_store_user: emulator
            .user(case, OTHER_STORE, Some("dmitri.volkov"), Some("Dmitri Volkov"))
            .await,
        described_group: emulator
            .group(case, STORE, Some("platform-oncall"), Some("The on-call rota"))
            .await,
        plain_group: emulator.group(case, STORE, Some("auditors"), None).await,
    }
}

/// The ids a search reported, as a set: the order two identities created in one clock tick
/// come back in is the identity store's own, so the rows below assert WHICH principals
/// matched rather than their sequence.
fn reported_ids(output: &SearchPrincipalsOutput) -> BTreeSet<String> {
    output
        .results
        .iter()
        .map(|result| result.principal_id.clone())
        .collect()
}

#[tokio::test]
async fn a_search_reports_the_identity_center_identities_the_term_matches() {
    let emulator = Emulator::new();
    let directory = directory(&emulator, "matching").await;
    let domain_id = emulator.domain("matching", "search", &["IDC"]).await;

    // (case, the search term, the ids that must come back)
    let rows: Vec<(&str, String, BTreeSet<String>)> = vec![
        (
            "the wildcard lists every principal",
            SEARCH_ALL_PRINCIPALS.to_string(),
            directory.every_principal(),
        ),
        (
            "an exact user name matches that user only",
            "aisha.khan".to_string(),
            BTreeSet::from([directory.named_user.clone()]),
        ),
        (
            "a substring of a display name matches",
            "Mendes".to_string(),
            BTreeSet::from([directory.mixed_case_user.clone()]),
        ),
        (
            "a term in another case matches",
            "CARLA.MENDES".to_string(),
            BTreeSet::from([directory.mixed_case_user.clone()]),
        ),
        (
            "a user name with no display name beside it matches",
            "bo.nguyen".to_string(),
            BTreeSet::from([directory.nameless_user.clone()]),
        ),
        (
            "a principal id matches the principal it names",
            directory.plain_group.clone(),
            BTreeSet::from([directory.plain_group.clone()]),
        ),
        (
            "a group display name matches the group",
            "platform-oncall".to_string(),
            BTreeSet::from([directory.described_group.clone()]),
        ),
        (
            "a user of the second identity store is searched too",
            "volkov".to_string(),
            BTreeSet::from([directory.other_store_user.clone()]),
        ),
        (
            "a term nothing carries matches nothing",
            "nobody-by-this-name".to_string(),
            BTreeSet::new(),
        ),
    ];

    for (case, term, expected) in rows {
        let output = emulator
            .searched(case, search_input(&domain_id, &term))
            .await;
        let actual = reported_ids(&output);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected principals {expected:?}, got {actual:?}"
        );
        assert!(
            output.next_token.is_none(),
            "case {case:?}: one page holds every match, so no nextToken is owed, got {:?}",
            output.next_token
        );
    }
}

#[tokio::test]
async fn every_reported_member_is_the_identity_stores_own_record() {
    let case = "projection";
    let emulator = Emulator::new();
    let directory = directory(&emulator, case).await;
    let domain_id = emulator.domain(case, "projection", &["IDC"]).await;

    let output = emulator
        .searched(case, search_input(&domain_id, SEARCH_ALL_PRINCIPALS))
        .await;

    // (the id, the store it lives in, whether it is a user)
    let rows = [
        (&directory.named_user, STORE, true),
        (&directory.nameless_user, STORE, true),
        (&directory.mixed_case_user, STORE, true),
        (&directory.other_store_user, OTHER_STORE, true),
        (&directory.described_group, STORE, false),
        (&directory.plain_group, STORE, false),
    ];

    for (id, store, is_user) in rows {
        let result = output
            .results
            .iter()
            .find(|result| result.principal_id == *id)
            .unwrap_or_else(|| {
                panic!("case {id:?}: the search did not report this principal: {output:?}")
            });
        if is_user {
            let user = emulator.described_user(id, store, id).await;
            let expected_display_name = user
                .display_name
                .clone()
                .or_else(|| user.user_name.clone())
                .unwrap_or_else(|| user.user_id.clone());
            assert_eq!(
                (
                    result.principal_type.as_str(),
                    result.display_name.as_str(),
                    result.user_name.as_deref(),
                    result.description.as_deref(),
                ),
                (
                    PRINCIPAL_TYPE_USER,
                    expected_display_name.as_str(),
                    user.user_name.as_deref(),
                    None,
                ),
                "case {id:?}: expected the user the identity store reports ({user:?}), got \
                 {result:?}"
            );
        } else {
            let group = emulator.described_group(id, store, id).await;
            let expected_display_name = group
                .display_name
                .clone()
                .unwrap_or_else(|| group.group_id.clone());
            assert_eq!(
                (
                    result.principal_type.as_str(),
                    result.display_name.as_str(),
                    result.user_name.as_deref(),
                    result.description.as_deref(),
                ),
                (
                    PRINCIPAL_TYPE_GROUP,
                    expected_display_name.as_str(),
                    None,
                    group.description.as_deref(),
                ),
                "case {id:?}: expected the group the identity store reports ({group:?}), got \
                 {result:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_search_changes_nothing_a_later_caller_sees() {
    let case = "readonly";
    let emulator = Emulator::new();
    let directory = directory(&emulator, case).await;
    let domain_id = emulator.domain(case, "readonly", &["IAM", "IDC"]).await;

    let before = emulator
        .described_user(case, STORE, &directory.named_user)
        .await;
    let group_before = emulator
        .described_group(case, STORE, &directory.described_group)
        .await;

    for term in [SEARCH_ALL_PRINCIPALS, "aisha.khan", "platform"] {
        emulator.searched(term, search_input(&domain_id, term)).await;
    }

    let after = emulator
        .described_user(case, STORE, &directory.named_user)
        .await;
    assert_eq!(
        (
            after.revision.as_str(),
            after.user_name.as_deref(),
            after.display_name.as_deref()
        ),
        (
            before.revision.as_str(),
            before.user_name.as_deref(),
            before.display_name.as_deref()
        ),
        "case {case:?}: searching changed the user the identity store reports: before \
         {before:?}, after {after:?}"
    );
    let group_after = emulator
        .described_group(case, STORE, &directory.described_group)
        .await;
    assert_eq!(
        (
            group_after.revision.as_str(),
            group_after.display_name.as_deref(),
            group_after.description.as_deref()
        ),
        (
            group_before.revision.as_str(),
            group_before.display_name.as_deref(),
            group_before.description.as_deref()
        ),
        "case {case:?}: searching changed the group the identity store reports: before \
         {group_before:?}, after {group_after:?}"
    );
}

#[tokio::test]
async fn only_a_domain_configured_with_identity_center_is_searched() {
    // (case, the domain's identityProviders, whether the search is served)
    // `IdentityProvider` declares exactly IAM and IDC
    // (`smithy_query(query="shape", shape="IdentityProvider")`), so these are every set a
    // domain can be configured with.
    let rows = [
        ("IAM only", vec!["IAM"], false),
        ("IDC only", vec!["IDC"], true),
        ("IAM and IDC", vec!["IAM", "IDC"], true),
        ("IDC and IAM", vec!["IDC", "IAM"], true),
    ];

    for (index, (case, providers, served)) in rows.into_iter().enumerate() {
        // An account holds at most one domain, so each row runs against an emulator of
        // its own rather than adding a second domain beside the previous row's.
        let emulator = Emulator::new();
        let directory = directory(&emulator, case).await;
        let every = directory.every_principal();
        let domain_id = emulator
            .domain(case, &format!("providers-{index}"), &providers)
            .await;
        let result = emulator
            .search(search_input(&domain_id, SEARCH_ALL_PRINCIPALS))
            .await;
        if served {
            let output = result.unwrap_or_else(|error| {
                panic!("case {case:?}: expected the search to be served, got {error:?}")
            });
            let actual = reported_ids(&output);
            assert_eq!(
                actual, every,
                "case {case:?}: expected principals {every:?}, got {actual:?}"
            );
        } else {
            expect_modelled_error(case, result, 400, "ValidationException");
            // The refusal a client reads over the wire states the condition, naming the
            // domain it refused and the requirement that failed, through the one wording
            // the store owns.
            let (status, body) = emulator
                .over_the_wire(
                    "SearchPrincipals",
                    SEARCH_PRINCIPALS_INPUT_SCHEMA,
                    &search_input(&domain_id, SEARCH_ALL_PRINCIPALS),
                )
                .await;
            let rendered = String::from_utf8_lossy(&body).to_string();
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 over the wire, got {status}: {rendered:?}"
            );
            // The sentence is written out here, with only the domain id interpolated from the
            // request, so a change to the wording is visible in this test rather than
            // following the implementation.
            let expected = format!(
                "Domain '{domain_id}' is not configured with Identity Center. SearchPrincipals \
                 requires a domain whose identityProviders include IDC."
            );
            assert!(
                rendered.contains(&expected),
                "case {case:?}: expected the refusal to state {expected:?}, got {rendered:?}"
            );
            assert_eq!(
                omni::state::not_identity_center_message(&domain_id),
                expected,
                "case {case:?}: the store's one wording of this refusal must be the sentence \
                 this test pins"
            );
        }
    }
}

#[tokio::test]
async fn a_domain_this_account_does_not_hold_is_reported_as_not_found() {
    let emulator = Emulator::new();
    directory(&emulator, "missing").await;
    // A domain that EXISTS is served, so the refusal below is about the id and not about
    // an empty store.
    let domain_id = emulator.domain("missing", "present", &["IDC"]).await;
    emulator
        .searched("a domain that exists", search_input(&domain_id, "aisha"))
        .await;

    expect_modelled_error(
        "a well-formed domain id naming nothing",
        emulator
            .search(search_input(MISSING_DOMAIN_ID, SEARCH_ALL_PRINCIPALS))
            .await,
        404,
        "ResourceNotFoundException",
    );
}

#[tokio::test]
async fn a_walk_of_the_pages_reports_every_match_exactly_once() {
    let emulator = Emulator::new();
    let directory = directory(&emulator, "paging").await;
    let every = directory.every_principal();
    let domain_id = emulator.domain("paging", "paging", &["IDC"]).await;

    // `maxResults` is modelled @range(min: 1, max: 100) with a documented default of 100:
    // both published bounds, sizes inside the collection, one that is exactly the
    // collection, and the request that names none.
    for page_size in [Some(1), Some(2), Some(5), Some(6), Some(100), None] {
        let case = format!("maxResults {page_size:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut next_token = None;
        // Bounded: one page per principal plus one, so a token that never clears fails
        // here instead of spinning.
        for _ in 0..=every.len() {
            let output = emulator
                .searched(
                    &case,
                    SearchPrincipalsInput {
                        domain_id: domain_id.clone(),
                        search_query: SEARCH_ALL_PRINCIPALS.to_string(),
                        max_results: page_size,
                        next_token: next_token.clone(),
                    },
                )
                .await;
            if let Some(size) = page_size {
                assert!(
                    output.results.len() <= size as usize,
                    "case {case:?}: a page holds at most {size} results, got {}",
                    output.results.len()
                );
            }
            assert!(
                !output.results.is_empty() || every.is_empty(),
                "case {case:?}: a page of a non-empty collection must report a match"
            );
            seen.extend(output.results.iter().map(|result| result.principal_id.clone()));
            next_token = output.next_token.clone();
            if next_token.is_none() {
                break;
            }
        }
        assert!(
            next_token.is_none(),
            "case {case:?}: the walk did not finish, left holding {next_token:?}"
        );
        let unique: BTreeSet<String> = seen.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            seen.len(),
            "case {case:?}: a walk reported a principal twice: {seen:?}"
        );
        assert_eq!(
            unique, every,
            "case {case:?}: expected the walk to report {every:?}, got {unique:?}"
        );
    }
}

#[tokio::test]
async fn a_next_token_this_service_did_not_mint_is_refused() {
    let emulator = Emulator::new();
    directory(&emulator, "tokens").await;
    let domain_id = emulator.domain("tokens", "tokens", &["IDC"]).await;

    // A token is opaque text, so the rows span the forms a caller can send: empty, text
    // that is not a token at all, and a token minted for ANOTHER collection of this same
    // service.
    for (case, token) in [
        ("empty", String::new()),
        ("not a token", "not-a-token".to_string()),
        (
            "another collection's token",
            hex::encode("spaces:1".as_bytes()),
        ),
    ] {
        let mut input = search_input(&domain_id, SEARCH_ALL_PRINCIPALS);
        input.next_token = Some(token.clone());
        expect_modelled_error(case, emulator.search(input).await, 400, "ValidationException");
    }

    // The token the service DID mint resumes the walk.
    let first = emulator
        .searched(
            "the minted token",
            SearchPrincipalsInput {
                domain_id: domain_id.clone(),
                search_query: SEARCH_ALL_PRINCIPALS.to_string(),
                max_results: Some(2),
                next_token: None,
            },
        )
        .await;
    let token = first
        .next_token
        .clone()
        .expect("a collection larger than the page owes a nextToken");
    let second = emulator
        .searched(
            "the minted token",
            SearchPrincipalsInput {
                domain_id,
                search_query: SEARCH_ALL_PRINCIPALS.to_string(),
                max_results: Some(2),
                next_token: Some(token),
            },
        )
        .await;
    let first_ids = reported_ids(&first);
    let second_ids = reported_ids(&second);
    assert!(
        first_ids.is_disjoint(&second_ids),
        "the second page must not repeat the first: {first_ids:?} and {second_ids:?}"
    );
}
