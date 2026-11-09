# ==============================================================================
# EdgeSync — Cloud Sync Server Container (§8 Week 11)
# Multi-stage build producing a minimal, secure, non-root Alpine container
# ==============================================================================

# --- Stage 1: Build statically linked binary ---
FROM golang:alpine AS builder

WORKDIR /app

# Install build dependencies
RUN apk add --no-cache ca-certificates tzdata

# Cache module dependencies
COPY go.mod go.sum ./
RUN go mod download

# Copy source tree
COPY . .

# Compile optimized static binary
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /edgesync-server ./cmd/api

# --- Stage 2: Minimal runtime container ---
FROM alpine:latest

# Install root TLS certificates and timezone data
RUN apk --no-cache add ca-certificates tzdata

# Create dedicated non-root user (UID 10001)
RUN addgroup -g 10001 -S edgesync && \
    adduser -u 10001 -S edgesync -G edgesync

WORKDIR /home/edgesync

# Copy binary and database migrations
COPY --from=builder /edgesync-server /usr/local/bin/edgesync-server
COPY --from=builder /app/migrations ./migrations

# Assign ownership to non-root user
RUN chown -R edgesync:edgesync /home/edgesync /usr/local/bin/edgesync-server

# Switch to non-root user
USER 10001:10001

# Default environment configuration
ENV PORT=8080
ENV MIGRATIONS_DIR=/home/edgesync/migrations

# Expose internal service port
EXPOSE 8080

# Execute server binary
ENTRYPOINT ["/usr/local/bin/edgesync-server"]
