# syntax=docker/dockerfile:1
# Build the Code-flavor MCP server. SQLX_OFFLINE uses the committed .sqlx
# cache so no database is needed at build time. cmake + pkg-config are
# required to build native crypto deps (aws-lc-sys / ring).
# Pin bookworm so the builder's glibc matches the distroless cc-debian12 runtime.
FROM rust:1.96-bookworm AS builder
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
FROM gcr.io/distroless/cc-debian12 AS runtime
COPY --from=builder /proxima-mcp /usr/local/bin/proxima-mcp
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/proxima-mcp"]
