//! SQL-backed print queue and station coordination API. PDF rendering and
//! physical printing live in the native client under `client/`.

pub mod auth;
pub mod config;
pub mod handlers;
pub mod jobs;
pub mod lifecycle;
pub mod models;
pub mod schema;
pub mod stations;

#[cfg(target_arch = "wasm32")]
mod entry {
    use broccoli_server_sdk::prelude::*;
    use extism_pdk::{FnResult, plugin_fn};

    use crate::{handlers, schema};

    #[plugin_fn]
    pub fn init() -> FnResult<String> {
        let host = Host::new();
        schema::create_tables(&host)?;
        host.log
            .info("print plugin: print_job / print_station ready")?;
        Ok("ok".into())
    }

    macro_rules! http_entry {
        ($name:ident => $handler:path) => {
            #[plugin_fn]
            pub fn $name(input: String) -> FnResult<String> {
                run_api_handler(&input, $handler)
            }
        };
    }

    // Contestant endpoints.
    http_entry!(api_print_submission => handlers::handle_print_submission);
    http_entry!(api_print_arbitrary => handlers::handle_print_arbitrary);
    http_entry!(api_my_jobs => handlers::handle_my_jobs);

    // Staff endpoints, permission-gated by the server before dispatch.
    http_entry!(api_admin_list_jobs => handlers::handle_admin_list_jobs);
    http_entry!(api_admin_get_job => handlers::handle_admin_get_job);
    http_entry!(api_admin_approve => handlers::handle_admin_approve);
    http_entry!(api_admin_reprint => handlers::handle_admin_reprint);
    http_entry!(api_admin_cancel => handlers::handle_admin_cancel);
    http_entry!(api_admin_pin => handlers::handle_admin_pin);
    http_entry!(api_admin_stations => handlers::handle_admin_stations);

    // Station endpoints, token validated in-handler.
    http_entry!(api_station_heartbeat => handlers::handle_station_heartbeat);
    http_entry!(api_station_jobs => handlers::handle_station_jobs);
    http_entry!(api_station_claim => handlers::handle_station_claim);
    http_entry!(api_station_status => handlers::handle_station_status);
}
