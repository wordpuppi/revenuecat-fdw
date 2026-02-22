#[allow(warnings)]
mod bindings;
use serde_json::{Map as JsonMap, Value as JsonValue};

use bindings::{
    exports::supabase::wrappers::routines::Guest,
    supabase::wrappers::{
        http, stats, time,
        types::{Cell, Column, Context, FdwError, FdwResult, OptionsType, Row, TypeOid, Value},
        utils,
    },
};

#[derive(Debug, Default)]
struct RevenueCatFdw {
    base_url: String,
    project_id: String,
    url: Option<String>,
    headers: Vec<(String, String)>,
    object: String,
    src_rows: Vec<JsonValue>,
    src_idx: usize,
    rowid_col: String,
}

static mut INSTANCE: *mut RevenueCatFdw = std::ptr::null_mut::<RevenueCatFdw>();
static FDW_NAME: &str = "RevenueCatFdw";

impl RevenueCatFdw {
    fn init_instance() {
        let instance = Self::default();
        unsafe {
            INSTANCE = Box::leak(Box::new(instance));
        }
    }

    fn this_mut() -> &'static mut Self {
        unsafe { &mut (*INSTANCE) }
    }

    // Objects that support single-item GET by ID
    fn can_pushdown_id(&self) -> bool {
        matches!(
            self.object.as_str(),
            "customers"
                | "subscriptions"
                | "purchases"
                | "products"
                | "entitlements"
                | "offerings"
        )
    }

    // Build the list endpoint URL for a given object
    fn list_url(&self, object: &str) -> String {
        format!(
            "{}/projects/{}/{}?limit=100",
            self.base_url, self.project_id, object
        )
    }

    // Build the single-item endpoint URL
    fn item_url(&self, object: &str, id: &str) -> String {
        format!(
            "{}/projects/{}/{}/{}",
            self.base_url, self.project_id, object, id
        )
    }

    // Fetch a page of results from RevenueCat API
    fn make_request(&mut self, ctx: &Context) -> FdwResult {
        let quals = ctx.get_quals();

        let url = if let Some(ref url) = self.url {
            // Pagination: use the stored next_page URL
            if url.starts_with("http") {
                url.clone()
            } else {
                // next_page is a relative path like /v2/projects/...
                format!("https://api.revenuecat.com{}", url)
            }
        } else {
            // First request: check for ID pushdown
            let pushdown_id = quals.iter().find(|q| q.field() == "id").and_then(|q| {
                if !self.can_pushdown_id() {
                    return None;
                }
                match q.value() {
                    Value::Cell(Cell::String(s)) => Some(s),
                    _ => None,
                }
            });

            match pushdown_id {
                Some(id) => self.item_url(&self.object, &id),
                None => self.list_url(&self.object),
            }
        };

        let req = http::Request {
            method: http::Method::Get,
            url,
            headers: self.headers.clone(),
            body: String::default(),
        };
        let resp = http::get(&req)?;

        let resp_json: JsonValue = serde_json::from_str(&resp.body).map_err(|e| e.to_string())?;

        // Handle 404 — resource not found is not an error, just empty results
        if resp.status_code == 404 {
            let is_not_found = resp_json
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| t == "resource_missing")
                .unwrap_or(false);
            if is_not_found {
                self.src_rows = Vec::new();
                self.src_idx = 0;
                self.url = None;
                return Ok(());
            }
        }

        http::error_for_status(&resp).map_err(|err| format!("{}: {}", err, resp.body))?;

        stats::inc_stats(FDW_NAME, stats::Metric::BytesIn, resp.body.len() as i64);

        // Parse response — either a list or a single object
        let object_type = resp_json
            .get("object")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if object_type == "list" {
            // List response: { "object": "list", "items": [...], "next_page": "..." }
            self.src_rows = resp_json
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            // Cursor pagination via next_page
            self.url = resp_json
                .get("next_page")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned());
        } else {
            // Single object response — wrap in a vec
            self.src_rows = vec![resp_json];
            self.url = None;
        }

        self.src_idx = 0;

        Ok(())
    }

    // Convert a RevenueCat JSON field to a PostgreSQL cell
    fn src_to_cell(&self, src_row: &JsonValue, tgt_col: &Column) -> Result<Option<Cell>, FdwError> {
        let tgt_col_name = tgt_col.name();

        // Special "attrs" column: dump the entire row as JSON
        if tgt_col_name == "attrs" {
            return Ok(Some(Cell::Json(src_row.to_string())));
        }

        let src = match src_row.as_object().and_then(|v| v.get(&tgt_col_name)) {
            Some(v) => v,
            None => return Ok(None), // Column not in response — return NULL
        };

        if src.is_null() {
            return Ok(None);
        }

        let cell = match tgt_col.type_oid() {
            TypeOid::Bool => src.as_bool().map(Cell::Bool),
            TypeOid::I8 => src.as_i64().map(|v| Cell::I8(v as i8)),
            TypeOid::I16 => src.as_i64().map(|v| Cell::I16(v as i16)),
            TypeOid::I32 => src.as_i64().map(|v| Cell::I32(v as i32)),
            TypeOid::I64 => src.as_i64().map(Cell::I64),
            TypeOid::F32 => src.as_f64().map(|v| Cell::F32(v as f32)),
            TypeOid::F64 => src.as_f64().map(Cell::F64),
            TypeOid::Numeric => src.as_f64().map(Cell::Numeric),
            TypeOid::String => {
                // RevenueCat sometimes returns non-string scalars in fields
                // we map to text — coerce them
                if let Some(s) = src.as_str() {
                    Some(Cell::String(s.to_owned()))
                } else {
                    Some(Cell::String(src.to_string()))
                }
            }
            TypeOid::Timestamp => {
                // RevenueCat uses millisecond epoch integers for timestamps
                if let Some(ms) = src.as_i64() {
                    // Convert ms to microseconds (what Supabase Wrappers expects)
                    Some(Cell::Timestamp(ms * 1_000))
                } else if let Some(s) = src.as_str() {
                    // Fallback: try RFC3339 string
                    let ts = time::parse_from_rfc3339(s)?;
                    Some(Cell::Timestamp(ts))
                } else {
                    None
                }
            }
            TypeOid::Timestamptz => {
                if let Some(ms) = src.as_i64() {
                    Some(Cell::Timestamptz(ms * 1_000))
                } else if let Some(s) = src.as_str() {
                    let ts = time::parse_from_rfc3339(s)?;
                    Some(Cell::Timestamptz(ts))
                } else {
                    None
                }
            }
            TypeOid::Date => {
                if let Some(ms) = src.as_i64() {
                    // Date expects seconds since epoch
                    Some(Cell::Date(ms / 1_000))
                } else if let Some(s) = src.as_str() {
                    let ts = time::parse_from_rfc3339(s)?;
                    Some(Cell::Date(ts / 1_000_000))
                } else {
                    None
                }
            }
            TypeOid::Json => {
                // Accept any JSON value (object, array, string, etc.)
                Some(Cell::Json(src.to_string()))
            }
        };

        Ok(cell)
    }

    // Convert a Row to a JSON body for POST requests
    fn row_to_body(&self, row: &Row) -> Result<String, FdwError> {
        let mut map = JsonMap::new();
        for (col_name, cell) in row.cols().iter().zip(row.cells().iter()) {
            if let Some(cell) = cell {
                let value = match cell {
                    Cell::Bool(v) => JsonValue::Bool(*v),
                    Cell::I8(v) => JsonValue::Number((*v as i64).into()),
                    Cell::I16(v) => JsonValue::Number((*v as i64).into()),
                    Cell::I32(v) => JsonValue::Number((*v as i64).into()),
                    Cell::I64(v) => JsonValue::Number((*v).into()),
                    Cell::String(v) => JsonValue::String(v.clone()),
                    Cell::Timestamp(v) => {
                        // Convert microseconds back to milliseconds for RevenueCat
                        JsonValue::Number((v / 1_000).into())
                    }
                    Cell::Timestamptz(v) => JsonValue::Number((v / 1_000).into()),
                    Cell::Json(v) => {
                        serde_json::from_str::<JsonValue>(v).map_err(|e| e.to_string())?
                    }
                    _ => {
                        return Err(format!("column '{}' type not supported for write", col_name));
                    }
                };
                map.insert(col_name.to_owned(), value);
            }
        }
        Ok(JsonValue::Object(map).to_string())
    }
}

impl Guest for RevenueCatFdw {
    fn host_version_requirement() -> String {
        "^0.1.0".to_string()
    }

    fn init(ctx: &Context) -> FdwResult {
        Self::init_instance();
        let this = Self::this_mut();

        let opts = ctx.get_options(OptionsType::Server);
        this.base_url = opts.require_or("api_url", "https://api.revenuecat.com/v2");
        this.project_id = opts.require("project_id")?;

        // Resolve API key: direct value or Vault secret
        let api_key = match opts.get("api_key") {
            Some(key) => key,
            None => {
                let key_id = opts.require("api_key_id")?;
                utils::get_vault_secret(&key_id).unwrap_or_default()
            }
        };

        this.headers.push(("user-agent".to_owned(), "WordPuppi RevenueCat FDW".to_owned()));
        this.headers.push(("content-type".to_owned(), "application/json".to_owned()));
        this.headers.push(("authorization".to_owned(), format!("Bearer {}", api_key)));

        stats::inc_stats(FDW_NAME, stats::Metric::CreateTimes, 1);

        Ok(())
    }

    fn begin_scan(ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        let opts = ctx.get_options(OptionsType::Table);
        this.object = opts.require("object")?;

        this.url = None;
        this.make_request(ctx)?;

        Ok(())
    }

    fn iter_scan(ctx: &Context, row: &Row) -> Result<Option<u32>, FdwError> {
        let this = Self::this_mut();

        // If all buffered rows are consumed
        if this.src_idx >= this.src_rows.len() {
            stats::inc_stats(FDW_NAME, stats::Metric::RowsIn, this.src_rows.len() as i64);
            stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, this.src_rows.len() as i64);

            // No more pages — scan complete
            if this.url.is_none() {
                return Ok(None);
            }

            // Fetch next page
            this.make_request(ctx)?;

            // If next page returned no rows, we're done
            if this.src_rows.is_empty() {
                return Ok(None);
            }
        }

        // Map current row to PostgreSQL columns
        let src_row = &this.src_rows[this.src_idx];
        for tgt_col in ctx.get_columns() {
            let cell = this.src_to_cell(src_row, &tgt_col)?;
            row.push(cell.as_ref());
        }

        this.src_idx += 1;

        Ok(Some(0))
    }

    fn re_scan(ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        this.url = None;
        this.make_request(ctx)
    }

    fn end_scan(_ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        this.src_rows.clear();
        this.src_idx = 0;
        Ok(())
    }

    fn begin_modify(ctx: &Context) -> FdwResult {
        let this = Self::this_mut();
        let opts = ctx.get_options(OptionsType::Table);
        this.object = opts.require("object")?;
        this.rowid_col = opts.require("rowid_column")?;
        Ok(())
    }

    fn insert(_ctx: &Context, row: &Row) -> FdwResult {
        let this = Self::this_mut();

        match this.object.as_str() {
            "customers" => {
                // POST /v2/projects/{pid}/customers
                // Body: { "id": "...", "attributes": [...] }
                let url = format!(
                    "{}/projects/{}/customers",
                    this.base_url, this.project_id
                );
                let body = this.row_to_body(row)?;
                let req = http::Request {
                    method: http::Method::Post,
                    url,
                    headers: this.headers.clone(),
                    body,
                };
                let resp = http::post(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "entitlements" => {
                // POST /v2/projects/{pid}/entitlements
                // Body: { "lookup_key": "...", "display_name": "..." }
                let url = format!(
                    "{}/projects/{}/entitlements",
                    this.base_url, this.project_id
                );
                let body = this.row_to_body(row)?;
                let req = http::Request {
                    method: http::Method::Post,
                    url,
                    headers: this.headers.clone(),
                    body,
                };
                let resp = http::post(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "offerings" => {
                // POST /v2/projects/{pid}/offerings
                let url = format!(
                    "{}/projects/{}/offerings",
                    this.base_url, this.project_id
                );
                let body = this.row_to_body(row)?;
                let req = http::Request {
                    method: http::Method::Post,
                    url,
                    headers: this.headers.clone(),
                    body,
                };
                let resp = http::post(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "granted_entitlements" => {
                // Grant entitlement to a customer
                // POST /v2/projects/{pid}/customers/{cid}/actions/grant_entitlement
                // Body: { "entitlement_id": "...", "expires_at": ... }
                let cols = row.cols();
                let cells = row.cells();

                let customer_id = cols
                    .iter()
                    .zip(cells.iter())
                    .find(|(name, _)| name.as_str() == "customer_id")
                    .and_then(|(_, cell)| match cell {
                        Some(Cell::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .ok_or("'customer_id' column is required for granted_entitlements INSERT")?;

                let entitlement_id = cols
                    .iter()
                    .zip(cells.iter())
                    .find(|(name, _)| name.as_str() == "entitlement_id")
                    .and_then(|(_, cell)| match cell {
                        Some(Cell::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .ok_or(
                        "'entitlement_id' column is required for granted_entitlements INSERT",
                    )?;

                // Optional expires_at (milliseconds epoch)
                let expires_at = cols
                    .iter()
                    .zip(cells.iter())
                    .find(|(name, _)| name.as_str() == "expires_at")
                    .and_then(|(_, cell)| match cell {
                        Some(Cell::Timestamp(v)) => Some(v / 1_000), // microseconds to ms
                        Some(Cell::Timestamptz(v)) => Some(v / 1_000),
                        Some(Cell::I64(v)) => Some(*v),
                        _ => None,
                    });

                let url = format!(
                    "{}/projects/{}/customers/{}/actions/grant_entitlement",
                    this.base_url, this.project_id, customer_id
                );

                let mut body_map = JsonMap::new();
                body_map.insert(
                    "entitlement_id".to_owned(),
                    JsonValue::String(entitlement_id),
                );
                if let Some(exp) = expires_at {
                    body_map.insert("expires_at".to_owned(), JsonValue::Number(exp.into()));
                }

                let req = http::Request {
                    method: http::Method::Post,
                    url,
                    headers: this.headers.clone(),
                    body: JsonValue::Object(body_map).to_string(),
                };
                let resp = http::post(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            _ => {
                return Err(format!(
                    "INSERT is not supported for object '{}'",
                    this.object
                ));
            }
        }

        Ok(())
    }

    fn update(_ctx: &Context, _rowid: Cell, _row: &Row) -> FdwResult {
        Err("UPDATE is not supported — RevenueCat API v2 has no general PATCH endpoints".to_owned())
    }

    fn delete(_ctx: &Context, rowid: Cell) -> FdwResult {
        let this = Self::this_mut();

        let id = match &rowid {
            Cell::String(s) => s.clone(),
            _ => return Err("rowid must be a text column".to_owned()),
        };

        match this.object.as_str() {
            "customers" => {
                // DELETE /v2/projects/{pid}/customers/{id}
                let url = format!(
                    "{}/projects/{}/customers/{}",
                    this.base_url, this.project_id, id
                );
                let req = http::Request {
                    method: http::Method::Delete,
                    url,
                    headers: this.headers.clone(),
                    body: String::default(),
                };
                let resp = http::delete(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "entitlements" => {
                // DELETE /v2/projects/{pid}/entitlements/{id}
                let url = format!(
                    "{}/projects/{}/entitlements/{}",
                    this.base_url, this.project_id, id
                );
                let req = http::Request {
                    method: http::Method::Delete,
                    url,
                    headers: this.headers.clone(),
                    body: String::default(),
                };
                let resp = http::delete(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "offerings" => {
                // DELETE /v2/projects/{pid}/offerings/{id}
                let url = format!(
                    "{}/projects/{}/offerings/{}",
                    this.base_url, this.project_id, id
                );
                let req = http::Request {
                    method: http::Method::Delete,
                    url,
                    headers: this.headers.clone(),
                    body: String::default(),
                };
                let resp = http::delete(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            "granted_entitlements" => {
                // Revoke a granted entitlement
                // The rowid for granted_entitlements is a composite: "customer_id:entitlement_id"
                // POST /v2/projects/{pid}/customers/{cid}/actions/revoke_granted_entitlement
                let parts: Vec<&str> = id.splitn(2, ':').collect();
                if parts.len() != 2 {
                    return Err(
                        "granted_entitlements rowid must be 'customer_id:entitlement_id'"
                            .to_owned(),
                    );
                }
                let customer_id = parts[0];
                let entitlement_id = parts[1];

                let url = format!(
                    "{}/projects/{}/customers/{}/actions/revoke_granted_entitlement",
                    this.base_url, this.project_id, customer_id
                );

                let mut body_map = JsonMap::new();
                body_map.insert(
                    "entitlement_id".to_owned(),
                    JsonValue::String(entitlement_id.to_owned()),
                );

                let req = http::Request {
                    method: http::Method::Post,
                    url,
                    headers: this.headers.clone(),
                    body: JsonValue::Object(body_map).to_string(),
                };
                let resp = http::post(&req)?;
                http::error_for_status(&resp)
                    .map_err(|err| format!("{}: {}", err, resp.body))?;
                stats::inc_stats(FDW_NAME, stats::Metric::RowsOut, 1);
            }
            _ => {
                return Err(format!(
                    "DELETE is not supported for object '{}'",
                    this.object
                ));
            }
        }

        Ok(())
    }

    fn end_modify(_ctx: &Context) -> FdwResult {
        Ok(())
    }
}

bindings::export!(RevenueCatFdw with_types_in bindings);
