# Per-wake egress logging proxy.
#
# A forward proxy the workspace container's HTTP(S)_PROXY points at. It logs
# every CONNECT tunnel and HTTP request line with a timestamp to stdout, so
# `docker logs` yields the wake's egress record. No TLS interception — the
# log is CONNECT-level metadata (which hosts, when), not payloads.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends tinyproxy ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# No Allow/Deny lines — the proxy only ever sees the per-wake internal
# network, so every client on it is permitted. No ConnectPort lines — CONNECT
# to any port is allowed; this is an observation instrument, not a jail.
RUN cat > /etc/tinyproxy/tinyproxy.conf <<'EOF'
Port 8888
Listen 0.0.0.0
Timeout 600
LogLevel Info
LogFile "/dev/stdout"
DisableViaHeader Yes
EOF

EXPOSE 8888

# `-d` keeps tinyproxy in the foreground so the container stays up and its
# log streams to `docker logs`.
CMD ["tinyproxy", "-d", "-c", "/etc/tinyproxy/tinyproxy.conf"]
