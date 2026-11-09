#!/usr/bin/env bash
# ==============================================================================
# EdgeSync — mTLS Certificate Generator (POSIX Bash)
# Specification: §8 Week 10
#
# Generates:
# - ca.crt, ca.key       (Root Certificate Authority)
# - server.crt, server.key (Server cert with SAN: localhost, 127.0.0.1)
# - client.crt, client.key (Client cert with clientAuth EKU)
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

OUTPUT_DIR="${1:-${ROOT_DIR}/certs}"
mkdir -p "${OUTPUT_DIR}"

echo "Generating mTLS certificates in: ${OUTPUT_DIR}"

(
    cd "${ROOT_DIR}/server"
    go run ./cmd/gen-certs -out "${OUTPUT_DIR}"
)

if [ -f "${OUTPUT_DIR}/ca.crt" ] && [ -f "${OUTPUT_DIR}/server.crt" ] && [ -f "${OUTPUT_DIR}/client.crt" ]; then
    echo "✅ mTLS certificates successfully generated."
    exit 0
else
    echo "❌ Failed to generate all required certificates."
    exit 1
fi
