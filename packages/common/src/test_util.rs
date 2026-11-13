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
