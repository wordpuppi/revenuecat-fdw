# RevenueCat Foreign Data Wrapper for PostgreSQL

A WebAssembly Foreign Data Wrapper (WASM FDW) that lets you query [RevenueCat](https://www.revenuecat.com/) subscription data as native PostgreSQL tables. Works with [Supabase Wrappers](https://supabase.github.io/wrappers/) or any PostgreSQL instance with the Wrappers extension.

**First RevenueCat FDW** — no existing WASM or native FDW for RevenueCat exists in the Supabase Wrappers catalog or elsewhere.

Built by [WordPuppi](https://wordpuppi.com).

## Supported Tables

| Table | RevenueCat Object | Read | Write | Rate Limit |
|-------|-------------------|------|-------|------------|
| `customers` | Customers | Yes | INSERT, DELETE | 480/min |
| `subscriptions` | Subscriptions | Yes | — | 480/min |
| `purchases` | Purchases | Yes | — | 480/min |
| `products` | Products | Yes | — | 60/min |
| `entitlements` | Entitlements | Yes | INSERT, DELETE | 60/min |
| `offerings` | Offerings | Yes | INSERT, DELETE | 60/min |
| `granted_entitlements` | Grant/Revoke actions | — | INSERT, DELETE | 480/min |

## Features

- **Cursor-based pagination** — automatically follows `next_page` links for large datasets
- **ID pushdown** — `WHERE id = 'xxx'` queries hit the single-object endpoint (1 API call vs pagination)
- **`attrs jsonb` escape hatch** — every table includes an `attrs` column with the full API response
- **Vault integration** — API keys stored securely in Supabase Vault
- **Write support** — create/delete customers, create/delete entitlements/offerings, grant/revoke entitlements

## Setup

### 1. Store your API key in Vault

```sql
select vault.create_secret('sk_xxx_your_revenuecat_v2_api_key', 'revenuecat', 'RevenueCat API key');
```

> You need a RevenueCat **v2 secret API key**. v1 keys won't work. Create one in your RevenueCat project under Project Settings > API Keys.

### 2. Create the foreign server

```sql
create server revenuecat_server
  foreign data wrapper wasm_wrapper
  options (
    fdw_package_url 'https://github.com/wordpuppi/revenuecat-fdw/releases/download/v0.1.1/revenuecat_fdw.wasm',
    fdw_package_name 'wordpuppi:revenuecat-fdw',
    fdw_package_version '0.1.1',
    fdw_package_checksum '<sha256-from-release>',
    api_url 'https://api.revenuecat.com/v2',
    project_id 'proj_your_project_id',
    api_key_id '<vault-secret-uuid>'
  );
```

### 3. Create foreign tables

```sql
create schema if not exists revenuecat;

-- Customers
create foreign table revenuecat.customers (
  id text,
  project_id text,
  first_seen_at timestamp,
  last_seen_at timestamp,
  last_seen_app_version text,
  last_seen_country text,
  last_seen_platform text,
  attrs jsonb
)
server revenuecat_server
options (object 'customers', rowid_column 'id');

-- Subscriptions (customer-scoped: requires WHERE customer_id = '...')
create foreign table revenuecat.subscriptions (
  id text,
  customer_id text,
  product_id text,
  status text,
  auto_renewal_status text,
  gives_access boolean,
  starts_at timestamp,
  current_period_starts_at timestamp,
  current_period_ends_at timestamp,
  environment text,
  store text,
  country text,
  attrs jsonb
)
server revenuecat_server
options (object 'subscriptions', rowid_column 'id');

-- Purchases (customer-scoped: requires WHERE customer_id = '...')
create foreign table revenuecat.purchases (
  id text,
  customer_id text,
  product_id text,
  purchased_at timestamp,
  quantity int,
  status text,
  environment text,
  store text,
  country text,
  attrs jsonb
)
server revenuecat_server
options (object 'purchases', rowid_column 'id');

-- Products
create foreign table revenuecat.products (
  id text,
  store_identifier text,
  type text,
  display_name text,
  app_id text,
  created_at timestamp,
  attrs jsonb
)
server revenuecat_server
options (object 'products', rowid_column 'id');

-- Entitlements (project-level config)
create foreign table revenuecat.entitlements (
  id text,
  project_id text,
  lookup_key text,
  display_name text,
  created_at timestamp,
  attrs jsonb
)
server revenuecat_server
options (object 'entitlements', rowid_column 'id');

-- Offerings
create foreign table revenuecat.offerings (
  id text,
  lookup_key text,
  display_name text,
  is_current boolean,
  project_id text,
  created_at timestamp,
  attrs jsonb
)
server revenuecat_server
options (object 'offerings', rowid_column 'id');

-- Granted entitlements (write-only: grant/revoke)
create foreign table revenuecat.granted_entitlements (
  id             text,       -- composite: "customer_id:entitlement_id"
  customer_id    text,
  entitlement_id text,
  expires_at     timestamp
)
server revenuecat_server
options (object 'granted_entitlements', rowid_column 'id');
```

## Query Examples

### Customer subscriptions (customer-scoped)

> **Note:** Subscriptions and purchases are customer-scoped. You must filter by `customer_id`:
> ```sql
> SELECT * FROM revenuecat.subscriptions WHERE customer_id = 'cust123';
> ```
> Unfiltered `SELECT *` is not supported by the RevenueCat API.

```sql
select id, status, product_id, current_period_ends_at
from revenuecat.subscriptions
where customer_id = 'user_12345';
```

### Single customer lookup (uses ID pushdown — 1 API call)

```sql
select * from revenuecat.customers where id = 'user_12345';
```

### Revenue from the full response JSON

```sql
select
  id,
  attrs->'total_revenue_in_usd'->>'gross' as revenue_gross,
  attrs->'total_revenue_in_usd'->>'currency' as currency
from revenuecat.subscriptions
where customer_id = 'user_12345';
```

### Create a customer

```sql
insert into revenuecat.customers (id) values ('wpp_new_customer');
```

### Grant an entitlement

```sql
insert into revenuecat.granted_entitlements (customer_id, entitlement_id, expires_at)
values ('wpp_new_customer', 'entla1b2c3d4e5', '2027-01-01'::timestamp);
```

### Revoke a granted entitlement

```sql
-- rowid is composite: 'customer_id:entitlement_id'
delete from revenuecat.granted_entitlements
where id = 'wpp_new_customer:entla1b2c3d4e5';
```

### Delete a customer

```sql
delete from revenuecat.customers where id = 'wpp_new_customer';
```

### Materialized view for analytics

```sql
-- Products are bulk-listable (no customer_id filter needed)
create materialized view revenuecat.products_cache as
select id, store_identifier, display_name, type, attrs
from revenuecat.products;

-- Refresh periodically (respects rate limits — 60 req/min for products)
refresh materialized view revenuecat.products_cache;
```

## Rate Limits

RevenueCat enforces per-API-key rate limits:

| Domain | Limit | Affected Tables |
|--------|-------|-----------------|
| Customer Information | 480 req/min | customers, subscriptions, purchases, granted_entitlements |
| Project Configuration | 60 req/min | products, entitlements, offerings |

For high-volume analytics, use `MATERIALIZED VIEW` with scheduled refreshes rather than live queries.

## Build from Source

```bash
# Prerequisites
rustup target add wasm32-unknown-unknown
cargo install cargo-component

# Build
cargo component build --release --target wasm32-unknown-unknown

# Output: target/wasm32-unknown-unknown/release/revenuecat_fdw.wasm
```

## How RevenueCat Timestamps Work

RevenueCat API v2 uses **millisecond epoch integers** for all timestamp fields (e.g., `first_seen_at: 1658399423658`). The FDW automatically converts these to PostgreSQL `timestamp` values. The `attrs jsonb` column always contains the raw millisecond values.

## License

MIT
