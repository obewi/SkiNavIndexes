# Source fingerprints and resilient Overpass enrichment

Completed 2026-09-16.

The producer publishes a fixed 32-character lowercase-hex `sourceFingerprint`
for every resort. It is the first 128 bits of a versioned SHA-256 digest over
canonical normalized source content for the root-plus-descendants processing
scope; release timestamps, dataset versions, and pack layout are excluded.

Overpass station queries are bounded to discovered station source IDs and
split into batches of 200. Station entries and the whole connection result
are persisted under `.overpass`, refreshed after 120 days, paced across the
default, Mail.ru, private.coffee, and Japanese fallback endpoints, and reused
as stale data if a refresh cannot complete. The workflow stores the rolling
cache in a GitHub Release rather than Actions cache storage.

Verification: formatting, 31 unit tests, the pipeline-structure test, and the
release build pass.
