// SPDX-License-Identifier: CC0-1.0

//! How the CHANNEL listing classifies a `NextToken` that is not a cursor it issued, end to
//! end over the wire.
//!
//! RULE: a pagination token a listing did not issue is not a stale cursor but a malformed
//! request parameter, refused HTTP 400 with the fixed sentence "Invalid NextToken." under the
//! input-validation class the operation itself models for one — `ValidationException` for
//! `ListChannels`, which the model binds it to, and the generic `InvalidArgumentException`
//! for a listing it does not bind it to, where an unusable parameter "can't be used".
//! `ExpiredNextTokenException` stays reserved for a cursor the listing DID issue whose
//! lifetime has elapsed, so a paginating client can tell a cursor it corrupted or hand-wrote
//! from one it held on to for too long.
//!
//! The recorded flow this was derived from calls `ListChannels` once with a string the service
//! never issued, which is one row of the table below. The rows differ along what the rule
//! generalises over: whether the string was ever a cursor at all, HOW a cursor it issued was
//! altered (its tail rewritten, a character flipped in its middle, a byte dropped, its seal
//! stripped off, a character outside the hex alphabet), which listing and which region a
//! well-formed cursor was minted for, and how long the string is — including each published
//! bound of the member's length.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.

use std::time::{Duration, SystemTime};

use serde_json::json;
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    epoch_millis, next_token_expired_message, RegionalListing, RegionalPageToken,
    INVALID_NEXT_TOKEN_MESSAGE, PAGE_TOKEN_TTL,
};

use crate::channel_lifecycle::{on_demand_stream, role_arn, storage_configuration, stream_configuration};
use crate::stream_tags::{error_code, error_message, Emulator, REGION};

/// The class the model binds to `ListChannels` for a malformed request parameter
/// (`smithy_query(query="digest")` -> `ListChannels.errors`, which carries
/// `ValidationException`).
const CHANNELS_MALFORMED_CODE: &str = "ValidationException";

/// The class a listing the model binds no `ValidationException` to answers the same defect
/// with (same call -> `ListStreams.errors`, which carries none).
const STREAMS_MALFORMED_CODE: &str = "InvalidArgumentException";

/// The `@length` minimum of the token member
/// (`smithy_query(query="operation",operation="ListChannels")` -> `NextToken`, len 1..1048576).
const NEXT_TOKEN_MIN_LENGTH: usize = 1;

/// The `@length` maximum of the same member.
const NEXT_TOKEN_MAX_LENGTH: usize = 1_048_576;

/// One row of the table: a `NextToken` no listing of this service issued for the channels of
/// this region, and the name the failure reports it by.
struct Row {
    case: &'static str,
    token: String,
}

/// Create the channels the listing pages, and answer a cursor `ListChannels` itself handed
/// back for them: the first page is asked for one channel at a time, so the region holds one
/// past it.
async fn live_cursor(emulator: &Emulator) -> String {
    let case = "setup";
    let source = on_demand_stream(emulator, "channel-cursor-source").await;
    for name in ["cursor-refusal-a", "cursor-refusal-b"] {
        emulator
            .ok(
                case,
                "CreateChannel",
                &json!({
                    "ChannelName": name,
                    "ServiceExecutionRoleARN": role_arn("channel-delivery"),
                    "StreamConfigurationList": [stream_configuration(&source, "JSON", None)],
                    "S3DestinationConfiguration": {
                        "StorageConfiguration": storage_configuration(&format!("bucket-{name}")),
                    },
                }),
            )
            .await;
    }

    let page = emulator
        .ok(case, "ListChannels", &json!({ "MaxResults": 1 }))
        .await;
    let cursor = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: a truncated ListChannels page must hand back a NextToken, got {page}")
        })
        .to_string();
    assert!(
        cursor.len() > 32,
        "case {case}: expected a cursor long enough for every alteration of the table, got \
         {cursor:?}"
    );
    cursor
}

/// Every string the rule refuses as a malformed cursor, each one alongside the real cursor it
/// was derived from where it was derived from one.
fn rows(cursor: &str) -> Vec<Row> {
    vec![
        Row {
            case: "text this service never encoded",
            token: "not-a-cursor-this-listing-issued".to_string(),
        },
        Row {
            case: "the shortest string the member admits",
            token: "a".repeat(NEXT_TOKEN_MIN_LENGTH),
        },
        Row {
            case: "a string at the member's length ceiling",
            token: "f".repeat(NEXT_TOKEN_MAX_LENGTH),
        },
        Row {
            case: "the cursor's last eight characters replaced",
            token: format!("{}deadbeef", &cursor[..cursor.len() - 8]),
        },
        Row {
            case: "one character of the cursor flipped in the middle",
            token: {
                let middle = cursor.len() / 2;
                let mut altered: Vec<char> = cursor.chars().collect();
                altered[middle] = if altered[middle] == '0' { '1' } else { '0' };
                altered.into_iter().collect()
            },
        },
        Row {
            case: "a byte dropped from the end of the cursor",
            token: cursor[..cursor.len() - 2].to_string(),
        },
        Row {
            case: "the cursor's payload with its seal stripped off",
            token: cursor[..cursor.len() - 16].to_string(),
        },
        Row {
            case: "a character outside the hex alphabet",
            token: format!("z{}", &cursor[1..]),
        },
        Row {
            case: "a cursor minted for this listing in another region",
            token: RegionalPageToken::new(
                RegionalListing::Channels,
                "eu-west-1",
                "cursor-refusal-b",
                SystemTime::now(),
            )
            .encode(),
        },
        Row {
            case: "a cursor minted for another listing of this region",
            token: RegionalPageToken::new(
                RegionalListing::Streams,
                REGION,
                "cursor-refusal-b",
                SystemTime::now(),
            )
            .encode(),
        },
    ]
}

/// Assert the un-paginated listing still serves a page, which is what makes a refusal of a
/// token a judgement on the token and nothing else.
async fn assert_serves(emulator: &Emulator, case: &str) {
    let page = emulator
        .ok(case, "ListChannels", &json!({ "MaxResults": 1 }))
        .await;
    assert!(
        page.get("ChannelSummaries").is_some(),
        "case {case}: a served page carries ChannelSummaries, got {page}"
    );
}

/// Assert `operation` refuses `token` as a malformed parameter under `expected_code`, with the
/// fixed sentence the class is worded with.
async fn assert_malformed(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    expected_code: &str,
    token: &str,
) {
    let (status, response) = emulator
        .call(operation, &json!({ "NextToken": token }))
        .await;
    assert_eq!(
        status, 400,
        "case {case}: expected {operation} to refuse a token it never issued, got {status} \
         with body {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, expected_code,
        "case {case}: expected {expected_code} from {operation}, got {code:?}"
    );
    let message = error_message(&response);
    assert_eq!(
        message, INVALID_NEXT_TOKEN_MESSAGE,
        "case {case}: expected the message {INVALID_NEXT_TOKEN_MESSAGE:?} from {operation}, \
         got {message:?}"
    );
}

// 1. A token the channel listing never issued is its MALFORMED-PARAMETER class
#[tokio::test]
async fn a_token_the_channel_listing_never_issued_is_a_validation_failure() {
    let emulator = Emulator::new();
    let cursor = live_cursor(&emulator).await;

    for row in rows(&cursor) {
        assert_serves(&emulator, "a listing before the refusal").await;
        assert_malformed(
            &emulator,
            row.case,
            "ListChannels",
            CHANNELS_MALFORMED_CODE,
            &row.token,
        )
        .await;
        assert_serves(&emulator, "a listing after the refusal").await;
    }
}

// 2. The cursor the listing DID issue is served, and the expiry stays its own class
#[tokio::test]
async fn a_cursor_the_channel_listing_issued_is_served_or_expired() {
    let emulator = Emulator::new();
    let cursor = live_cursor(&emulator).await;

    let case = "a cursor inside its lifetime";
    let resumed = emulator
        .ok(case, "ListChannels", &json!({ "NextToken": cursor }))
        .await;
    assert!(
        resumed.get("ChannelSummaries").is_some(),
        "case {case}: expected the cursor to resume the listing, got {resumed}"
    );

    let case = "a cursor past its lifetime";
    let minted_at = SystemTime::now() - PAGE_TOKEN_TTL - Duration::from_secs(1);
    let expired = RegionalPageToken::new(
        RegionalListing::Channels,
        REGION,
        "cursor-refusal-b",
        minted_at,
    )
    .encode();
    let (status, response) = emulator
        .call("ListChannels", &json!({ "NextToken": expired }))
        .await;
    assert_eq!(
        status, 400,
        "case {case}: expected the expiry refusal, got {status} with body {response}"
    );
    let code = error_code(&response);
    assert_eq!(
        code, "ExpiredNextTokenException",
        "case {case}: expected ExpiredNextTokenException for a cursor this listing issued \
         whose lifetime has elapsed, got {code:?}"
    );
    let message = error_message(&response);
    assert_eq!(
        message,
        next_token_expired_message("ListChannels", epoch_millis(minted_at)),
        "case {case}: expected the published expiry wording, got {message:?}"
    );
}

// 3. A listing the model binds no ValidationException to keeps the generic class
#[tokio::test]
async fn a_listing_without_the_validation_class_keeps_the_generic_one() {
    let emulator = Emulator::new();

    // Strings NEITHER listing ever issued, so the one difference between the two calls below
    // is which operation reads them.
    let unissued: Vec<Row> = vec![
        Row {
            case: "text this service never encoded",
            token: "not-a-cursor-this-listing-issued".to_string(),
        },
        Row {
            case: "the shortest string the member admits",
            token: "a".repeat(NEXT_TOKEN_MIN_LENGTH),
        },
    ];

    // A cursor of the CHANNEL listing names no page of the stream listing, which is a row of
    // the stream listing's own table only.
    let other_listing = Row {
        case: "a cursor minted for another listing of this region",
        token: RegionalPageToken::new(
            RegionalListing::Channels,
            REGION,
            "cursor-refusal-b",
            SystemTime::now(),
        )
        .encode(),
    };

    for row in unissued.iter().chain([&other_listing]) {
        assert_malformed(
            &emulator,
            row.case,
            "ListStreams",
            STREAMS_MALFORMED_CODE,
            &row.token,
        )
        .await;
    }

    // The same strings reach the channel listing as its own class, so the difference is the
    // operation's modelled errors and not the string.
    for row in &unissued {
        let (status, response) = emulator
            .call("ListChannels", &json!({ "NextToken": row.token }))
            .await;
        let code = error_code(&response);
        assert_eq!(
            (status, code.as_str()),
            (400, CHANNELS_MALFORMED_CODE),
            "case {}: expected ListChannels to answer its own class for the same string, got \
             {status} with {response}",
            row.case
        );
    }
}
