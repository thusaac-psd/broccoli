//! A real Redis for tests, at the version the message queue needs.

use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ContainerRequest, ImageExt};
use testcontainers_modules::redis::Redis;

/// The Redis image tag every test runs against (the queue needs Redis 7).
pub const REDIS_TAG: &str = "7-alpine";

/// The Redis image, for tests that adjust the container before starting it.
pub fn redis_image() -> ContainerRequest<Redis> {
    Redis::default().with_tag(REDIS_TAG)
}

/// A started Redis and its URL.
pub async fn redis() -> (ContainerAsync<Redis>, String) {
    let container = redis_image().start().await.expect("start redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    (container, format!("redis://127.0.0.1:{port}"))
}

/// A started Redis on a fixed host port, so that stopping and starting the
/// container keeps its URL, with every write appended and synced so a
/// restart keeps all of it.
pub async fn restartable_redis() -> (ContainerAsync<Redis>, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("local addr")
        .port();
    let container = redis_image()
        .with_mapped_port(port, ContainerPort::Tcp(6379))
        .with_cmd([
            "redis-server",
            "--appendonly",
            "yes",
            "--appendfsync",
            "always",
        ])
        .start()
        .await
        .expect("start redis");
    (container, format!("redis://127.0.0.1:{port}"))
}

/// Start a stopped [`restartable_redis`] again and wait until it accepts
/// connections (with its data loaded), as it reports in its log.
pub async fn start_again(container: &ContainerAsync<Redis>) {
    let ready = || async {
        let log = container.stdout_to_vec().await.unwrap_or_default();
        String::from_utf8_lossy(&log)
            .matches("Ready to accept connections")
            .count()
    };
    let before = ready().await;
    container.start().await.expect("restart redis");
    while ready().await <= before {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Make a pending stream entry look idle for `idle` without waiting, keeping
/// its owner: `XCLAIM ... IDLE` sets the idle time a consumer group reports.
pub async fn age_entry(
    conn: &mut redis::aio::MultiplexedConnection,
    stream: &str,
    group: &str,
    owner: &str,
    id: &str,
    idle: std::time::Duration,
) {
    let _: Vec<String> = redis::cmd("XCLAIM")
        .arg(stream)
        .arg(group)
        .arg(owner)
        .arg(0)
        .arg(id)
        .arg("IDLE")
        .arg(idle.as_millis() as u64)
        .arg("JUSTID")
        .query_async(conn)
        .await
        .expect("age the entry");
}
