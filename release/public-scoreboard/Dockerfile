FROM caddy:2-alpine

# The upstream binary has a low-port file capability. A fresh copy removes it
# so the server can run on 8080 as a non-root user with all capabilities dropped.
RUN cp /usr/bin/caddy /usr/bin/caddy-unprivileged \
    && mv /usr/bin/caddy-unprivileged /usr/bin/caddy
