# Configuration Reference

A contest needs only what a node cannot work out for itself: where the
database, Redis and object storage are, and the secrets. `install.sh` asks for
those and writes them to the role's env file. Everything else is derived from
the machine and the request, or has a default that suits a contest.

Every setting reads from `config/config.toml` as `section.key` or from the
environment as `BROCCOLI__SECTION__KEY`. In a Compose deployment, add an
override to the role's env file (`.env.server`, `.env.worker`, ...) and
restart the role: the whole env file reaches the container. A single-host
install keeps every secret in `.env.single-host`, which no container gets whole;
its overrides go in `.env.single-host.server` and `.env.single-host.worker`.
A key no Broccoli binary reads, for example one that has been removed or is
misspelled, is logged at boot as `Ignoring an unknown configuration key` and
has no effect. A server key given to a worker, or the other way round, is
ignored without a warning, so one config file can serve both.

## Required

| Setting | Server | Worker | What it is |
| --- | --- | --- | --- |
| `database.url` | yes | yes | Postgres URL |
| `mq.url` | yes | yes | Redis URL |
| `auth.jwt_secret` | yes | | Token signing key, at least 32 bytes |
| `bootstrap.admin_username`, `bootstrap.admin_password` | first boot | | The first admin account |
| `storage.backend` | yes | yes | `object_storage`, `database` or `filesystem` |
| `storage.object_storage.endpoint`, `.access_key`, `.secret_key`, `.bucket` | object storage | object storage | The S3 store |

## Derived

These have no setting.

| What | How |
| --- | --- |
| Wall time (contest phases, token expiry, rate limits, `Date`) | The database clock, sampled every 30 to 60 s |
| Server database pools | Handlers: 4 per CPU, 16 to 40. Plugin SQL: one per plugin CPU token, 4 to 16. Plugin writes: one per two tokens, 2 to 8. Shrunk at boot to the server's share of `max_connections` (see Database connections in the runbook) |
| Worker database pool | `worker.max_concurrency` + 2 |
| Claim loop | Notification driven, with a 1 s fallback tick and 32 rows per statement |
| In-flight cap per server | 8 per live worker slot, at least 256 |
| Queue cap (queued units at which new submissions get 503) | 200 per live worker slot, at least 5000 |
| `Secure` session cookie | Set when the request arrived over HTTPS, as a trusted proxy reports it |
| Log format | `pretty` on a terminal, JSON otherwise |
| Server and worker IDs | From the machine ID (`install.sh` writes them) |

## Advanced overrides

Leave these alone unless a measurement says otherwise.

### Server (`server.*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `host`, `port` | `127.0.0.1`, `3000` | Listen address |
| `public_base_url` | from the request | Origin for links the server hands out |
| `trusted_proxies` | none | CIDRs whose `X-Forwarded-*` headers are trusted (`install.sh` sets the gateway) |
| `cors.allow_origins`, `cors.max_age` | none, `3600` | Cross-origin frontend |
| `rate_limit_auth` | `true` | Per-IP limit of 10 logins a minute on `/auth/login`; turn off only behind a NAT that shows the server one address for many contestants |
| `max_queued_submissions` | derived | A fixed queue cap; `0` turns it off |
| `dispatcher_semaphore_enabled`, `dispatcher_concurrency`, `dispatcher_admission_queue_max` | `true`, `16`, `1000` | In-process dispatch admission; the queue is above the in-flight floor so the in-flight cap governs |
| `dispatcher_lease_steal_enabled` | `true` | Servers take over the in-flight work of a server that stopped renewing its leases |
| `lease_ttl_secs`, `lease_refresh_interval_secs`, `steal_scan_interval_secs`, `steal_batch_size` | `60`, `10`, `15`, `8` | Lease recovery timing |
| `max_dispatch_retries`, `max_system_error_retries` | `5`, `10` | Retry budgets |
| `cancel_primitive_enabled` | `false` | Redis-backed operation cancellation |
| `claim_fiber_enabled` | `true` | Turn off to stop a server from dispatching while debugging |
| `plugin_timer_tick_interval_secs`, `plugin_timer_batch`, `plugin_timer_max_attempts` | `1`, `64`, `5` | Plugin timer delivery |
| `batch_max_age_secs` (top level) | `7200` | Age at which an unfinished evaluate batch is reaped |

### Database (`database.*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `plugin_url` | provisioned | A DBA-managed login for raw plugin SQL |
| `statement_timeout_secs` | `60` | Per-statement timeout; `0` turns it off |
| `tcp_keepalives` | `true` | Behind a PgBouncer that rejects startup options, set this to `false` and `statement_timeout_secs` to `0` |

### Accounts and submissions

| Setting | Default | Effect |
| --- | --- | --- |
| `auth.login_failure_limit`, `auth.login_failure_window_secs` | `10`, `60` | Failed logins per user and IP before a short throttle |
| `bootstrap.reset_admin` | `false` | Reset the bootstrap admin on the next boot (see the runbook) |
| `submission.max_size` | 2 MiB | Largest source upload |
| `submission.rate_limit_per_minute` | `600` | Submissions and code runs per user per minute, an abuse guard nobody typing reaches; contest attempt limits belong to the `submission-limit` and `cooldown` plugins; `0` turns it off |

### Queue (`mq.*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `enabled` | `true` | Run without Redis (development only) |
| `operation_queue_name` | `operation_tasks` | Prefix of every queue key |
| `dlq.max_retries`, `dlq.base_delay_ms`, `dlq.max_delay_ms` | `3`, `1000`, `60000` | Dead-letter retry policy |
| `dlq.stuck_job_timeout_secs`, `dlq.stuck_job_scan_interval_secs`, `dlq.max_inflight_secs` | `21600`, `60`, `3600` | Stuck-job detection |
| `dlq.retry_cleanup_interval_secs`, `dlq.retry_max_age_secs` | `300`, `7200` | Retry bookkeeping |

### Storage (`storage.*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `data_dir` | `./data` | Filesystem backend root |
| `max_blob_size` | 2 GiB | Largest stored blob |
| `object_storage.region`, `object_storage.path_style`, `object_storage.temp_dir` | `us-east-1`, `false`, system temp | S3 details (`install.sh` sets `path_style` for SeaweedFS) |

### Worker (`worker.*`, `storage.cache_*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `max_concurrency` | `1` | Keep at 1: see Capacity Planning in the runbook. Scale by adding worker machines |
| `sandbox_backend`, `isolate_bin`, `enable_cgroups`, `allow_insecure_no_cgroups` | `isolate`, `isolate`, `true`, `false` | Sandbox |
| `box_id_lock_dir` | system temp | One lock file per isolate box id; every worker container on one host must share it |
| `isolate_config` | `/usr/local/etc/isolate` | Read for `num_boxes`, the box ids every worker on the host shares |
| `metrics_addr` | `0.0.0.0:9091` | Prometheus `/metrics` bind; one port per worker on a host |
| `tools.dir` | none | Platform tools such as `broccoli-compare` |
| `cache_leader_election_enabled`, `cache_leader_ttl_secs`, `cache_leader_heartbeat_interval_secs`, `cache_follower_poll_interval_ms`, `cache_follower_max_wait_secs` | `true`, `60`, `5`, `250`, `30` | Test data download coordination between workers on one host |
| `storage.cache_dir`, `storage.max_cache_size` | `./data/cache`, 32 GiB | Local test data cache |

### Logs and tracing (`observability.*`)

| Setting | Default | Effect |
| --- | --- | --- |
| `log_format` | `auto` | `json` or `pretty` to force one |
| `log_filter` | `info` | `tracing` filter; `RUST_LOG` wins when set |
| `otlp.endpoint`, `otlp.service_name` | off, per binary | OpenTelemetry export |

Plugin settings (`plugin.*`) are described in the runbook's plugin pool
section.
