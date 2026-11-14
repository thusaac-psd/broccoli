#!/usr/bin/env python3
"""Run the judging-throughput matrix and print a table.

Starts the requested server/worker topologies from release binaries against an
existing Postgres and Redis, drives each point with
`broccoli-stress-test perf`, samples per-process CPU from /proc and container
CPU through `docker exec`, and writes one JSON per run plus `summary.md`.

Example (mock sandbox, see docs/internals/performance.md):

    python3 scripts/perf/judging-matrix.py \
        --topologies 1x1,1x4,2x4 --test-cases 1,10,100 \
        --concurrency 1,8,32,128 --submissions 100 --repeats 3 \
        --out-dir perf-out/master

A topology `SxW` is S servers and W workers on this host. Every worker uses
`--worker-concurrency` slots. Extra settings go through `--server-env` and
`--worker-env` (`KEY=VALUE`, repeatable), e.g. to prove a bottleneck by
moving one knob.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import statistics
import subprocess
import sys
import time
import tomllib
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CLK_TCK = os.sysconf("SC_CLK_TCK")
# The plugins a judged ICPC or IOI batch submission passes through.
JUDGING_PLUGINS = ("icpc", "ioi", "batch-evaluator", "standard-checkers", "standard-languages")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bin-dir", default=str(ROOT / "target/release"))
    p.add_argument("--db-url", default="postgres://broccoli:broccoli_app_secret@127.0.0.1:5433/broccoli",
                   help="the app role's URL: a LOGIN CREATEROLE role, as docker-compose.yaml creates")
    p.add_argument("--db-admin-url", default="postgres://postgres:broccoli_pg_secret@127.0.0.1:5433/postgres",
                   help="used to recreate the database when --fresh-db is set")
    p.add_argument("--redis-url", default="redis://127.0.0.1:6379")
    p.add_argument("--plugins-source", default=str(ROOT / "plugins"),
                   help="directory holding the built plugins the run copies from")
    p.add_argument("--plugins", default=",".join(JUDGING_PLUGINS),
                   help="plugins the servers load; any other enabled plugin stays out of the run")
    p.add_argument("--topologies", default="1x1")
    p.add_argument("--test-cases", default="1")
    p.add_argument("--concurrency", default="1")
    p.add_argument("--submissions", type=int, default=100,
                   help="per point; scaled down for large test-case counts by --max-test-cases-per-point")
    p.add_argument("--max-test-cases-per-point", type=int, default=1500)
    p.add_argument("--min-submissions", type=int, default=10)
    p.add_argument("--repeats", type=int, default=3)
    p.add_argument("--contest-type", default="icpc")
    p.add_argument("--scenario", default="ab-cpp-ac")
    p.add_argument("--worker-concurrency", type=int, default=1)
    p.add_argument("--sandbox-backend", default="mock")
    p.add_argument("--tools-dir", default="/tools",
                   help="worker tools dir; must be /tools with the mock sandbox")
    p.add_argument("--poll-interval-ms", type=int, default=50,
                   help="client poll interval at concurrency 1 (latency points)")
    p.add_argument("--load-poll-interval-ms", type=int, default=1000,
                   help="client poll interval at concurrency > 1, so polling does not "
                        "dominate server and database load")
    p.add_argument("--idle-secs", type=int, default=0)
    p.add_argument("--server-env", action="append", default=[])
    p.add_argument("--worker-env", action="append", default=[])
    p.add_argument("--pg-container", default="bpg")
    p.add_argument("--redis-container", default="bredis")
    p.add_argument("--fresh-db", action="store_true", help="drop and recreate the database per topology")
    p.add_argument("--flush-redis", action="store_true", help="FLUSHALL before each topology")
    p.add_argument("--out-dir", default="perf-out")
    p.add_argument("--label", default="")
    p.add_argument("--admin-username", default="perfadmin")
    p.add_argument("--admin-password", default="perf-admin-password-123")
    return p.parse_args()


def kv(items: list[str]) -> dict[str, str]:
    out = {}
    for it in items:
        k, _, v = it.partition("=")
        out[k] = v
    return out


def proc_cpu_s(pid: int) -> float:
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / CLK_TCK
    except (OSError, IndexError, ValueError):
        return 0.0


def container_cpu_s(name: str) -> float:
    cmd = ("cat /sys/fs/cgroup/cpu.stat 2>/dev/null | awk '/usage_usec/{print $2/1e6}' "
           "|| true; cat /sys/fs/cgroup/cpuacct/cpuacct.usage 2>/dev/null | awk '{print $1/1e9}'")
    try:
        out = subprocess.run(["docker", "exec", name, "sh", "-c", cmd], capture_output=True, text=True,
                             timeout=10).stdout.split()
        return float(out[0]) if out else 0.0
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return 0.0


def wait_http(url: str, timeout: float = 120.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2):
                return
        except Exception:
            time.sleep(0.5)
    raise RuntimeError(f"timed out waiting for {url}")


def stage_plugins(source: Path, names: list[str], dest: Path) -> Path:
    """Copy the named built plugins into a fresh `dest` and return it.

    The servers load every plugin in their plugins directory. A plugin outside
    the judging path changes what is measured, and one that is enabled but not
    built can fail closed on every request it guards, so the run loads only
    the named plugins.
    """
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    for name in names:
        src = source / name
        manifest = src / "plugin.toml"
        if not manifest.is_file():
            sys.exit(f"no plugin manifest at {manifest}")
        entry = tomllib.loads(manifest.read_text()).get("server", {}).get("entry")
        if entry and not (src / entry).is_file():
            sys.exit(f"plugin {name} is not built ({src / entry} is missing); run ./scripts/build-plugins.sh")
        shutil.copytree(src, dest / name, ignore=shutil.ignore_patterns("target", "node_modules"))
    return dest


def psql(url: str, sql: str) -> None:
    subprocess.run(["psql", url, "-v", "ON_ERROR_STOP=1", "-qc", sql], check=True)


class Topology:
    def __init__(self, a: argparse.Namespace, servers: int, workers: int, logdir: Path):
        self.a, self.servers, self.workers, self.logdir = a, servers, workers, logdir
        self.procs: list[tuple[str, subprocess.Popen]] = []

    def base_env(self) -> dict[str, str]:
        env = dict(os.environ)
        env.update({
            "BROCCOLI__DATABASE__URL": self.a.db_url,
            "BROCCOLI__MQ__URL": self.a.redis_url,
            "BROCCOLI__MQ__ENABLED": "true",
            "BROCCOLI__OBSERVABILITY__LOG_FORMAT": "json",
            "BROCCOLI__OBSERVABILITY__LOG_FILTER": "warn",
            "BROCCOLI_CONFIG": "/nonexistent/perf",
        })
        return env

    def start(self) -> None:
        self.logdir.mkdir(parents=True, exist_ok=True)
        for i in range(self.servers):
            env = self.base_env()
            env.update({
                "BROCCOLI__SERVER__HOST": "127.0.0.1",
                "BROCCOLI__SERVER__PORT": str(3000 + i),
                "BROCCOLI__SERVER__ID": f"perf-server-{i}",
                "BROCCOLI__AUTH__JWT_SECRET": "perf-jwt-secret-perf-jwt-secret-perf-jwt-secret",
                "BROCCOLI__BOOTSTRAP__ADMIN_USERNAME": self.a.admin_username,
                "BROCCOLI__BOOTSTRAP__ADMIN_PASSWORD": self.a.admin_password,
                # Keep the per-submit rate-limit queries on the path, but never trip it.
                "BROCCOLI__SUBMISSION__RATE_LIMIT_PER_MINUTE": "100000000",
                "BROCCOLI__SERVER__MAX_QUEUED_SUBMISSIONS": "0",
                # Every point logs in from this host; the per-IP login limit
                # would refuse the eleventh point within a minute.
                "BROCCOLI__SERVER__RATE_LIMIT_AUTH": "false",
                "BROCCOLI__PLUGIN__PLUGINS_DIR": str(self.a.plugins_dir),
            })
            env.update(kv(self.a.server_env))
            self._spawn(f"server-{i}", [f"{self.a.bin_dir}/server"], env)
            # Serialise boots: the first one runs migrations and plugin sync.
            wait_http(f"http://127.0.0.1:{3000 + i}/metrics", 300)
        for i in range(self.workers):
            env = self.base_env()
            env.update({
                "BROCCOLI__WORKER__ID": f"perf-worker-{i}",
                "BROCCOLI__WORKER__SANDBOX_BACKEND": self.a.sandbox_backend,
                "BROCCOLI__WORKER__MAX_CONCURRENCY": str(self.a.worker_concurrency),
                "BROCCOLI__WORKER__METRICS_ADDR": f"127.0.0.1:{9100 + i}",
                "BROCCOLI__STORAGE__CACHE_DIR": str(self.logdir / f"cache-{i}"),
                "BROCCOLI__STORAGE__DATA_DIR": str(self.logdir / f"data-{i}"),
            })
            # The mock sandbox runs argv on the host and does not remap
            # `/tools/...`, so the tools dir has to be `/tools` itself.
            tools = Path(self.a.tools_dir)
            tools.mkdir(parents=True, exist_ok=True)
            compare = tools / "broccoli-compare"
            if not compare.exists() or compare.stat().st_size != Path(self.a.bin_dir, "broccoli-compare").stat().st_size:
                compare.write_bytes(Path(self.a.bin_dir, "broccoli-compare").read_bytes())
                compare.chmod(0o755)
            env["BROCCOLI__WORKER__TOOLS__DIR"] = str(tools)
            env.update(kv(self.a.worker_env))
            self._spawn(f"worker-{i}", [f"{self.a.bin_dir}/worker"], env)
        for i in range(self.workers):
            wait_http(f"http://127.0.0.1:{9100 + i}/metrics", 120)
        time.sleep(3)  # first heartbeats

    def _spawn(self, name: str, cmd: list[str], env: dict[str, str]) -> None:
        log = open(self.logdir / f"{name}.log", "ab")
        self.procs.append((name, subprocess.Popen(cmd, env=env, stdout=log, stderr=log, cwd=str(ROOT))))

    def cpu(self) -> dict[str, float]:
        out: dict[str, float] = {}
        for name, p in self.procs:
            role = name.split("-")[0]
            out[role] = out.get(role, 0.0) + proc_cpu_s(p.pid)
        out["postgres"] = container_cpu_s(self.a.pg_container)
        out["redis"] = container_cpu_s(self.a.redis_container)
        return out

    def alive(self) -> None:
        dead = [n for n, p in self.procs if p.poll() is not None]
        if dead:
            raise RuntimeError(f"process exited: {dead}; see {self.logdir}")

    def stop(self) -> None:
        for _, p in self.procs:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        for _, p in self.procs:
            try:
                p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                p.kill()
        self.procs.clear()


def run_point(a, topo: Topology, tc: int, conc: int, rep: int, out: Path) -> dict:
    # Enough submissions to fill every client a few times, capped by the
    # test cases one point may judge, never fewer than 2 per client.
    subs = min(a.submissions, max(conc * 4, a.min_submissions))
    subs = max(conc * 2, min(subs, max(1, a.max_test_cases_per_point // tc)))
    servers = ",".join(f"http://127.0.0.1:{3000 + i}" for i in range(topo.servers))
    metrics = [f"http://127.0.0.1:{3000 + i}/metrics" for i in range(topo.servers)]
    metrics += [f"http://127.0.0.1:{9100 + i}/metrics" for i in range(topo.workers)]
    path = out / f"{topo.servers}x{topo.workers}-tc{tc}-c{conc}-r{rep}.json"
    cmd = [f"{a.bin_dir}/broccoli-stress-test", "perf",
           "--server-url", servers,
           "--admin-username", a.admin_username, "--admin-password", a.admin_password,
           "--contest-type", a.contest_type, "--scenario", a.scenario,
           "--test-cases", str(tc), "--submissions", str(subs), "--concurrency", str(conc),
           "--poll-interval-ms", str(a.poll_interval_ms if conc == 1 else a.load_poll_interval_ms), "--idle-secs", str(a.idle_secs),
           "--database-url", a.db_url, "--redis-url", a.redis_url,
           "--metrics-url", ",".join(metrics), "--out", str(path)]
    cpu0 = topo.cpu()
    t0 = time.time()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.time() - t0
    cpu1 = topo.cpu()
    (out / (path.stem + ".txt")).write_text(proc.stdout + proc.stderr)
    topo.alive()
    if not path.exists():
        raise RuntimeError(f"perf failed ({proc.returncode}): {proc.stderr[-2000:]}")
    r = json.loads(path.read_text())
    n = max(1, r["completed"])
    # CPU includes warmup and idle sampling; normalise by the measured window share.
    share = r["wall_s"] / wall if wall > 0 else 1.0
    r["cpu_s_per_submission"] = {k: (cpu1[k] - cpu0.get(k, 0.0)) * share / n for k in cpu1}
    r["cpu_util_cores"] = {k: (cpu1[k] - cpu0.get(k, 0.0)) / wall for k in cpu1}
    r["exit_code"] = proc.returncode
    path.write_text(json.dumps(r, indent=2))
    return r


def g(d: dict, *ks, default=float("nan")):
    for k in ks:
        if not isinstance(d, dict) or k not in d or d[k] is None:
            return default
        d = d[k]
    return d


def spread(vals: list[float]) -> str:
    vals = [v for v in vals if v == v]
    if not vals:
        return "n/a"
    m = statistics.median(vals)
    if len(vals) == 1:
        return f"{m:.2f}"
    return f"{m:.2f} ({min(vals):.2f}-{max(vals):.2f})"


def summarize(rows: list[tuple[str, int, int, list[dict]]], label: str) -> str:
    lines = [f"# Judging throughput {label}".rstrip(), "",
             "Median of repeats, (min-max) in brackets. Latencies in ms.", "",
             "| topo | tc | conc | subs/s | tc/s | e2e p50 | e2e p95 | e2e p99 | submit p50 | "
             "queued p50 | judge p50 | SQL/sub | SQL/tc | Redis/sub | Redis/tc | srv CPU ms/sub | "
             "wrk CPU ms/sub | pg CPU ms/sub |",
             "|" + "---|" * 18]
    for topo, tc, conc, rs in rows:
        def col(*ks, scale=1.0):
            return spread([g(r, *ks) * scale for r in rs])
        lines.append("| " + " | ".join([
            topo, str(tc), str(conc),
            col("client_throughput", "subs_per_s"), col("client_throughput", "test_cases_per_s"),
            col("e2e_latency_ms", "p50"), col("e2e_latency_ms", "p95"), col("e2e_latency_ms", "p99"),
            col("submit_latency_ms", "p50"),
            col("stages_ms", "queued", "p50"),
            spread([g(r, "stages_ms", "total", "p50") - g(r, "stages_ms", "queued", "p50") for r in rs]),
            col("db", "statements_per_submission"), col("db", "statements_per_test_case"),
            col("redis", "commands_per_submission"), col("redis", "commands_per_test_case"),
            col("cpu_s_per_submission", "server", scale=1e3),
            col("cpu_s_per_submission", "worker", scale=1e3),
            col("cpu_s_per_submission", "postgres", scale=1e3),
        ]) + " |")
    return "\n".join(lines) + "\n"


def main() -> int:
    a = parse_args()
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    hw = {
        "nproc": os.cpu_count(),
        "mem": Path("/proc/meminfo").read_text().splitlines()[0],
        "cpu": next((l.split(":", 1)[1].strip() for l in Path("/proc/cpuinfo").read_text().splitlines()
                     if l.startswith("model name")), "?"),
        "git": subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True,
                              text=True).stdout.strip(),
        "args": vars(a),
    }
    (out / "environment.json").write_text(json.dumps(hw, indent=2))
    a.plugins_dir = stage_plugins(Path(a.plugins_source), [n for n in a.plugins.split(",") if n],
                                  out.resolve() / "plugins")
    rows = []
    for topo_s in a.topologies.split(","):
        s, w = (int(x) for x in topo_s.lower().split("x"))
        if a.fresh_db:
            db = a.db_url.rsplit("/", 1)[1]
            psql(a.db_admin_url, f'DROP DATABASE IF EXISTS "{db}" WITH (FORCE)')
            owner = urllib.parse.urlsplit(a.db_url).username
            psql(a.db_admin_url, f'CREATE DATABASE "{db}" OWNER "{owner}"')
            # The app role cannot create the statement statistics extension.
            psql(a.db_admin_url.rsplit("/", 1)[0] + f"/{db}", "CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
        if a.flush_redis:
            subprocess.run(["redis-cli", "-u", a.redis_url, "FLUSHALL"], check=True, capture_output=True)
        topo = Topology(a, s, w, out / f"logs-{topo_s}")
        try:
            topo.start()
            for tc in (int(x) for x in a.test_cases.split(",")):
                for conc in (int(x) for x in a.concurrency.split(",")):
                    rs = []
                    for rep in range(a.repeats):
                        r = run_point(a, topo, tc, conc, rep, out)
                        rs.append(r)
                        print(f"{topo_s} tc={tc} c={conc} r={rep}: "
                              f"{g(r, 'client_throughput', 'subs_per_s'):.2f} subs/s "
                              f"e2e p50 {g(r, 'e2e_latency_ms', 'p50'):.0f} ms "
                              f"SQL/sub {g(r, 'db', 'statements_per_submission'):.1f}", flush=True)
                    rows.append((topo_s, tc, conc, rs))
                    (out / "summary.md").write_text(summarize(rows, a.label))
        finally:
            topo.stop()
    print(summarize(rows, a.label))
    return 0


if __name__ == "__main__":
    sys.exit(main())
