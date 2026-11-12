//! `print_station` persistence.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::prelude::Params;

use crate::lifecycle::bind_opt;
use crate::models::{HeartbeatRequest, StationRow};

/// A station counts as online if it checked in within this window.
const ONLINE_WINDOW_SECS: i64 = 45;

pub fn heartbeat(host: &Host, station: &str, req: &HeartbeatRequest) -> Result<(), SdkError> {
    let printers_json = serde_json::to_string(&req.printers).unwrap_or_else(|_| "[]".to_string());
    let mut p = Params::new();
    let sql = format!(
        "INSERT INTO print_station (name, location, printers, version, queue_seen, last_seen) \
         VALUES ({}, {}, {}::jsonb, {}, {}, NOW()) \
         ON CONFLICT (name) DO UPDATE SET \
            location = EXCLUDED.location, \
            printers = EXCLUDED.printers, \
            version = EXCLUDED.version, \
            queue_seen = EXCLUDED.queue_seen, \
            last_seen = NOW()",
        p.bind(station.to_string()),
        bind_opt(&mut p, req.location.clone()),
        p.bind(printers_json),
        bind_opt(&mut p, req.version.clone()),
        bind_opt(&mut p, req.queue_seen),
    );
    host.db.execute_with_args(&sql, &p.into_args())?;
    Ok(())
}

pub fn list_stations(host: &Host) -> Result<Vec<StationRow>, SdkError> {
    let sql = format!(
        "SELECT name, location, printers, version, queue_seen, \
            EXTRACT(EPOCH FROM last_seen) AS last_seen, \
            (NOW() - last_seen) < INTERVAL '{ONLINE_WINDOW_SECS} seconds' AS online \
         FROM print_station ORDER BY last_seen DESC"
    );
    host.db.query(&sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn heartbeat_upserts_station() {
        let host = Host::mock();
        host.db.queue_execute_result(1);
        let req = HeartbeatRequest {
            printers: vec!["main".into(), "overflow".into()],
            location: Some("Room A".into()),
            version: Some("0.1.0".into()),
            queue_seen: Some(3),
        };
        heartbeat(&host, "room-a", &req).unwrap();
        let sql = &host.db.executions()[0].sql;
        assert!(sql.contains("INSERT INTO print_station"));
        assert!(sql.contains("ON CONFLICT (name) DO UPDATE"));
        assert!(sql.contains("::jsonb"));
    }

    #[test]
    fn list_stations_computes_online() {
        let host = Host::mock();
        host.db.queue_query_result(json!([
            { "name": "a", "printers": ["main"], "online": true, "last_seen": 1.0 },
            { "name": "b", "printers": [], "online": false, "last_seen": 2.0 }
        ]));
        let rows = list_stations(&host).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].online);
        assert!(!rows[1].online);
    }
}
