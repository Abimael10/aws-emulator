// SPDX-License-Identifier: CC0-1.0

//! The refusal a serverless `CacheUsageLimits` bound OUTSIDE its member's published
//! range is owed, and the sentence it is worded in.
//!
//! The rule under test: a `CacheUsageLimits` bound a request names must either lie
//! within the range the API publishes for that member or be the zero that removes
//! the limit, so every operation that would STORE such an envelope
//! (`CreateServerlessCache`, `ModifyServerlessCache`) refuses a bound outside it with
//! the modelled client fault — HTTP 400, `awsQuery` code `InvalidParameterValue` — in
//! a sentence naming the offending member and the bounds it publishes, and writes
//! nothing; a bound inside the range is accepted, persisted exactly as sent and
//! reported back by a later read.
//!
//! This is a DIFFERENT rule from the contradictory-envelope one the
//! `serverless_usage_limit_envelope` suite covers (a `Minimum` above its own
//! `Maximum`, which can hold with both bounds inside the range): a single bound is
//! judged against the member's range on its own, before the two are compared with
//! each other, and it carries its own sentence.
//!
//! `DataStorage{Minimum,Maximum}` and `ECPUPerSecond{Minimum,Maximum}` are plain
//! `IntegerOptional` with no modelled `@range`
//! (`smithy_query(query="shape",shape="DataStorage,ECPUPerSecond")`), so no boundary
//! check stops an out-of-range bound and no SDK refuses one client-side: the refusal
//! is the operation's own, and it is what a client branches on — a non-modelled
//! status is neither a client fault nor final to any SDK.
//!
//! The tables are inputs, not one recorded request: the rows differ along what the
//! rule generalises over — which member carries the bound, which bound it is, which
//! side of the range the value falls on and how far out it is (one past the edge, far
//! past it, and the extreme the member's type can carry) — and the accepted rows
//! walk each published edge and the zero that removes a limit. A storage row asserts
//! the WHOLE sentence, because AWS publishes that member's two bounds; an ECPU row
//! asserts the member the sentence names and its fixed frame, because the numbers
//! between the two halves are the floor the service publishes rather than one the
//! model or the documentation fixes — WHICH floor an ECPU bound is held to is a
//! question of its own and no row here pins it, so the ECPU rows are the values below
//! any floor a per-second ECPU bound could have. Nothing reaches into
//! either service's state: the placement exists only because Amazon EC2 answered for
//! it, and a cache's limits only because a later `DescribeServerlessCaches` reported
//! them.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// The one value `DataStorage$Unit` (a required `DataStorageUnit` enum) publishes.
const STORAGE_UNIT: &str = "GB";

/// The engine the rows run on, the one the recorded sequence this rule was reported
/// against used.
const ENGINE: &str = "redis";

/// The form pairs one request sends beside its action.
type Pairs = Vec<(String, String)>;

/// One row: the case name, the usage-limit pairs the request sends, and the whole
/// sentence the refusal has to carry.
type RefusalRow = (&'static str, Pairs, String);

/// One accepted row: the case name, the usage-limit pairs the request sends, and
/// every `(element suffix, text)` a later read of the cache must report.
type AcceptedRow = (&'static str, Pairs, Vec<(&'static str, String)>);

/// The sentence a bound outside a range bounded at BOTH ends is refused with. Its
/// variable parts are the member the bound belongs to and that member's two
/// published bounds; the rest is fixed text, and it quotes nothing of the value that
/// broke the rule.
fn bounded_sentence(member: &str, lowest: i32, highest: i32) -> String {
    format!(
        "Invalid value for {member} limit. Must be between {lowest} and {highest}. You can set \
         the value to zero to remove the limit."
    )
}

/// The FIXED frame of the sentence, whichever member and whichever bounds a refusal
/// quotes between them: it opens by naming the member whose limit the request got
/// wrong and closes with the way to remove that limit instead of setting one.
///
/// Used where the numbers in the middle are the ones the SERVICE publishes for a
/// member rather than ones the model or the documentation fixes, so a row asserts
/// what the rule fixes — which member is named, and that the sentence is the
/// limit-value one — without freezing a bound this issue is not about.
fn sentence_frame(member: &str) -> (String, &'static str) {
    (
        format!("Invalid value for {member} limit."),
        "You can set the value to zero to remove the limit.",
    )
}

/// The `DataStorage` pairs for the bounds a row names, with the unit a named storage
/// limit is always measured in.
fn storage(minimum: Option<i32>, maximum: Option<i32>) -> Pairs {
    let mut pairs = vec![(
        "CacheUsageLimits.DataStorage.Unit".to_string(),
        STORAGE_UNIT.to_string(),
    )];
    if let Some(minimum) = minimum {
        pairs.push((
            "CacheUsageLimits.DataStorage.Minimum".to_string(),
            minimum.to_string(),
        ));
    }
    if let Some(maximum) = maximum {
        pairs.push((
            "CacheUsageLimits.DataStorage.Maximum".to_string(),
            maximum.to_string(),
        ));
    }
    pairs
}

/// The `ECPUPerSecond` pairs for the bounds a row names; see [`storage`].
fn ecpu(minimum: Option<i32>, maximum: Option<i32>) -> Pairs {
    let mut pairs = Vec::new();
    if let Some(minimum) = minimum {
        pairs.push((
            "CacheUsageLimits.ECPUPerSecond.Minimum".to_string(),
            minimum.to_string(),
        ));
    }
    if let Some(maximum) = maximum {
        pairs.push((
            "CacheUsageLimits.ECPUPerSecond.Maximum".to_string(),
            maximum.to_string(),
        ));
    }
    pairs
}

/// `left` followed by `right`, for a row that names both members.
fn both(left: Pairs, right: Pairs) -> Pairs {
    let mut pairs = left;
    pairs.extend(right);
    pairs
}

/// An emulator and the subnets the ACCOUNT has, as Amazon EC2 — the owner of subnets,
/// and the one source an ElastiCache placement is resolved against — reports them.
///
/// The recorded sequence this rule was reported against places the cache in the
/// account's own subnets, so every row here does too: a request that is refused on
/// its usage limits is refused with the placement resolving, never because of it.
struct Fixture {
    emulator: Emulator,
    subnets: Vec<String>,
}

impl Fixture {
    async fn stand_up() -> Self {
        let emulator = Emulator::new();
        // The default subnets of the account's Region, which share one VPC — the
        // placement a serverless cache's VPC endpoint is deployed into.
        let subnets: Vec<String> = emulator
            .account_subnets()
            .await
            .into_iter()
            .filter(|subnet| subnet.default_for_az)
            .take(3)
            .map(|subnet| subnet.id)
            .collect();
        assert!(
            subnets.len() >= 2,
            "EC2 must report the account's default subnets for a placement, got {subnets:?}"
        );
        Self { emulator, subnets }
    }

    /// The fixture's subnets as the `SubnetIds` pairs a request names them in.
    fn subnet_pairs(&self) -> Pairs {
        self.subnets
            .iter()
            .enumerate()
            .map(|(index, id)| (format!("SubnetIds.SubnetId.{}", index + 1), id.clone()))
            .collect()
    }

    /// `CreateServerlessCache` for `name` in the account's subnets, with the
    /// usage-limit pairs a row sends.
    async fn create(&self, name: &str, limits: &[(String, String)]) -> Reply {
        let mut request = params(&[("ServerlessCacheName", name), ("Engine", ENGINE)]);
        request.extend(limits.iter().cloned());
        request.extend(self.subnet_pairs());
        self.emulator.call("CreateServerlessCache", &request).await
    }

    /// `ModifyServerlessCache` for `name` with the usage-limit pairs a row sends.
    async fn modify(&self, name: &str, limits: &[(String, String)]) -> Reply {
        let mut request = params(&[("ServerlessCacheName", name)]);
        request.extend(limits.iter().cloned());
        self.emulator.call("ModifyServerlessCache", &request).await
    }

    /// `DescribeServerlessCaches` for the one cache `name` resolves to.
    async fn describe(&self, name: &str) -> Reply {
        self.emulator
            .call(
                "DescribeServerlessCaches",
                &params(&[("ServerlessCacheName", name)]),
            )
            .await
    }

    /// The cache `name` resolves to, once the clock owes it `status`.
    async fn settled(&self, case: &str, name: &str, status: &str) -> Reply {
        poll_until(
            &self.emulator,
            REGION,
            case,
            &format!("the cache reaching {status}"),
            "DescribeServerlessCaches",
            &params(&[("ServerlessCacheName", name)]),
            |reply| reply.text("member/Status") == Some(status),
        )
        .await
    }
}

/// Assert that `reply` is the modelled client refusal a bad usage limit is owed:
/// HTTP 400 with the `awsQuery` code `InvalidParameterValue`.
fn assert_client_refusal(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: an out-of-range usage limit must be refused with HTTP 400, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        "InvalidParameterValue",
        "case {case:?}: expected the modelled error code InvalidParameterValue, got {:?} ({})",
        reply.error_code(),
        reply.body
    );
}

/// Assert that `reply` is that refusal carrying exactly `sentence`.
fn assert_out_of_range_refusal(case: &str, reply: &Reply, sentence: &str) {
    assert_client_refusal(case, reply);
    assert_eq!(
        reply.error_message(),
        sentence,
        "case {case:?}: expected the sentence {sentence:?}, got {:?}",
        reply.error_message()
    );
}

/// Assert that `reply` is that refusal, worded as the limit-value sentence for
/// `member`: it names that member's limit and closes with how to remove it. See
/// [`sentence_frame`] for why a row about `ECPUPerSecond` asserts the frame rather
/// than the numbers between its two halves.
fn assert_refusal_names_member(case: &str, reply: &Reply, member: &str) {
    assert_client_refusal(case, reply);
    let (opening, closing) = sentence_frame(member);
    let message = reply.error_message();
    assert!(
        message.starts_with(&opening) && message.ends_with(closing),
        "case {case:?}: expected a sentence opening {opening:?} and closing {closing:?}, got \
         {message:?}"
    );
}

/// Assert that `reply` is the not-found fault a cache this account does not hold is
/// owed, worded around the name that was asked for.
fn assert_not_found(case: &str, reply: &Reply, name: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: a cache this account does not hold must answer HTTP 404, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        "ServerlessCacheNotFoundFault",
        "case {case:?}: expected ServerlessCacheNotFoundFault, got {:?} ({})",
        reply.error_code(),
        reply.body
    );
    let expected = format!("Serverless cache with name {name} not found.");
    assert_eq!(
        reply.error_message(),
        expected,
        "case {case:?}: expected the sentence {expected:?}, got {:?}",
        reply.error_message()
    );
}

#[tokio::test]
async fn the_refusal_quotes_the_storage_bounds_the_api_publishes() {
    let fixture = Fixture::stand_up().await;
    // The whole sentence, against the text itself rather than against the constants
    // the rest of this suite builds its sentences from: AWS publishes 1 GB through
    // 5000 GB for `CacheUsageLimits.DataStorage`, so a bound outside that range is
    // answered in exactly these words. Every other storage row below is worded from
    // `state::DATA_STORAGE_MIN_GB` / `state::DATA_STORAGE_MAX_GB`, and this is what
    // holds those two to the published numbers and the template to this wording.
    let refused = fixture
        .create("published-bounds-cache", &storage(None, Some(-5)))
        .await;
    assert_out_of_range_refusal(
        "a storage bound outside the published range",
        &refused,
        "Invalid value for DataStorage limit. Must be between 1 and 5000. You can set the value \
         to zero to remove the limit.",
    );
    assert_eq!(
        bounded_sentence(
            "DataStorage",
            state::DATA_STORAGE_MIN_GB,
            state::DATA_STORAGE_MAX_GB
        ),
        refused.error_message(),
        "the rows below must be worded from the bounds the service publishes"
    );
}

#[tokio::test]
async fn a_create_refuses_every_out_of_range_storage_bound_and_stores_nothing() {
    let fixture = Fixture::stand_up().await;

    // The axis: which bound, which side of the range, and how far out — one past the
    // published edge, far past it, the extremes the member's `IntegerOptional` can
    // carry, and both bounds out at once.
    let rows: Vec<RefusalRow> = vec![
        (
            "the recorded negative storage maximum",
            storage(None, Some(-5)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            // The first value below the range that is not the zero REMOVING the
            // limit, which is accepted instead of refused.
            "a storage minimum one below the zero that removes a limit",
            storage(Some(state::USAGE_LIMIT_REMOVED - 1), None),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            "a storage maximum one above the published ceiling",
            storage(None, Some(state::DATA_STORAGE_MAX_GB + 1)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            "a storage minimum far above the published ceiling",
            storage(Some(state::DATA_STORAGE_MAX_GB * 20), None),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            "the smallest value the storage member's type can carry",
            storage(None, Some(i32::MIN)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            "the largest value the storage member's type can carry",
            storage(None, Some(i32::MAX)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            // Both bounds are out of range, and the envelope is not contradictory:
            // the range rule is judged per bound, so it still answers the range
            // sentence.
            "both storage bounds outside the range",
            storage(Some(-3), Some(state::DATA_STORAGE_MAX_GB + 4000)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
    ];

    for (case, limits, sentence) in &rows {
        let name = "out-of-range-cache";
        let refused = fixture.create(name, limits).await;
        assert_out_of_range_refusal(case, &refused, sentence);

        // And the refusal came before anything was written — the value was not
        // quietly clamped into range and stored: the account holds no cache under
        // that name.
        let read_back = fixture.describe(name).await;
        assert_not_found(case, &read_back, name);
    }
}

#[tokio::test]
async fn a_create_refuses_a_bound_below_the_ecpu_floor_and_stores_nothing() {
    let fixture = Fixture::stand_up().await;

    // The other member the rule holds for. These rows sit below ANY floor the API
    // could publish for a per-second ECPU bound — a negative number of ECPUs is not a
    // capacity at all — so each is asserted against the member the sentence has to
    // name and the fixed frame it has to carry, which is what the rule fixes for this
    // member (see [`sentence_frame`]).
    let rows: Vec<(&str, Pairs)> = vec![
        ("a negative ECPU minimum", ecpu(Some(-1), None)),
        (
            "a negative ECPU maximum beside a storage bound inside the range",
            both(storage(None, Some(10)), ecpu(None, Some(-5000))),
        ),
        (
            "an ECPU minimum at the smallest value its type can carry",
            ecpu(Some(i32::MIN), None),
        ),
    ];

    for (case, limits) in &rows {
        let name = "below-ecpu-floor-cache";
        let refused = fixture.create(name, limits).await;
        assert_refusal_names_member(case, &refused, "ECPUPerSecond");

        let read_back = fixture.describe(name).await;
        assert_not_found(case, &read_back, name);
    }
}

#[tokio::test]
async fn a_create_accepts_every_bound_inside_the_range_and_reports_it_back() {
    let fixture = Fixture::stand_up().await;

    // The axis: each published edge of the storage range, the recorded in-range
    // control, the zero that removes a limit, and the floor the ECPU member is
    // bounded at.
    let rows: Vec<AcceptedRow> = vec![
        (
            "the recorded in-range storage maximum",
            storage(None, Some(10)),
            vec![("DataStorage/Maximum", "10".to_string())],
        ),
        (
            "a storage limit at the published floor",
            storage(Some(state::DATA_STORAGE_MIN_GB), None),
            vec![(
                "DataStorage/Minimum",
                state::DATA_STORAGE_MIN_GB.to_string(),
            )],
        ),
        (
            "a storage limit at the published ceiling",
            storage(None, Some(state::DATA_STORAGE_MAX_GB)),
            vec![(
                "DataStorage/Maximum",
                state::DATA_STORAGE_MAX_GB.to_string(),
            )],
        ),
        (
            "the zero that removes a storage limit rather than setting one",
            storage(None, Some(state::USAGE_LIMIT_REMOVED)),
            vec![(
                "DataStorage/Maximum",
                state::USAGE_LIMIT_REMOVED.to_string(),
            )],
        ),
        (
            // The ECPU range AWS publishes for a serverless cache: 1,000 through
            // 15,000,000 ECPUs per second.
            "an ECPU envelope inside the range published for a serverless cache",
            ecpu(Some(1_000), Some(15_000_000)),
            vec![
                ("ECPUPerSecond/Minimum", "1000".to_string()),
                ("ECPUPerSecond/Maximum", "15000000".to_string()),
            ],
        ),
    ];

    for (index, (case, limits, reported)) in rows.iter().enumerate() {
        let name = format!("in-range-cache-{index}");
        let created = fixture.create(&name, limits).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: a bound inside the range must be accepted with HTTP 200, got {} {}",
            created.status, created.body
        );
        assert_eq!(
            created.text("Status"),
            Some(state::SERVERLESS_STATUS_CREATING),
            "case {case:?}: a created cache is not ready yet, got {:?}",
            created.text("Status")
        );

        // The round trip: the cache reaches the ready status the clock owes it, and
        // reports the bound it was created with, unchanged.
        let settled = fixture
            .settled(case, &name, state::SERVERLESS_STATUS_AVAILABLE)
            .await;
        for (member, expected) in reported {
            assert_eq!(
                settled.text(member),
                Some(expected.as_str()),
                "case {case:?}: expected {member} {expected:?} after the round trip, got {:?} ({})",
                settled.text(member),
                settled.body
            );
        }
        assert_eq!(
            settled.all("SubnetId"),
            fixture
                .subnets
                .iter()
                .map(String::as_str)
                .collect::<Vec<&str>>(),
            "case {case:?}: expected the cache to report the subnets it was placed in, got {:?}",
            settled.all("SubnetId")
        );
    }
}

#[tokio::test]
async fn a_refused_modify_leaves_the_live_cache_running_the_limits_it_had() {
    let fixture = Fixture::stand_up().await;
    let baseline = both(storage(Some(1), Some(10)), ecpu(Some(1000), Some(5000)));
    let created = fixture.create("ranged-cache", &baseline).await;
    assert_eq!(
        created.status, 200,
        "the baseline create must answer HTTP 200, got {} {}",
        created.status, created.body
    );
    fixture
        .settled(
            "the baseline",
            "ranged-cache",
            state::SERVERLESS_STATUS_AVAILABLE,
        )
        .await;

    /// What the baseline cache runs, which every refused modify must leave in place.
    const BASELINE: [(&str, &str); 5] = [
        ("DataStorage/Unit", STORAGE_UNIT),
        ("DataStorage/Minimum", "1"),
        ("DataStorage/Maximum", "10"),
        ("ECPUPerSecond/Minimum", "1000"),
        ("ECPUPerSecond/Maximum", "5000"),
    ];

    // The same axis at the other operation that would store an envelope: a bound
    // outside the range is refused whether the request carries one bound or both.
    let rows: Vec<RefusalRow> = vec![
        (
            "a storage maximum above the published ceiling",
            storage(None, Some(state::DATA_STORAGE_MAX_GB + 1)),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
        (
            "a negative storage minimum",
            storage(Some(-2), None),
            bounded_sentence(
                "DataStorage",
                state::DATA_STORAGE_MIN_GB,
                state::DATA_STORAGE_MAX_GB,
            ),
        ),
    ];

    for (case, limits, sentence) in &rows {
        let refused = fixture.modify("ranged-cache", limits).await;
        assert_out_of_range_refusal(case, &refused, sentence);

        let untouched = fixture.describe("ranged-cache").await;
        for (member, expected) in BASELINE {
            assert_eq!(
                untouched.text(member),
                Some(expected),
                "case {case:?}: a refused modify must leave {member} at {expected:?}, got {:?} ({})",
                untouched.text(member),
                untouched.body
            );
        }
        assert_eq!(
            untouched.text("Status"),
            Some(state::SERVERLESS_STATUS_AVAILABLE),
            "case {case:?}: a refused modify must leave the cache available, got {:?}",
            untouched.text("Status")
        );
    }

    // The other member, at the same operation: a bound below any floor a per-second
    // ECPU limit could publish is the member's own limit-value refusal, and the live
    // cache keeps the envelope it is running.
    let below_floor = fixture.modify("ranged-cache", &ecpu(None, Some(-1))).await;
    assert_refusal_names_member("a negative ECPU maximum", &below_floor, "ECPUPerSecond");
    let untouched = fixture.describe("ranged-cache").await;
    for (member, expected) in BASELINE {
        assert_eq!(
            untouched.text(member),
            Some(expected),
            "a refused modify must leave {member} at {expected:?}, got {:?} ({})",
            untouched.text(member),
            untouched.body
        );
    }

    // And the operation still works: a bound inside the range is applied and
    // reported, so the refusals above are about the range and not about the member.
    let widened = fixture
        .modify(
            "ranged-cache",
            &storage(None, Some(state::DATA_STORAGE_MAX_GB)),
        )
        .await;
    assert_eq!(
        widened.status, 200,
        "a bound at the published ceiling must answer HTTP 200, got {} {}",
        widened.status, widened.body
    );
    let settled = fixture
        .settled(
            "the widening",
            "ranged-cache",
            state::SERVERLESS_STATUS_AVAILABLE,
        )
        .await;
    assert_eq!(
        settled.text("DataStorage/Maximum"),
        Some(state::DATA_STORAGE_MAX_GB.to_string().as_str()),
        "the widened cache must report the ceiling it was moved to, got {:?} ({})",
        settled.text("DataStorage/Maximum"),
        settled.body
    );
}
