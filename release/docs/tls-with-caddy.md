# TLS With Caddy

Use the `gateway` role when a LAN or cloud deployment needs one public HTTPS
entrypoint in front of multiple server machines.

## DNS

Point an A or AAAA record at the gateway host. Ports 80 and 443 must be
reachable from the public internet for Let's Encrypt validation.

## HTTP Gateway

The bundled gateway compose listens on HTTP port 80 and load balances to the
server addresses in `BROCCOLI_UPSTREAMS`:

```bash
BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000' ./install.sh gateway
```

## HTTPS Caddyfile

For public HTTPS, copy `examples/Caddyfile`, set the domain values, and run
Caddy on the gateway host:

```bash
export ACME_EMAIL=admin@example.com
export BROCCOLI_DOMAIN=judge.example.com
export BROCCOLI_UPSTREAMS='10.0.0.21:3000 10.0.0.22:3000'
caddy validate --config examples/Caddyfile
caddy run --config examples/Caddyfile
```

## Trusted Proxies

Every server node behind the gateway must trust the gateway's address, and
only that address, for `X-Forwarded-For`. Without it, login and register rate
limits key on the gateway and the whole venue shares one budget; with a wider
range, any client inside it can forge its address. `install.sh gateway` prints
the addresses its upstreams see it as. On each server node run:

```bash
BROCCOLI_GATEWAY_HOSTS='10.0.0.5' ./install.sh server
```

which writes `BROCCOLI__SERVER__TRUSTED_PROXIES='["10.0.0.5/32"]'` into
`.env.server`. With HTTPS at the gateway, also set
`BROCCOLI__AUTH__SECURE_COOKIES=true` there.

## Troubleshooting

Use `caddy validate --config examples/Caddyfile` for syntax errors and
`docker compose -f docker-compose.server.yaml logs -f server` on the affected
server node for application errors. A 502 from Caddy usually means no upstream
server is healthy.
