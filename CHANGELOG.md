# Changelog

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
