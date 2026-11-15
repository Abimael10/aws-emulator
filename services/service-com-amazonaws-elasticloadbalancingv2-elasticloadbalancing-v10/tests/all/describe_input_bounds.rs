// SPDX-License-Identifier: CC0-1.0

//! What the ELBv2 `Describe*` reads refuse on the REQUEST's own shape, before they look
//! anything up — over the wire, through the router.
//!
//! The rules these tables pin:
//!
//! * `DescribeLoadBalancers` describes at most 20 load balancers per call
//!   (`LoadBalancerArns`' own documentation: "You can specify up to 20 load balancers in
//!   a single call" — `smithy_query(query="doc",shape="DescribeLoadBalancersInput",
//!   member="LoadBalancerArns")`; the model declares no length constraint for either
//!   list, so the bound is the service's own). Past it the answer is HTTP 400
//!   `ValidationError` naming the limit and the count the request carried. Its two
//!   selectors — `LoadBalancerArns` and `Names` — state two different selections, so a
//!   call carries at most one of them, and a call carrying both is HTTP 400
//!   `ValidationError` on the pair.
//! * `DescribeTargetGroups` describes at most 20 target groups per call, and its three
//!   filters — `Names`, `TargetGroupArns` and `LoadBalancerArn` — select in three
//!   different ways, so a call carries at most one of them. Either way the answer is
//!   HTTP 400 `ValidationError`.
//! * Both are judged BEFORE any lookup, which is what the rows with identifiers that all
//!   exist pin: a request outside the shape the API accepts is never answered as a
//!   missing resource, and never with a report. An identifier nothing holds INSIDE the
//!   bound still answers the modelled not-found, which is what the rows carrying one
//!   absent identifier pin.
//! * A `Marker` is an identifier this service MINTS, so a value it did not mint names no
//!   position in any collection and is refused on the request: HTTP 400 `ValidationError`
//!   quoting the marker. A marker is bound to the collection it was issued for.
//! * Pagination walks a whole collection rather than a selection out of it, so
//!   `DescribeLoadBalancers` refuses a call that both names the load balancers to report
//!   and asks for a page of them.
//! * A read that selects by NAME judges the name on its own form first: a value outside
//!   the clauses the create enforces names no resource this API could have minted, so it
//!   is HTTP 400 `ValidationError` stating the clause, while a well-formed name nothing
//!   holds stays the modelled not-found.
//!
//! Every row differs along what those rules generalise over: which member carries the
//! identifiers, how many it carries on each side of the bound, whether the identifiers
//! resolve, and which filters are combined. The wire harness (`emulator`, `call`,
//! `action`, `p`, `members`) is `all/provisioning_loop.rs`'s; nothing here restates it.

use crate::provisioning_loop::{account, action, call, emulator, members, p, REGION};
use crate::target_group_attributes_and_modification::unescaped;
use smithy_server_runtime::MultiServiceRouter;

/// The awsQuery error code this service answers a request whose values it cannot accept
/// with, for a rule its model declares no error shape for.
const VALIDATION_ERROR: &str = "ValidationError";

/// The `awsQueryError` code of the modelled `LoadBalancerNotFoundException`.
const LOAD_BALANCER_NOT_FOUND: &str = "LoadBalancerNotFound";

/// The `awsQueryError` code of the modelled `TargetGroupNotFoundException`.
const TARGET_GROUP_NOT_FOUND: &str = "TargetGroupNotFound";

/// How many resources one `Describe` call may ask about, as the handlers bound it.
const PER_CALL: usize = 20;

/// How many load balancers and target groups each table below creates: one more than the
/// bound, so a row one past the bound can be built entirely out of identifiers that
/// EXIST — which is what makes the refusal a statement about the request.
const POOL: usize = PER_CALL + 1;

/// What one row expects back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The request is inside the shape the API accepts, and reports this many resources.
    Reports(usize),
    /// The request is outside the per-call bound; the refusal carries this count when the
    /// service's wording quotes one.
    PastBound(usize),
    /// The request carries two selectors at once, which state two selections: refused on
    /// the pair with this message, whatever the identifiers in them are.
    Conflicting(&'static str),
    /// The request is well shaped, and one identifier in it resolves to nothing.
    NotFound,
}

/// The refusal `DescribeLoadBalancers` answers a call that selects by both of its members
/// with.
const LOAD_BALANCER_SELECTOR_CONFLICT: &str =
    "Load balancer names and load balancer ARNs cannot be specified at the same time";

/// The refusal `DescribeLoadBalancers` answers a call that both names the load balancers
/// to report and asks for a page of them.
const PAGINATION_CONFLICT: &str = "Pagination is not supported when specifying load balancers";

/// One row: the case, the indices the request names by ARN, the indices it names by name,
/// and what must come back.
type ListCase = (&'static str, Vec<usize>, Vec<usize>, Expect);

/// One row of the filter table: the case, the request's filter fields, and the awsQuery
/// error code the answer must carry (empty for a row the service serves).
type FilterCase = (&'static str, Vec<(String, String)>, &'static str);

/// Create a load balancer with two subnets and answer its ARN.
///
/// `all/provisioning_loop.rs`'s creator is private to that suite; this one exists because
/// the tables below need a POOL of load balancers rather than one, and it asserts only
/// what it needs: that the create was served and minted an ARN.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) -> String {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case create {name}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case create {name}: no LoadBalancerArn in {}", reply.body))
}

/// A load balancer ARN of the right shape that no load balancer in the store carries.
fn absent_load_balancer_arn(index: usize) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/app/absent-lb-{index}/0123456789abcdef",
        account()
    )
}

/// A target group ARN of the right shape that no target group in the store carries.
fn absent_target_group_arn(index: usize) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:targetgroup/absent-tg-{index}/0123456789abcdef",
        account()
    )
}

/// The ARN at `index` of a created pool, or an ARN nothing holds for an index past it.
fn arn_at(pool: &[String], index: usize, absent: fn(usize) -> String) -> String {
    pool.get(index).cloned().unwrap_or_else(|| absent(index))
}

/// The name at `index` of a created pool, or a name nothing holds for an index past it.
fn name_at(names: &[String], index: usize, absent_prefix: &str) -> String {
    names
        .get(index)
        .cloned()
        .unwrap_or_else(|| format!("{absent_prefix}-{index}"))
}

/// The whole index range `0..count`, for a row that names a run of the pool.
fn ix(count: usize) -> Vec<usize> {
    (0..count).collect()
}

/// Assert a row that carries two selectors at once was refused on the PAIR: the
/// `ValidationError` code and the service's wording for that rule.
fn assert_conflict(case: &str, reply: &crate::provisioning_loop::Reply, conflict: &str) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, VALIDATION_ERROR),
        "case {case:?}: expected 400 {VALIDATION_ERROR}, got {} body {}",
        reply.status,
        reply.body
    );
    let message = reply.text("Message").map(|text| unescaped(&text));
    assert_eq!(
        message.as_deref(),
        Some(conflict),
        "case {case:?}: expected the message {conflict:?}, got {message:?}"
    );
}

/// The indices `from..to`, for a row that names a slice of the pool.
fn span(from: usize, to: usize) -> Vec<usize> {
    (from..to).collect()
}

#[tokio::test]
async fn describe_load_balancers_bounds_the_load_balancers_one_call_asks_about() {
    let (router, _engine) = emulator();

    // A pool of 21 load balancers, so a request one past the bound can name only load
    // balancers that exist.
    let mut arns = Vec::new();
    let mut names = Vec::new();
    for index in 0..POOL {
        let name = format!("bound-lb-{index:02}");
        arns.push(create_load_balancer(&router, &name).await);
        names.push(name);
    }

    // (case, ARN indices, name indices, expectation)
    //
    // The rows walk the bound from either side, on each selector alone and on the two
    // together, with identifiers that exist and identifiers that do not: what decides the
    // refusal is how many load balancers the call asks about, not whether they are there.
    let rows: Vec<ListCase> = vec![
        ("one name", vec![], ix(1), Expect::Reports(1)),
        ("one ARN", ix(1), vec![], Expect::Reports(1)),
        (
            "twenty names, the published bound",
            vec![],
            ix(PER_CALL),
            Expect::Reports(PER_CALL),
        ),
        (
            "twenty ARNs, the published bound",
            ix(PER_CALL),
            vec![],
            Expect::Reports(PER_CALL),
        ),
        (
            "ten ARNs and ten names together, inside the bound",
            ix(10),
            span(10, PER_CALL),
            Expect::Conflicting(LOAD_BALANCER_SELECTOR_CONFLICT),
        ),
        (
            "eleven ARNs and ten names together, one past the bound",
            ix(11),
            span(11, POOL),
            Expect::Conflicting(LOAD_BALANCER_SELECTOR_CONFLICT),
        ),
        (
            "twenty-one names that all exist, one past the bound",
            vec![],
            ix(POOL),
            Expect::PastBound(POOL),
        ),
        (
            "twenty-one ARNs that all exist, one past the bound",
            ix(POOL),
            vec![],
            Expect::PastBound(POOL),
        ),
        (
            "fifty names, most of which nothing holds",
            vec![],
            ix(50),
            Expect::PastBound(50),
        ),
        (
            "nineteen names that exist and one that does not, inside the bound",
            vec![],
            span(0, 19).into_iter().chain([POOL + 7]).collect(),
            Expect::NotFound,
        ),
        (
            "one ARN nothing holds, inside the bound",
            vec![POOL + 3],
            vec![],
            Expect::NotFound,
        ),
        ("no selector at all", vec![], vec![], Expect::Reports(POOL)),
    ];

    for (case, arn_indices, name_indices, expected) in rows {
        let mut form = action("DescribeLoadBalancers");
        for (position, index) in arn_indices.iter().enumerate() {
            form.push(p(
                &format!("LoadBalancerArns.member.{}", position + 1),
                &arn_at(&arns, *index, absent_load_balancer_arn),
            ));
        }
        for (position, index) in name_indices.iter().enumerate() {
            form.push(p(
                &format!("Names.member.{}", position + 1),
                &name_at(&names, *index, "absent-lb"),
            ));
        }
        // Pagination walks the whole collection rather than a selection out of it, so a
        // row that NAMES the load balancers it asks about must not also ask for a page of
        // them (the pagination table below). `PageSize` therefore goes only on the row
        // that selects nothing, where it is what makes the whole collection one page; a
        // row that selects reports its whole selection anyway.
        if arn_indices.is_empty() && name_indices.is_empty() {
            form.push(p("PageSize", "400"));
        }
        let reply = call(&router, &form).await;

        match expected {
            Expect::Reports(count) => {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: expected 200, got {} body {}",
                    reply.status, reply.body
                );
                let reported = members(&reply.body, "LoadBalancers").len();
                assert_eq!(
                    reported, count,
                    "case {case:?}: expected {count} load balancers reported, got {reported} body {}",
                    reply.body
                );
            }
            Expect::PastBound(asked_about) => {
                let expected_message = format!(
                    "Up to {PER_CALL} load balancers can be described at a time, but {asked_about} were specified"
                );
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, VALIDATION_ERROR),
                    "case {case:?}: expected 400 {VALIDATION_ERROR}, got {} body {}",
                    reply.status,
                    reply.body
                );
                let message = reply.text("Message").map(|text| unescaped(&text));
                assert_eq!(
                    message.as_deref(),
                    Some(expected_message.as_str()),
                    "case {case:?}: expected the message {expected_message:?}, got {message:?}"
                );
            }
            Expect::NotFound => {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, LOAD_BALANCER_NOT_FOUND),
                    "case {case:?}: expected 400 {LOAD_BALANCER_NOT_FOUND}, got {} body {}",
                    reply.status,
                    reply.body
                );
            }
            Expect::Conflicting(conflict) => assert_conflict(case, &reply, conflict),
        }
    }
}

#[tokio::test]
async fn describe_target_groups_bounds_the_target_groups_one_call_asks_about() {
    let (router, _engine) = emulator();

    // The same pool, of target groups: 21 of them, so the row one past the bound names
    // only target groups that exist.
    let mut arns = Vec::new();
    let mut names = Vec::new();
    for index in 0..POOL {
        let name = format!("bound-tg-{index:02}");
        arns.push(crate::provisioning_loop::create_http_target_group(&router, &name).await);
        names.push(name);
    }

    // (case, ARN indices, name indices, expectation)
    //
    // Each row names ONE of the two selectors, because the filters are mutually exclusive
    // (the next test); the bound is walked from either side on each of them.
    let rows: Vec<ListCase> = vec![
        ("one ARN", ix(1), vec![], Expect::Reports(1)),
        ("one name", vec![], ix(1), Expect::Reports(1)),
        (
            "twenty ARNs, the bound",
            ix(PER_CALL),
            vec![],
            Expect::Reports(PER_CALL),
        ),
        (
            "twenty names, the bound",
            vec![],
            ix(PER_CALL),
            Expect::Reports(PER_CALL),
        ),
        (
            "twenty-one ARNs that all exist, one past the bound",
            ix(POOL),
            vec![],
            Expect::PastBound(POOL),
        ),
        (
            "twenty-one names that all exist, one past the bound",
            vec![],
            ix(POOL),
            Expect::PastBound(POOL),
        ),
        (
            "fifty ARNs, most of which nothing holds",
            ix(50),
            vec![],
            Expect::PastBound(50),
        ),
        (
            "nineteen ARNs that exist and one that does not, inside the bound",
            span(0, 19).into_iter().chain([POOL + 7]).collect(),
            vec![],
            Expect::NotFound,
        ),
        (
            "one name nothing holds, inside the bound",
            vec![],
            vec![POOL + 3],
            Expect::NotFound,
        ),
        ("no filter at all", vec![], vec![], Expect::Reports(POOL)),
    ];

    for (case, arn_indices, name_indices, expected) in rows {
        let mut form = action("DescribeTargetGroups");
        for (position, index) in arn_indices.iter().enumerate() {
            form.push(p(
                &format!("TargetGroupArns.member.{}", position + 1),
                &arn_at(&arns, *index, absent_target_group_arn),
            ));
        }
        for (position, index) in name_indices.iter().enumerate() {
            form.push(p(
                &format!("Names.member.{}", position + 1),
                &name_at(&names, *index, "absent-tg"),
            ));
        }
        form.push(p("PageSize", "400"));
        let reply = call(&router, &form).await;

        match expected {
            Expect::Reports(count) => {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: expected 200, got {} body {}",
                    reply.status, reply.body
                );
                let reported = members(&reply.body, "TargetGroups").len();
                assert_eq!(
                    reported, count,
                    "case {case:?}: expected {count} target groups reported, got {reported} body {}",
                    reply.body
                );
            }
            // The service's wording for this bound quotes the limit and no count, so the
            // row's count decides only which side of the bound it is on.
            Expect::PastBound(_) => {
                let expected_message =
                    format!("You cannot describe more than '{PER_CALL}' target groups at a time");
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, VALIDATION_ERROR),
                    "case {case:?}: expected 400 {VALIDATION_ERROR}, got {} body {}",
                    reply.status,
                    reply.body
                );
                let message = reply.text("Message").map(|text| unescaped(&text));
                assert_eq!(
                    message.as_deref(),
                    Some(expected_message.as_str()),
                    "case {case:?}: expected the message {expected_message:?}, got {message:?}"
                );
            }
            Expect::NotFound => {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, TARGET_GROUP_NOT_FOUND),
                    "case {case:?}: expected 400 {TARGET_GROUP_NOT_FOUND}, got {} body {}",
                    reply.status,
                    reply.body
                );
            }
            Expect::Conflicting(conflict) => assert_conflict(case, &reply, conflict),
        }
    }
}

#[tokio::test]
async fn a_marker_no_read_issued_is_refused_as_an_invalid_marker_id() {
    let (router, _engine) = emulator();

    // Something to page through, so a read that is served has a position to resume from.
    for index in 0..3 {
        create_load_balancer(&router, &format!("marker-lb-{index}")).await;
        crate::provisioning_loop::create_http_target_group(&router, &format!("marker-tg-{index}"))
            .await;
    }

    // A `Marker` is an identifier this service MINTS: a value it did not mint names no
    // position in any collection, so it is refused on the request — HTTP 400
    // `ValidationError` quoting the marker — rather than answered with a page or an empty
    // report. The rows differ along the FORM of the value: words, the ordinals a caller
    // might compute for itself, a negative and a fractional one, and one far past the end.
    let markers: [(&str, String); 7] = [
        ("a word", "not-a-marker".to_string()),
        ("an ordinal", "1".to_string()),
        ("the first ordinal", "0".to_string()),
        ("a negative ordinal", "-1".to_string()),
        ("a fractional ordinal", "1.5".to_string()),
        ("an ordinal past the end", "9999".to_string()),
        ("an empty marker", String::new()),
    ];
    // (the read, the fields it needs beside the marker)
    let reads: [(&str, Vec<(String, String)>); 5] = [
        ("DescribeLoadBalancers", vec![]),
        ("DescribeTargetGroups", vec![]),
        ("DescribeSSLPolicies", vec![]),
        ("DescribeAccountLimits", vec![]),
        ("DescribeTrustStores", vec![]),
    ];

    for (read, fields) in &reads {
        for (form_case, marker) in &markers {
            let case = format!("{read} given {form_case}");
            let mut form = action(read);
            form.push(p("Marker", marker));
            for field in fields {
                form.push(field.clone());
            }
            let reply = call(&router, &form).await;
            let expected = format!("Invalid Marker Id: {marker}");
            let stated = reply.text("Message").map(|text| unescaped(&text));
            assert_eq!(
                (reply.status, reply.error_code(), stated.clone()),
                (400, VALIDATION_ERROR.to_string(), Some(expected.clone())),
                "case {case:?}: expected 400 {VALIDATION_ERROR} {expected:?}, got {} body {}",
                reply.status,
                reply.body
            );
        }
    }

    // A marker is bound to the COLLECTION it was issued for: the one the load balancers
    // page left names no position among the target groups, and is refused in the same
    // words — while the read that issued it resumes from it.
    let mut form = action("DescribeLoadBalancers");
    form.push(p("PageSize", "1"));
    let first = call(&router, &form).await;
    let issued = first
        .text("NextMarker")
        .unwrap_or_else(|| panic!("case paging the load balancers: no NextMarker in {}", first.body));

    let mut form = action("DescribeTargetGroups");
    form.push(p("Marker", &issued));
    let elsewhere = call(&router, &form).await;
    let expected = format!("Invalid Marker Id: {issued}");
    let stated = elsewhere.text("Message").map(|text| unescaped(&text));
    assert_eq!(
        (elsewhere.status, elsewhere.error_code(), stated.clone()),
        (400, VALIDATION_ERROR.to_string(), Some(expected.clone())),
        "case another collection's marker: expected 400 {VALIDATION_ERROR} {expected:?}, got {} \
         body {}",
        elsewhere.status,
        elsewhere.body
    );

    let mut form = action("DescribeLoadBalancers");
    form.push(p("Marker", &issued));
    form.push(p("PageSize", "1"));
    let resumed = call(&router, &form).await;
    assert_eq!(
        (resumed.status, members(&resumed.body, "LoadBalancers").len()),
        (200, 1),
        "case the marker this read issued: expected the next page, got {} body {}",
        resumed.status,
        resumed.body
    );
}

#[tokio::test]
async fn describe_load_balancers_refuses_pagination_together_with_a_selection() {
    let (router, _engine) = emulator();

    let mut arns = Vec::new();
    let mut names = Vec::new();
    for index in 0..3 {
        let name = format!("paging-lb-{index}");
        arns.push(create_load_balancer(&router, &name).await);
        names.push(name);
    }

    // (case, the fields beside the pagination members, whether the row carries a
    // selection)
    //
    // The rows cross the two selectors — by ARN and by name, with identifiers that exist
    // and identifiers that do not — with each pagination member on its own and the two
    // together: what decides the refusal is that the request both names load balancers and
    // asks for a page, not which member it named them in or whether they are there.
    let selections: Vec<(&str, Vec<(String, String)>)> = vec![
        (
            "no selection",
            vec![],
        ),
        (
            "one ARN that exists",
            vec![p("LoadBalancerArns.member.1", &arns[0])],
        ),
        (
            "two ARNs that exist",
            vec![
                p("LoadBalancerArns.member.1", &arns[0]),
                p("LoadBalancerArns.member.2", &arns[1]),
            ],
        ),
        (
            "one ARN nothing holds",
            vec![p("LoadBalancerArns.member.1", &absent_load_balancer_arn(9))],
        ),
        (
            "one name that exists",
            vec![p("Names.member.1", &names[0])],
        ),
        ("one name nothing holds", vec![p("Names.member.1", "absent-lb-9")]),
    ];
    let pagination: Vec<(&str, Vec<(String, String)>)> = vec![
        ("no pagination", vec![]),
        ("a page size", vec![p("PageSize", "1")]),
        ("a marker", vec![p("Marker", "any-marker")]),
        (
            "a page size and a marker",
            vec![p("PageSize", "1"), p("Marker", "any-marker")],
        ),
    ];

    for (selection_case, selection) in &selections {
        for (pagination_case, paging) in &pagination {
            let case = format!("{selection_case} with {pagination_case}");
            let mut form = action("DescribeLoadBalancers");
            for field in selection.iter().chain(paging.iter()) {
                form.push(field.clone());
            }
            let reply = call(&router, &form).await;
            if selection.is_empty() || paging.is_empty() {
                // One of the two alone is a request this API accepts — which is what makes
                // the refusal above about the COMBINATION. A marker nothing issued is its
                // own refusal, so the rows that carry one are not asserted to be served
                // here (`a_marker_no_read_issued_is_refused_as_an_invalid_marker_id`).
                assert_ne!(
                    reply.text("Message").map(|text| unescaped(&text)).as_deref(),
                    Some(PAGINATION_CONFLICT),
                    "case {case:?}: one of the two alone must not earn the combination's \
                     refusal, got {}",
                    reply.body
                );
                continue;
            }
            assert_eq!(
                (
                    reply.status,
                    reply.error_code(),
                    reply.text("Message").map(|text| unescaped(&text))
                ),
                (
                    400,
                    VALIDATION_ERROR.to_string(),
                    Some(PAGINATION_CONFLICT.to_string())
                ),
                "case {case:?}: expected 400 {VALIDATION_ERROR} {PAGINATION_CONFLICT:?}, got {} \
                 body {}",
                reply.status,
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn a_name_no_resource_could_carry_is_refused_on_the_request() {
    let (router, _engine) = emulator();

    // A name outside the documented form names no resource this API could ever have
    // minted, so a read that SELECTS by name refuses it on the request — HTTP 400
    // `ValidationError` stating the clause the value broke — rather than reporting it as a
    // name nothing holds. The clauses are the ones the creates enforce, and the rows walk
    // them: the length, the character set, a leading and a trailing hyphen, the prefix a
    // load balancer name may not begin with, and the DNS name a caller reaches for when it
    // has no ARN.
    //
    // (case, the name, the clause the refusal must state — `None` for a name that is well
    // formed and simply absent)
    let thirty_three = "a".repeat(33);
    let names: Vec<(&str, String, Option<&str>)> = vec![
        ("a well-formed name nothing holds", "absent-name".to_string(), None),
        (
            "a name at the published maximum nothing holds",
            "a".repeat(32),
            None,
        ),
        (
            "a name one past the published maximum",
            thirty_three.clone(),
            Some("cannot be longer than '32' characters"),
        ),
        (
            "the published DNS name rather than the name",
            format!("probe-lb-0123456789abcdef.{REGION}.elb.amazonaws.com"),
            Some("cannot be longer than '32' characters"),
        ),
        (
            "a name carrying an underscore",
            "probe_lb".to_string(),
            Some("can only contain characters that are alphanumeric characters or hyphens"),
        ),
        (
            "a name carrying a dot",
            "probe.lb".to_string(),
            Some("can only contain characters that are alphanumeric characters or hyphens"),
        ),
        (
            "a name beginning with a hyphen",
            "-probe-lb".to_string(),
            Some("cannot begin or end with a hyphen"),
        ),
        (
            "a name ending with a hyphen",
            "probe-lb-".to_string(),
            Some("cannot begin or end with a hyphen"),
        ),
    ];

    // (the read, the member its names travel in, the subject its clauses name, the
    // modelled code a well-formed absent name earns)
    let reads: [(&str, &str, &str, &str); 2] = [
        (
            "DescribeLoadBalancers",
            "Names.member.1",
            "The load balancer name",
            LOAD_BALANCER_NOT_FOUND,
        ),
        (
            "DescribeTargetGroups",
            "Names.member.1",
            "The target group name",
            TARGET_GROUP_NOT_FOUND,
        ),
    ];

    for (read, member, subject, not_found) in &reads {
        for (name_case, name, clause) in &names {
            let case = format!("{read} given {name_case}");
            let mut form = action(read);
            form.push(p(member, name));
            let reply = call(&router, &form).await;
            let Some(clause) = clause else {
                // A name this API could have minted, that nothing holds: the modelled
                // not-found, which is what makes the refusals above about the REQUEST.
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, *not_found),
                    "case {case:?}: expected 400 {not_found}, got {} body {}",
                    reply.status,
                    reply.body
                );
                continue;
            };
            let expected = format!("{subject} '{name}' {clause}");
            let stated = reply.text("Message").map(|text| unescaped(&text));
            assert_eq!(
                (reply.status, reply.error_code(), stated.clone()),
                (400, VALIDATION_ERROR.to_string(), Some(expected.clone())),
                "case {case:?}: expected 400 {VALIDATION_ERROR} {expected:?}, got {} body {}",
                reply.status,
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn describe_target_groups_refuses_more_than_one_filter() {
    let (router, _engine) = emulator();

    let load_balancer = create_load_balancer(&router, "filter-lb").await;
    let group = crate::provisioning_loop::create_http_target_group(&router, "filter-tg").await;
    let absent_load_balancer = absent_load_balancer_arn(1);

    // (case, the filter fields, the expected awsQuery code — empty for a row the service
    // serves)
    //
    // The rows walk every combination of the three filters: each one alone, each pair, and
    // all three. A pair is refused as surely as the three together, and the refusal is
    // about the request's shape, so it is answered even when a filter in it names a load
    // balancer nothing holds.
    let rows: Vec<FilterCase> = vec![
        (
            "target group ARNs alone",
            vec![p("TargetGroupArns.member.1", &group)],
            "",
        ),
        ("names alone", vec![p("Names.member.1", "filter-tg")], ""),
        (
            "a load balancer ARN alone",
            vec![p("LoadBalancerArn", &load_balancer)],
            "",
        ),
        (
            "names and target group ARNs",
            vec![
                p("Names.member.1", "filter-tg"),
                p("TargetGroupArns.member.1", &group),
            ],
            VALIDATION_ERROR,
        ),
        (
            "names and a load balancer ARN",
            vec![
                p("Names.member.1", "filter-tg"),
                p("LoadBalancerArn", &load_balancer),
            ],
            VALIDATION_ERROR,
        ),
        (
            "target group ARNs and a load balancer ARN",
            vec![
                p("TargetGroupArns.member.1", &group),
                p("LoadBalancerArn", &load_balancer),
            ],
            VALIDATION_ERROR,
        ),
        (
            "all three filters",
            vec![
                p("Names.member.1", "filter-tg"),
                p("TargetGroupArns.member.1", &group),
                p("LoadBalancerArn", &load_balancer),
            ],
            VALIDATION_ERROR,
        ),
        (
            "all three filters, where the load balancer ARN names nothing",
            vec![
                p("Names.member.1", "filter-tg"),
                p("TargetGroupArns.member.1", &group),
                p("LoadBalancerArn", &absent_load_balancer),
            ],
            VALIDATION_ERROR,
        ),
        (
            "one filter naming a load balancer nothing holds",
            vec![p("LoadBalancerArn", &absent_load_balancer)],
            LOAD_BALANCER_NOT_FOUND,
        ),
    ];

    let conflict = "Target group names, target group ARNs, and a load balancer ARN cannot be \
                    specified at the same time";

    for (case, fields, code) in rows {
        let mut form = action("DescribeTargetGroups");
        for field in &fields {
            form.push(field.clone());
        }
        let reply = call(&router, &form).await;

        if code.is_empty() {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: one filter is accepted, so expected 200, got {} body {}",
                reply.status, reply.body
            );
            continue;
        }
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, code),
            "case {case:?}: expected 400 {code}, got {} body {}",
            reply.status,
            reply.body
        );
        if code == VALIDATION_ERROR {
            let message = reply.text("Message").map(|text| unescaped(&text));
            assert_eq!(
                message.as_deref(),
                Some(conflict),
                "case {case:?}: expected the message {conflict:?}, got {message:?}"
            );
        }
    }

    // A refused read must change nothing: every target group is still reported, and the
    // one filter that was refused in combination still works on its own.
    let mut form = action("DescribeTargetGroups");
    form.push(p("Names.member.1", "filter-tg"));
    let after = call(&router, &form).await;
    assert_eq!(
        (after.status, members(&after.body, "TargetGroups").len()),
        (200, 1),
        "case after the refusals: the target group must still be reported, got {} body {}",
        after.status,
        after.body
    );
}
