-- RevenueCat FDW Setup
-- Run this SQL in your Supabase project to connect RevenueCat data as native PostgreSQL tables.
--
-- Prerequisites:
--   1. Supabase Wrappers extension enabled (available on all Supabase projects)
--   2. A RevenueCat v2 secret API key (Project Settings > API Keys — v1 keys will NOT work)
--   3. Your RevenueCat project ID (visible in the RevenueCat dashboard URL)
--
-- Steps:
--   1. Replace 'sk_xxx_your_revenuecat_v2_api_key' with your actual key
--   2. Replace 'proj_your_project_id' with your RevenueCat project ID
--   3. After CREATE SERVER, replace '<sha256-from-release>' with the checksum from
--      https://github.com/webpuppi/revenuecat-fdw/releases/tag/v0.1.0
--   4. Run the full script

-- ============================================================
-- Step 1: Store API key securely in Vault
-- ============================================================
-- The UUID returned here is used as api_key_id in the server definition below.
-- Save it: SELECT vault.create_secret(...) returns the secret UUID.

select vault.create_secret(
  'sk_xxx_your_revenuecat_v2_api_key',
  'revenuecat',
  'RevenueCat v2 secret API key'
);

-- ============================================================
-- Step 2: Create the foreign server
-- Replace api_key_id with the UUID returned from Step 1.
-- Replace fdw_package_checksum with the SHA256 from the GitHub release.
-- ============================================================

create server revenuecat_server
  foreign data wrapper wasm_wrapper
  options (
    fdw_package_url     'https://github.com/wordpuppi/revenuecat-fdw/releases/download/v0.1.1/revenuecat_fdw.wasm',
    fdw_package_name    'wordpuppi:revenuecat-fdw',
    fdw_package_version '0.1.1',
    fdw_package_checksum '<sha256-from-release>',
    api_url             'https://api.revenuecat.com/v2',
    project_id          'proj_your_project_id',
    api_key_id          '<vault-secret-uuid>'
  );

-- ============================================================
-- Step 3: Create schema and foreign tables
-- ============================================================

create schema if not exists revenuecat;

-- Customers
-- Supports: SELECT, INSERT (create customer), DELETE (delete customer)
-- ID pushdown: WHERE id = 'xxx' uses single-object endpoint (1 API call)
create foreign table revenuecat.customers (
  id                    text,
  project_id            text,
  first_seen_at         timestamp,
  last_seen_at          timestamp,
  last_seen_app_version text,
  last_seen_country     text,
  last_seen_platform    text,
  attrs                 jsonb       -- full API response (escape hatch)
)
server revenuecat_server
options (object 'customers', rowid_column 'id');

-- Subscriptions (customer-scoped)
-- Supports: SELECT only
-- IMPORTANT: Requires WHERE customer_id = '...' filter (no bulk list endpoint)
-- ID pushdown: WHERE id = 'xxx' also supported
create foreign table revenuecat.subscriptions (
  id                         text,
  customer_id                text,
  product_id                 text,
  status                     text,    -- active | expired | in_trial | in_grace_period | ...
  auto_renewal_status        text,    -- will_renew | will_not_renew | ...
  gives_access               boolean,
  starts_at                  timestamp,
  current_period_starts_at   timestamp,
  current_period_ends_at     timestamp,
  environment                text,    -- production | sandbox
  store                      text,    -- app_store | play_store | stripe | ...
  country                    text,
  attrs                      jsonb
)
server revenuecat_server
options (object 'subscriptions', rowid_column 'id');

-- Purchases (customer-scoped, one-time)
-- Supports: SELECT only
-- IMPORTANT: Requires WHERE customer_id = '...' filter (no bulk list endpoint)
-- ID pushdown: WHERE id = 'xxx' also supported
create foreign table revenuecat.purchases (
  id           text,
  customer_id  text,
  product_id   text,
  purchased_at timestamp,
  quantity     int,
  status       text,
  environment  text,
  store        text,
  country      text,
  attrs        jsonb
)
server revenuecat_server
options (object 'purchases', rowid_column 'id');

-- Products (project-level catalog)
-- Supports: SELECT only
-- ID pushdown: WHERE id = 'xxx' supported
-- Note: lower rate limit (60 req/min)
create foreign table revenuecat.products (
  id               text,
  store_identifier text,
  type             text,    -- subscription | one_time
  display_name     text,
  app_id           text,
  created_at       timestamp,
  attrs            jsonb
)
server revenuecat_server
options (object 'products', rowid_column 'id');

-- Entitlements (project-level access configuration)
-- Supports: SELECT, INSERT (create entitlement), DELETE (delete entitlement)
-- ID pushdown: WHERE id = 'xxx' supported
-- Note: lower rate limit (60 req/min)
create foreign table revenuecat.entitlements (
  id           text,
  project_id   text,
  lookup_key   text,
  display_name text,
  created_at   timestamp,
  attrs        jsonb
)
server revenuecat_server
options (object 'entitlements', rowid_column 'id');

-- Offerings (paywall configuration)
-- Supports: SELECT, INSERT (create offering), DELETE (delete offering)
-- ID pushdown: WHERE id = 'xxx' supported
-- Note: lower rate limit (60 req/min)
create foreign table revenuecat.offerings (
  id           text,
  lookup_key   text,
  display_name text,
  is_current   boolean,
  project_id   text,
  created_at   timestamp,
  attrs        jsonb
)
server revenuecat_server
options (object 'offerings', rowid_column 'id');

-- Granted entitlements (write-only: grant or revoke access)
-- Supports: INSERT (grant), DELETE (revoke) only — no SELECT
-- To revoke: DELETE WHERE id = 'customer_id:entitlement_id'
create foreign table revenuecat.granted_entitlements (
  id             text,       -- composite: "customer_id:entitlement_id"
  customer_id    text,
  entitlement_id text,
  expires_at     timestamp   -- NULL = permanent grant
)
server revenuecat_server
options (object 'granted_entitlements', rowid_column 'id');

-- ============================================================
-- Optional: Materialized view for analytics
-- Products are bulk-listable (no customer_id filter needed).
-- Refresh periodically to respect rate limits (60 req/min for products).
-- ============================================================

create materialized view revenuecat.products_cache as
select id, store_identifier, display_name, type, attrs
from revenuecat.products;

-- Refresh with: REFRESH MATERIALIZED VIEW revenuecat.products_cache;
-- Schedule this via pg_cron or Supabase Edge Function cron for live analytics.
