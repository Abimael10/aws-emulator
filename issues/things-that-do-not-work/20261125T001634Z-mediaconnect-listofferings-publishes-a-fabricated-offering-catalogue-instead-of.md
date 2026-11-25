Issue-Source: trace-sweep-divergence
Issue-Trace: mediaconnect-catalog-list-offerings
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediaconnect-catalog-list-offerings
Issue-Shape-Id: com.amazonaws.mediaconnect#MediaConnect
Issue-Key: mediaconnect|ListOfferings|2xx->2xx|$.offerings[*]
Issue-Commit: 75f577a9b4ae922afffed072560bc3cac4bdc4f1

# ListOfferings publishes a fabricated offering catalogue instead of the Region's published price list

> _Note: this issue was identified against the code base at commit `75f577a9b4ae922afffed072560bc3cac4bdc4f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListOfferings

## Description
The offering catalogue this emulator publishes for a Region is not the catalogue AWS publishes: it carries three times as many tiers as the recording, and the tier the recording shares with it is published under a different offering id at a different rate. ListOfferings, DescribeOffering and PurchaseOffering all read that one projection (`crate::state::published_offerings`), so every offering ARN and price the service hands out is affected, not just the listing.

Cause 1 of 1: the seeded offering catalogue has the wrong membership and wrong terms for the 500 Mbps tier (fixture mediaconnect-catalog-list-offerings, step task#1).

AWS: Smithy: ListOfferings GET /v1/offerings -> 200, output ListOfferingsResponse{offerings: __listOfOffering (jsonName "offerings"), nextToken}; MaxResults @range 1..1000, documented default "pagination with a maximum of 10 results per page". The model fixes only what an entry MEANS (Offering.PriceUnits = HOURLY, DurationUnits = MONTHS, ResourceType = Mbps_Outbound_Bandwidth, CurrencyCode USD) and carries NO price list, so the catalogue's membership, its offering ids and its rates are recorded behavior only. The recording shows the Region's catalogue as exactly FOUR offerings, all with duration 12 MONTHS, priceUnits HOURLY, currencyCode USD, resourceType Mbps_Outbound_Bandwidth, and no nextToken: 50 Mbps at pricePerUnit "1.161" (offering:80246697), 1500 Mbps at "11.444" (offering:80248314), 150 Mbps at "2.834" (offering:80248601), and 500 Mbps at "6.474" under offeringArn arn:aws:mediaconnect:<region>:offering:80248603, whose offeringDescription is "Reserved outbound bandwidth of 500Mbps at $6.474/hour". In that description only "Reserved outbound bandwidth of ", "Mbps at $" and "/hour" are fixed text; the bitrate (500) and the price (6.474) are that catalogue entry's own terms, and the region token in the ARN comes from the signed request, so none of the three may be taken from one run literally -- but the SET of entries and each entry's id/bitrate/rate are catalogue data that is the same for every account in the Region, which is exactly what a client may pin.

Emulator: the same request answers 200 with TWELVE offerings. Elements 0..2 agree with AWS (80246697 / 80248314 / 80248601). At index 3 AWS's 500 Mbps entry (offering:80248603, pricePerUnit "6.474") is MISSING, replaced by offering:80249012, 100 Mbps, pricePerUnit "2.039", "Reserved outbound bandwidth of 100Mbps at $2.039/hour"; indices 4..11 are eight further entries AWS does not publish at all (e.g. offering:80249113 200 Mbps "3.374", offering:80249214 250 Mbps, ... offering:80249820 1000 Mbps). The 500 Mbps tier does exist in the emulator but under a different id and rate (offering:80249719 at "5.879" instead of offering:80248603 at "6.474"). Replay reports $.offerings[3] missing+extra and $.offerings[4]..$.offerings[11] extra; status and nextToken match.

Why: an offering ARN is the whole of an offering's identity to a client -- it is read from a listing and handed back to DescribeOffering and PurchaseOffering. A client that pins the ARN it recorded against AWS (arn:aws:mediaconnect:<region>:offering:80248603) resolves nothing here, so describing or purchasing the 500 Mbps reservation fails against the emulator while succeeding against AWS; conversely a client that discovers an offering here gets ARNs and rates AWS never published, and any cost/assertion logic reading pricePerUnit or the offeringDescription computes the wrong price ($5.879/hour vs $6.474/hour for 500 Mbps). The inflated catalogue also changes pagination: with the documented default page size of 10 the real catalogue is one short page with no nextToken, while the emulator's twelve entries split into pages, so a client that walks pages sees a shape AWS never produces.

Fix: the service's offering state -- the seeded catalogue data and its projection, not the listing handler, which pages correctly over whatever the catalogue holds. The catalogue's membership must be the Region's published entries (the four the recording shows), and the 500 Mbps tier must carry AWS's offering id and rate so that its ARN, pricePerUnit and the description rendered from them agree with AWS. DescribeOffering/PurchaseOffering need no separate change once the single projection is corrected.

Traces: mediaconnect-catalog-list-offerings#task#1 (ListOfferings, expected 200 / actual 200, body diverges at $.offerings[3] and $.offerings[4]..[11]). Related open issue 20261024T010542Z-mediaconnect-purchaseoffering-stub.md covers PurchaseOffering being an unimplemented stub; it does not cover the catalogue contents this issue reports.

## Evidence
Fixture mediaconnect-catalog-list-offerings, step task#1 -- ListOfferings, expected HTTP 200, actual HTTP 200, body mismatch.

Expected (recorded AWS): offerings = 4 entries, no nextToken; $.offerings[3] = {"offeringArn": "arn:aws:mediaconnect:us-west-2:offering:80248603", "offeringDescription": "Reserved outbound bandwidth of 500Mbps at $6.474/hour", "pricePerUnit": "6.474", "priceUnits": "HOURLY", "currencyCode": "USD", "duration": 12, "durationUnits": "MONTHS", "resourceSpecification": {"reservedBitrate": 500, "resourceType": "Mbps_Outbound_Bandwidth"}}.

Actual (emulator): offerings = 12 entries; $.offerings[3] = {"offeringArn": "arn:aws:mediaconnect:us-west-2:offering:80249012", "offeringDescription": "Reserved outbound bandwidth of 100Mbps at $2.039/hour", "pricePerUnit": "2.039", "resourceSpecification": {"reservedBitrate": 100, ...}}; $.offerings[4] = offering:80249113 (200 Mbps, "3.374"); $.offerings[5]..$.offerings[11] likewise extra (250/300/350/400/450/500 at "5.879"/1000 Mbps tiers). Replay keys: $.offerings[3],$.offerings[4],$.offerings[5],$.offerings[6],$.offerings[7],$.offerings[8],$.offerings[9],$.offerings[10],$.offerings[11].

Source of the divergence in the checkout: services/service-com-amazonaws-mediaconnect-mediaconnect/src/state/offerings.rs lines 94-107 (OFFERING_CATALOGUE, 12 hard-coded tiers; no entry 80248603, 500 Mbps published as 80249719 at 5_879 thousandths) projected by published_offerings() at lines 198-225 and paged by services/service-com-amazonaws-mediaconnect-mediaconnect/src/ops/list_offerings.rs lines 223-244.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture mediaconnect-catalog-list-offerings, step task#1.
2. Equivalently: GET /v1/offerings signed for the Region the fixture uses (us-west-2), with no NextToken.
3. Observe 200 with 12 offerings. AWS returns the Region's published catalogue of 4 offerings and no nextToken.
4. Compare index 3: AWS returns offeringArn arn:aws:mediaconnect:<region>:offering:80248603, 500 Mbps, pricePerUnit "6.474"; the emulator returns offering:80249012, 100 Mbps, "2.039", and eight further offerings AWS does not publish.
5. Take any offeringArn AWS published (…:offering:80248603) and call DescribeOffering/PurchaseOffering with it: the emulator's catalogue does not hold it.
