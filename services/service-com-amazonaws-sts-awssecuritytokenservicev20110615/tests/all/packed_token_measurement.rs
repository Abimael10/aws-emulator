// SPDX-License-Identifier: CC0-1.0

//! WHAT a mint's session state is measured at, in bytes.
//!
//! RULE: the size a mint reports for its session state — the figure `PackedPolicyTooLarge`
//! names, and the figure `PackedPolicySize` is the percentage of — is the length of the
//! PACKED token that state converts into: one binary record per packed element (the inline
//! session policy, each managed session policy ARN, each session tag, whose key and value
//! are each a field of its record) behind the token's own fixed header, carried as text at
//! four characters per three bytes. Nothing of the API's own document shape is part of it:
//! no member name, no quote and no escape of a serialized request enters the measurement,
//! and the bytes counted for an element are the bytes of the element itself.
//!
//! That matters twice over to a caller. The reported figure is what it subtracts from to
//! trim a session state that did not fit, and the same measurement is the accept/refuse
//! boundary and the percentage an accepted mint reports — so a measurement inflated by a
//! document's framing refuses session states AWS serves and misreports the room left in
//! every one it serves.
//!
//! Each row states the byte figure its session state is measured at as a LITERAL, so the
//! measurement is pinned to the rule and not to whatever the service happens to compute:
//! the framing is the token's 17-byte header, a 5-byte head per record and a 3-byte head
//! per field, and a session state of fifty tags whose keys are 124 characters and whose
//! values are 256 characters is measured at 26,092 bytes — the figure AWS reports for that
//! request.
//!
//! The rows differ along what the rule generalises over: WHICH of the three packed inputs
//! carries the bytes, how many elements the bytes are spread across, how long each element
//! is, whether the bytes are ASCII or multi-byte, and where the state falls relative to the
//! budget (well under it, exactly on it, one coding group past it, far past it), down to a
//! mint that passed NO session state at all — which still packs the token's own header, so
//! the figure it reports is that header's length and the percentage it reports is one.
//! The budget's two halves — the refusal and the percentage — are driven from the same
//! figure.

use super::sts_sessions::{call, element, emulator, member, seed_demo_role, LONG_TERM_KEY};

/// The budget a packed session token must fit in: the model's `minimumSessionTokenSizeType`
/// range maximum.
const SESSION_TOKEN_SIZE_LIMIT: usize = 4096;

/// The session state one row packs, and the size that state is measured at.
struct Row {
    case: &'static str,
    /// The inline session policy the row carries, if any.
    policy: Option<String>,
    /// The managed session policy ARNs the row names.
    policy_arns: Vec<String>,
    /// The session tags the row passes, as key/value pairs.
    tags: Vec<(String, String)>,
    /// The bytes the state above is measured at — stated, not derived.
    measured: usize,
}

/// A JSON session policy document of exactly `bytes` bytes.
fn policy_of(bytes: usize) -> String {
    let skeleton = r#"{"Version":"2012-10-17","Comment":""}"#;
    let document = format!(
        r#"{{"Version":"2012-10-17","Comment":"{}"}}"#,
        "x".repeat(bytes - skeleton.len())
    );
    assert_eq!(
        document.len(),
        bytes,
        "case policy_of({bytes}): built a document of {} bytes",
        document.len()
    );
    document
}

/// A managed session policy ARN of exactly `bytes` bytes.
fn arn_of(index: usize, bytes: usize) -> String {
    let prefix = format!("arn:aws:iam::aws:policy/Measured{index}");
    let arn = format!("{prefix}{}", "A".repeat(bytes - prefix.len()));
    assert_eq!(
        arn.len(),
        bytes,
        "case arn_of({index}, {bytes}): built an ARN of {} bytes",
        arn.len()
    );
    arn
}

/// `count` session tags whose keys are `key_bytes` bytes and whose values are `value_bytes`
/// bytes, each key unique (a duplicate key is its own refusal).
fn tags_of(count: usize, key_bytes: usize, value_bytes: usize) -> Vec<(String, String)> {
    (1..=count)
        .map(|index| {
            (
                format!("{index:0>width$}", width = key_bytes),
                format!("{index:0>width$}", width = value_bytes),
            )
        })
        .collect()
}

impl Row {
    /// A row carrying only session tags.
    fn of_tags(
        case: &'static str,
        count: usize,
        key_bytes: usize,
        value_bytes: usize,
        measured: usize,
    ) -> Self {
        Self {
            case,
            policy: None,
            policy_arns: Vec::new(),
            tags: tags_of(count, key_bytes, value_bytes),
            measured,
        }
    }

    /// The form parameters a request carrying this row's session state sends.
    fn params(&self) -> Vec<(String, String)> {
        let mut params = Vec::new();
        if let Some(policy) = &self.policy {
            params.push(("Policy".to_string(), policy.clone()));
        }
        for (index, arn) in self.policy_arns.iter().enumerate() {
            params.push((format!("PolicyArns.member.{}.arn", index + 1), arn.clone()));
        }
        for (index, (key, value)) in self.tags.iter().enumerate() {
            params.push((format!("Tags.member.{}.Key", index + 1), key.clone()));
            params.push((format!("Tags.member.{}.Value", index + 1), value.clone()));
        }
        params
    }
}

/// How AWS words the refusal of a session state past the budget.
fn too_large_message(measured: usize) -> String {
    format!(
        "Serialized token too large for session. Token size of {measured} bytes exceeds the \
         maximum allowed limit of {SESSION_TOKEN_SIZE_LIMIT} bytes."
    )
}

/// The percentage of the budget `measured` bytes take up, rounded up.
fn percent_of_budget(measured: usize) -> usize {
    measured.saturating_mul(100).div_ceil(SESSION_TOKEN_SIZE_LIMIT)
}

/// The rows: each session state, and the byte figure the rule measures it at.
///
/// Every figure is the stated framing applied to that state — the 17-byte token header,
/// plus 5 bytes and the element's own bytes per record, plus 3 bytes per field of a tag's
/// record, coded at four characters per three bytes.
fn rows() -> Vec<Row> {
    vec![
        // The recorded class: fifty tags of a 124-byte key and a 256-byte value.
        // 17 + 50 * (5 + 2 * 3 + 124 + 256) = 19_567 bytes of records, coded 26_092.
        Row::of_tags(
            "fifty session tags of 124-byte keys and 256-byte values",
            50,
            124,
            256,
            26_092,
        ),
        // One tag at the modelled maxima: 17 + 5 + 6 + 128 + 256 = 412, coded 552.
        Row::of_tags(
            "one session tag at the modelled key and value maxima",
            1,
            128,
            256,
            552,
        ),
        // 17 + (5 + 6 + 4 + 11) = 43, coded 60.
        Row {
            case: "one small session tag",
            policy: None,
            policy_arns: Vec::new(),
            tags: vec![("Team".to_string(), "engineering".to_string())],
            measured: 60,
        },
        // A value of ten two-byte characters is twenty bytes, not ten:
        // 17 + (5 + 6 + 1 + 20) = 49, coded 68.
        Row {
            case: "one session tag whose value is multi-byte",
            policy: None,
            policy_arns: Vec::new(),
            tags: vec![("k".to_string(), "é".repeat(10))],
            measured: 68,
        },
        // 17 + 10 * (5 + 6 + 16 + 32) = 607, coded 812.
        Row::of_tags(
            "ten session tags of 16-byte keys and 32-byte values",
            10,
            16,
            32,
            812,
        ),
        // Exactly on the budget: 17 + 13 * (5 + 6 + 28 + 196) = 3_072, coded 4_096.
        Row::of_tags("session tags exactly at the budget", 13, 28, 196, 4_096),
        // One coding group past it: 17 + 16 * (5 + 6 + 20 + 160) = 3_073, coded 4_100.
        Row::of_tags(
            "session tags one coding group over the budget",
            16,
            20,
            160,
            4_100,
        ),
        // The inline policy at its documented plaintext limit, whose quotes and braces are
        // bytes of the document and nothing more: 17 + 5 + 2_048 = 2_070, coded 2_760.
        Row {
            case: "an inline session policy at its plaintext limit",
            policy: Some(policy_of(2_048)),
            policy_arns: Vec::new(),
            tags: Vec::new(),
            measured: 2_760,
        },
        // 17 + (5 + 60) = 82, coded 112.
        Row {
            case: "one managed session policy ARN",
            policy: None,
            policy_arns: vec![arn_of(1, 60)],
            tags: Vec::new(),
            measured: 112,
        },
        // 17 + 3 * (5 + 40) = 152, coded 204.
        Row {
            case: "three managed session policy ARNs",
            policy: None,
            policy_arns: (1..=3).map(|index| arn_of(index, 40)).collect(),
            tags: Vec::new(),
            measured: 204,
        },
        // All three kinds at once:
        // 17 + (5 + 500) + 2 * (5 + 50) + 2 * (5 + 6 + 10 + 20) = 714, coded 952.
        Row {
            case: "a policy, two ARNs and two tags together",
            policy: Some(policy_of(500)),
            policy_arns: (1..=2).map(|index| arn_of(index, 50)).collect(),
            tags: tags_of(2, 10, 20),
            measured: 952,
        },
        // Nothing to pack still packs the token's own HEADER: 17 bytes, coded 24.
        Row {
            case: "no session state at all",
            policy: None,
            policy_arns: Vec::new(),
            tags: Vec::new(),
            measured: 24,
        },
    ]
}

#[tokio::test]
async fn a_mints_session_state_is_measured_at_the_length_of_its_packed_token() {
    let (router, engine) = emulator();
    let (role_arn, _role_id) = seed_demo_role(&engine);

    for row in rows() {
        let case = row.case;
        let mut params: Vec<(String, String)> = vec![
            ("RoleArn".to_string(), role_arn.clone()),
            ("RoleSessionName".to_string(), "measured".to_string()),
        ];
        params.extend(row.params());
        let params: Vec<(&str, String)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRole", &params).await;

        if row.measured > SESSION_TOKEN_SIZE_LIMIT {
            assert_eq!(
                status, 400,
                "case {case:?}: a state measured at {} bytes is over the budget, got {status} \
                 {body}",
                row.measured
            );
            assert_eq!(
                element(&body, "Code").as_deref(),
                Some("PackedPolicyTooLarge"),
                "case {case:?}: expected <Code>PackedPolicyTooLarge</Code>, got {body}"
            );
            let message = member(case, &body, "Message");
            assert_eq!(
                message,
                too_large_message(row.measured),
                "case {case:?}: expected the refusal to name {} bytes, got {message:?}",
                row.measured
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: a state measured at {} bytes is within the budget, got {status} \
                 {body}",
                row.measured
            );
            let reported = member(case, &body, "PackedPolicySize");
            let expected = percent_of_budget(row.measured).to_string();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {} bytes as {expected}% of the budget, got {reported:?} \
                 in {body}",
                row.measured
            );
        }
    }
}

/// RULE (the same measurement, stated as the invariant that tells it apart from a sum of
/// the contents): every element carries its own framing, so the same total content spread
/// across more elements packs into a LARGER token, and more bytes in one element pack into
/// a larger one too.
#[tokio::test]
async fn the_measured_size_counts_each_elements_own_framing() {
    let (router, engine) = emulator();
    let (role_arn, _role_id) = seed_demo_role(&engine);

    // One row: the case, the state measured at the larger figure, and the state measured at
    // the smaller one — the same content in fewer elements, or less content per element.
    let rows: Vec<(&str, Row, Row)> = vec![
        (
            "sixteen tags against eight carrying the same total content",
            Row::of_tags("sixteen tags of a 16-byte value", 16, 8, 16, 772),
            Row::of_tags("eight tags of a 32-byte value", 8, 8, 32, 568),
        ),
        (
            "four tags against one carrying the same total content",
            Row::of_tags("four tags of a 24-byte value", 4, 8, 24, 252),
            Row::of_tags("one tag of a 96-byte value", 1, 8, 96, 176),
        ),
        (
            "two tags whose values are longer",
            Row::of_tags("two tags of a 64-byte value", 2, 8, 64, 244),
            Row::of_tags("two tags of a 16-byte value", 2, 8, 16, 116),
        ),
    ];

    for (case, larger, smaller) in rows {
        let mut sizes = Vec::new();
        for row in [&larger, &smaller] {
            let mut params: Vec<(String, String)> = vec![
                ("RoleArn".to_string(), role_arn.clone()),
                ("RoleSessionName".to_string(), "measured".to_string()),
            ];
            params.extend(row.params());
            let params: Vec<(&str, String)> = params
                .iter()
                .map(|(name, value)| (name.as_str(), value.clone()))
                .collect();
            let (status, body) = call(&router, LONG_TERM_KEY, "AssumeRole", &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected a mint for {:?}, got {status} {body}",
                row.case
            );
            let reported = member(case, &body, "PackedPolicySize");
            let expected = percent_of_budget(row.measured).to_string();
            assert_eq!(
                reported, expected,
                "case {case:?}: {:?} is measured at {} bytes, so {expected}% of the budget, got \
                 {reported:?} in {body}",
                row.case, row.measured
            );
            sizes.push(row.measured);
        }
        assert!(
            sizes[0] > sizes[1],
            "case {case:?}: expected the state in more or longer elements to be measured \
             larger, got {sizes:?}"
        );
    }
}
