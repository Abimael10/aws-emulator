// SPDX-License-Identifier: CC0-1.0

//! Amazon Simple Queue Service's published default-quota catalog.

use super::*;

/// Amazon SQS's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `sqs` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code sqs` reports, which is
/// the "Amazon SQS quotas" page of the Amazon SQS Developer Guide), and the `QuotaCode`s
/// that catalog is keyed on.
///
/// SQS publishes almost all of its quotas as NOT adjustable: they are properties of the
/// queue the service implements — how large a message may be, how long it may be hidden
/// or kept, how many of them may be in flight — rather than allowances an account may ask
/// to have raised. A row that measures something carries the unit AWS publishes it in
/// (bytes for a size, seconds for a window, days for a retention) rather than being a
/// bare count.
///
/// The recorded listing states the quota published under `L-98134C75` (`Attributes per
/// Message`, 10, with the sentence SQS words it with), under `L-81970B52` (`Message Size
/// in S3 Bucket`, 2 Gigabytes), under `L-BBEFA6CF` (`Queue Policy Size`, 8192 Bytes, NOT
/// adjustable) and under `L-BF2A6161` (`UTF-8 Queue Tag Value Length`, 256). The message
/// size, the retention period and the two transaction rates this catalog used to key on
/// those four codes are NOT published here, and neither are the codes it used to key the
/// message-attribute count and the policy size on (`L-6A03DCE9`, `L-A7816957`): no source
/// available here states AWS's code for the first four or AWS's quota for the last two, so
/// they are left to the default-quota-set gap rather than served under another quota's
/// code. A row whose published `Description` no source available here states carries none,
/// rather than a sentence this emulator words itself.
pub(in crate::state) fn sqs_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Batch Entries per Batch Request", 10.0)
            .code("L-2DA3E3B2")
            .fixed()
            .describe("The number of entries in a batch request."),
        quota("Delay Queue Delay", 900.0)
            .code("L-49DA5CEC")
            .unit("Seconds")
            .fixed()
            .describe("The delay, in seconds, that a delay queue applies to a message."),
        quota("Maximum Receives per Redrive Policy", 1000.0)
            .code("L-4BE1B2BD")
            .fixed()
            .describe(
                "The number of times a message is received before a redrive policy moves \
                 it to the dead-letter queue.",
            ),
        quota("In Flight Messages per FIFO Queue", 20_000.0)
            .code("L-4FF3DAAF")
            .fixed()
            .describe("The number of in flight messages for a FIFO queue."),
        quota("Message Group ID Length", 128.0)
            .code("L-79B6240C")
            .fixed()
            .describe("The number of characters in a message group ID."),
        quota("Message Size in S3 Bucket", 2.0)
            .code("L-81970B52")
            .unit("Gigabytes")
            .fixed(),
        quota("Attributes per Message", 10.0)
            .code("L-98134C75")
            .fixed()
            .describe("The number of attributes added to a message."),
        quota("In Flight Messages per Standard Queue", 120_000.0)
            .code("L-9F628B95")
            .fixed()
            .describe("The number of in flight messages for a standard queue."),
        quota("Queue Name Length", 80.0)
            .code("L-A01B4DF0")
            .fixed()
            .describe("The number of characters in a queue name."),
        quota("Tags per Queue", 50.0)
            .code("L-B2A3B9D5")
            .fixed()
            .describe("The number of tags on a queue."),
        quota("Queue Policy Size", 8192.0)
            .code("L-BBEFA6CF")
            .unit("Bytes")
            .fixed()
            .describe("The size, in bytes, of a queue policy."),
        quota("UTF-8 Queue Tag Value Length", 256.0)
            .code("L-BF2A6161")
            .fixed(),
        quota("Long Polling Wait Time", 20.0)
            .code("L-C491D5A4")
            .unit("Seconds")
            .fixed()
            .describe("The time, in seconds, that a receive request waits for a message."),
        quota("Visibility Timeout", 43_200.0)
            .code("L-E8A9C91E")
            .unit("Seconds")
            .fixed()
            .describe(
                "The time, in seconds, that a received message is hidden from the other \
                 consumers of the queue.",
            ),
        quota("Message Deduplication ID Length", 128.0)
            .code("L-F115E65A")
            .fixed()
            .describe("The number of characters in a message deduplication ID."),
        // The principal plane of a queue policy: a count of the principals the policy
        // document may name, which AWS publishes as fixed.
        quota("Principals per Queue Policy", 50.0)
            .code("L-F61F33C3")
            .fixed()
            .describe("The number of principals in a queue policy."),
    ]
}
