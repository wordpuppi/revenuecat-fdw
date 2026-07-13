# Changelog

## [0.1.3] - 2026-07-13

Correctness hardening (AB#403 / AB#401):

- Pagination: an empty intermediate page no longer ends the scan early — advance
  through pages until rows are found or the `next_page` cursor is exhausted
  (previously a mid-scan empty page silently dropped every later page).
- Operator pushdown: only `=` quals are pushed down to RevenueCat's single-item
  and customer-scoped endpoints; every other operator (`!=`, `<>`, `>`, `LIKE`,
  ...) now falls through to a full list scan for Postgres to filter, instead of
  being silently treated as equality and returning the wrong rows.
- 404 scoping: a 404 is only swallowed as an empty result for single-item
  lookups; a 404 on a list endpoint (wrong project_id/customer_id) now surfaces
  as an error instead of a silent empty table.
- setup.sql guard rail: `REVOKE INSERT, DELETE ON revenuecat.customers` from the
  app roles + PUBLIC — the destructive customer-wipe DELETE is not an app op.

## [0.1.2] - 2026-02-23

- Move release CI from GitHub Actions to Gitea (build on personal server, publish to both Gitea and GitHub)
- Add Cargo.toml metadata (description, license, repository, keywords)

## [0.1.1] - 2026-02-23

- Fix `granted_entitlements` DELETE: add `id` column to DDL for composite `customer_id:entitlement_id` rowid
- Fix `subscriptions` and `purchases` bulk SELECT: route through customer-scoped endpoint (`/customers/{cid}/subscriptions`) instead of unsupported project-level list
- Return clear error when `customer_id` filter is missing for subscriptions/purchases
- Replace subscriptions-based materialized view example with products (bulk-listable)
- Add `supabase/snippets/setup.sql` with full copy-pasteable setup script

## [0.1.0] - 2026-02-22

- Initial release
- Read support for customers, subscriptions, purchases, products, entitlements, offerings
- Write support: INSERT/DELETE for customers, entitlements, offerings, granted_entitlements
- Cursor-based pagination with automatic `next_page` following
- ID pushdown optimization (`WHERE id = '...'` hits single-object endpoint)
- `attrs jsonb` escape hatch column on every table
- Supabase Vault integration for API key storage
- SHA256 checksum published with each release
