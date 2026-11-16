//! The station protocol end to end: this client's [`ServerClient`] against the
//! print plugin's own station handlers, routed by the plugin's `plugin.toml`.
//! The plugin runs on the SDK's mock host, so the database answers what each
//! test queues, while auth, paths, query strings and bodies are the real ones.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

use broccoli_server_sdk::Host;
use broccoli_server_sdk::prelude::*;
use print_plugin::handlers;
use serde_json::{Value, json};

use crate::api::{ClaimOutcome, ServerClient};
use crate::config::ServerCfg;

const MANIFEST: &str = include_str!("../../plugin.toml");

/// The host serves a plugin's routes under this prefix.
const PROXY_PREFIX: &str = "/api/v1/p/print";

type Handler = fn(&Host, &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError>;

fn handler(name: &str) -> Handler {
    match name {
        "api_station_heartbeat" => handlers::handle_station_heartbeat,
        "api_station_jobs" => handlers::handle_station_jobs,
        "api_station_claim" => handlers::handle_station_claim,
        "api_station_status" => handlers::handle_station_status,
        other => panic!("the client called {other}, which is not a station route"),
    }
}

struct Route {
    method: String,
    segments: Vec<String>,
    handler: String,
}

fn routes() -> Vec<Route> {
    let manifest: toml::Value = toml::from_str(MANIFEST).unwrap();
    manifest["server"]["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Route {
            method: r["method"].as_str().unwrap().to_string(),
            segments: r["path"]
                .as_str()
                .unwrap()
                .split('/')
                .map(String::from)
                .collect(),
            handler: r["handler"].as_str().unwrap().to_string(),
        })
        .collect()
}

/// The manifest route `method path` matches, with its path parameters.
fn resolve<'a>(
    routes: &'a [Route],
    method: &str,
    path: &str,
) -> Option<(&'a Route, HashMap<String, String>)> {
    let segments: Vec<&str> = path.split('/').collect();
    routes.iter().find_map(|route| {
        if route.method != method || route.segments.len() != segments.len() {
            return None;
        }
        let mut params = HashMap::new();
        for (pattern, actual) in route.segments.iter().zip(&segments) {
            match pattern.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(name) => {
                    params.insert(name.to_string(), actual.to_string());
                }
                None if pattern == actual => {}
                None => return None,
            }
        }
        Some((route, params))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

/// One HTTP/1.1 request off `stream`, as the plugin proxy hands it over.
fn read_request(routes: &[Route], stream: &TcpStream) -> (Handler, PluginHttpRequest) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap().to_string();
    let target = parts.next().unwrap().to_string();

    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let body = match headers.get("content-length") {
        Some(len) => {
            let mut buf = vec![0; len.parse().unwrap()];
            reader.read_exact(&mut buf).unwrap();
            Some(serde_json::from_slice::<Value>(&buf).unwrap())
        }
        None => None,
    };

    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let path = path
        .strip_prefix(PROXY_PREFIX)
        .unwrap_or_else(|| panic!("{path} is outside the plugin's proxy prefix"));
    let query = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    let (route, params) = resolve(routes, &method, path)
        .unwrap_or_else(|| panic!("no plugin route for {method} {path}"));
    let request = PluginHttpRequest {
        method,
        path: path.to_string(),
        params,
        query,
        headers,
        body,
        auth: None,
        contest: None,
        problem_id: None,
    };
    (handler(&route.handler), request)
}

/// Serves `requests` station requests from `host`, then hands `host` back.
struct Plugin {
    url: String,
    thread: JoinHandle<Host>,
}

impl Plugin {
    fn serve(host: Host, requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let routes = routes();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let (handler, request) = read_request(&routes, &stream);
                let response = handler(&host, &request).unwrap_or_else(|e| e.into_response());
                let body = serde_json::to_vec(&response.body.unwrap_or(Value::Null)).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
            host
        });
        Self { url, thread }
    }

    fn finish(self) -> Host {
        self.thread.join().unwrap()
    }
}

/// A host whose plugin-wide and contest 7 `print` configs list `global` and
/// `contest` as their stations. The mock host reads schema defaults from the
/// `plugin.toml` of the crate under test, which is this client, so each
/// config is stored whole.
fn host_with_stations(global: Value, contest: Value) -> Host {
    let config = |stations: Value| {
        json!({
            "enabled": true, "require_approval": false, "max_pages": 10,
            "allow_arbitrary": false, "max_jobs_per_user": 20, "banner": "",
            "stations": stations,
        })
    };
    let host = Host::mock();
    host.config.seed("plugin", "", "print", config(global));
    host.config.seed("contest", "7", "print", config(contest));
    host
}

fn queued_job(id: i64) -> Value {
    json!({
        "id": id, "contest_id": 7, "username": "alice", "language": "cpp",
        "filename": "a.cpp", "source": "int main(){}", "created_at": 1.0,
    })
}

/// A station runs one tick's worth of the protocol and returns the name the
/// server knows it by.
fn run_station(host: Host, server: ServerCfg) -> (String, Host) {
    // the heartbeat's upsert, the listing, the claim and its two reports
    host.db.queue_query_result(json!([queued_job(1)]));
    for _ in 0..4 {
        host.db.queue_execute_result(1);
    }
    let plugin = Plugin::serve(host, 5);
    let client = ServerClient::new(&ServerCfg {
        url: plugin.url.clone(),
        ..server
    });

    let name = client.heartbeat(&["main".to_string()], 0).unwrap();
    let jobs = client.fetch_jobs(4).unwrap();
    assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [1]);
    assert_eq!(client.claim(1, "main").unwrap(), ClaimOutcome::Claimed);
    client.report(1, "printing", None, None).unwrap();
    client.report(1, "done", Some(1), None).unwrap();
    (name, plugin.finish())
}

#[test]
fn a_global_station_runs_the_whole_protocol() {
    let host = host_with_stations(json!([{ "name": "hall", "token": "G" }]), json!([]));
    let server = ServerCfg {
        url: String::new(),
        token: "G".into(),
        contest: None,
    };
    let (name, _) = run_station(host, server);
    assert_eq!(name, "global/hall");
}

#[test]
fn a_contest_station_runs_the_whole_protocol_inside_its_contest() {
    let host = host_with_stations(json!([]), json!([{ "name": "room-7", "token": "C7" }]));
    let server = ServerCfg {
        url: String::new(),
        token: "C7".into(),
        contest: Some(7),
    };
    let (name, host) = run_station(host, server);
    assert_eq!(name, "contest-7/room-7");
    let confined = |sql: &str| sql.contains("contest_id = $");
    assert!(host.db.queries().iter().all(|q| confined(&q.sql)));
    assert!(host.db.executions()[1..].iter().all(|e| confined(&e.sql)));
}

#[test]
fn a_contest_station_that_does_not_name_its_contest_is_refused() {
    let host = host_with_stations(json!([]), json!([{ "name": "room-7", "token": "C7" }]));
    let plugin = Plugin::serve(host, 1);
    let client = ServerClient::new(&ServerCfg {
        url: plugin.url.clone(),
        token: "C7".into(),
        contest: None,
    });
    let err = client.heartbeat(&[], 0).unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
    plugin.finish();
}
