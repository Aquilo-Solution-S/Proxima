# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89
# Build the Code-flavor MCP server. SQLX_OFFLINE uses the committed .sqlx
# cache so no database is needed at build time. cmake + pkg-config are
# required to build native crypto deps (aws-lc-sys / ring).
# Pin bookworm so the builder's glibc matches the distroless cc-debian12 runtime.
FROM rust:1.97-bookworm@sha256:7d0723df719e7f213b69dc7c8c595985c3f4b060cfbee4f7bc0e347a86fe3b6a AS builder
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
FROM gcr.io/distroless/cc-debian12@sha256:d703b626ba455c4e6c6fbe5f36e6f427c85d51445598d564652a2f334179f96e AS runtime
COPY --from=builder /proxima-mcp /usr/local/bin/proxima-mcp
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/proxima-mcp"]
