# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89
# Build the Code-flavor MCP server. SQLX_OFFLINE uses the committed .sqlx
# cache so no database is needed at build time. cmake + pkg-config are
# required to build native crypto deps (aws-lc-sys / ring).
# Pin bookworm so the builder's glibc matches the distroless cc-debian12 runtime.
FROM rust:1.97-bookworm@sha256:77fac8b98f9f46062bb680b6d25d5bcaabfc400143952ebc572e924bcbedc3fa AS builder
ENV SQLX_OFFLINE=true
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p proxima-mcp --features code \
    && cp target/release/proxima-mcp /proxima-mcp

# Distroless cc image: glibc + libstdc++ (for aws-lc) + ca-certificates
# (for outbound TLS to Zitadel/S3/embeddings), non-root by default.
FROM gcr.io/distroless/cc-debian12@sha256:a90cf0f046efb32466b38b0972fef3a95e7c580e392e79ff1b7ac08c15fed0bc AS runtime
COPY --from=builder /proxima-mcp /usr/local/bin/proxima-mcp
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/proxima-mcp"]
