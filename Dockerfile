# syntax=docker/dockerfile:1@sha256:87999aa3d42bdc6bea60565083ee17e86d1f3339802f543c0d03998580f9cb89
# Build the Code-flavor MCP server. cmake + pkg-config are required to
# build native crypto deps (aws-lc-sys / ring). Pin bookworm so the
# builder's glibc matches the distroless cc-debian12 runtime.
FROM rust:1.98-bookworm@sha256:e70e2eec3d495fd5c8e0be74adda86507dfac7f51a724fbf9813ff59b2b247c7 AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p proxima-mcp \
    && cp target/release/proxima-mcp /proxima-mcp

# Distroless cc image: glibc + libstdc++ (for aws-lc) + ca-certificates
# (for outbound TLS to Zitadel/S3/embeddings), non-root by default.
FROM gcr.io/distroless/cc-debian12@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa AS runtime
# Provenance. Without these a running container cannot be attributed to a
# release or a commit — `initialize.serverInfo` reports the version, but only
# to an MCP client that can already reach it.
ARG VERSION=0.0.0
ARG REVISION=unknown
LABEL org.opencontainers.image.title="proxima-mcp" \
      org.opencontainers.image.description="Proxima MCP server (code flavor)" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/Aquilo-Solution-S/Proxima"
COPY --from=builder /proxima-mcp /usr/local/bin/proxima-mcp
USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/proxima-mcp"]
