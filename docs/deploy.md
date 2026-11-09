# Deploying EdgeSync Sync Server to Fly.io

This document provides step-by-step instructions for deploying the **EdgeSync Central Sync Server** to [Fly.io](https://fly.io).

The Go Sync Server runs in a hardened, non-root Alpine container, connects to PostgreSQL via `DATABASE_URL`, and automatically applies database migrations on startup. For public cloud deployments, edge TLS termination is handled by Fly's proxy while the application serves on standard HTTP internally. (For disconnected/DDIL edge deployments, the server additionally supports full mTLS via local certificates).

---

## Prerequisites

1. Install the Fly CLI (`flyctl`):
   - **macOS / Linux:**
     ```bash
     curl -L https://fly.io/install.sh | sh
     ```
   - **Windows (PowerShell):**
     ```powershell
     powershell -Command "iwr https://fly.io/install.ps1 -useb | iex"
     ```
2. Authenticate with your Fly account:
   ```bash
   flyctl auth login
   ```
3. A PostgreSQL database instance (Fly Postgres, Supabase, Neon, AWS RDS, or any managed PostgreSQL provider).

---

## Deployment Steps

All deployment commands should be executed from the `server/` directory:

```bash
cd server
```

### 1. Launch App Configuration

If deploying for the first time, initialize the app on Fly.io using the existing `fly.toml`:

```bash
flyctl launch --copy-config --yes
```

> **Note:** If prompted whether to deploy now, select **No** so you can set the database secret first.

### 2. Configure Database Secret

Set the `DATABASE_URL` secret. Replace the connection string with your PostgreSQL credentials:

```bash
flyctl secrets set DATABASE_URL="postgres://<user>:<password>@<host>:<port>/<dbname>?sslmode=require"
```

*Example for Fly Postgres (attached cluster):*
```bash
flyctl postgres attach <postgres-app-name>
```

### 3. Deploy the Server

Deploy the multi-stage Docker container:

```bash
flyctl deploy
```

Fly.io will build the container image using `server/Dockerfile`, allocate a 512MB VM, run the startup migrations, and wait for the `/healthz` HTTP health check to pass.

---

## Verifying Deployment

Once deployed, retrieve your application URL or status with:

```bash
flyctl status
```

### Health Check Probing

Verify the live deployment using `curl`:

```bash
curl -i https://edgesync-server.fly.dev/healthz
```

**Expected Response (HTTP 200 OK):**
```http
HTTP/2 200 
content-type: application/json
date: Sun, 08 Nov 2026 23:30:00 GMT

{"status":"ok"}
```

---

## Optional: Operating with mTLS (Private DDIL Enclave)

If deploying the container inside a private network or air-gapped enclave where end-to-end mutual TLS is required:

1. Generate your certificates using `scripts/gen_certs.sh` or `scripts/gen_certs.ps1`.
2. Provide the certificates to the container and set the environment variables:
   ```bash
   TLS_CERT=/path/to/server.crt
   TLS_KEY=/path/to/server.key
   TLS_CA=/path/to/ca.crt
   ```
3. When `TLS_CERT` and `TLS_KEY` are unset, the server gracefully serves HTTP on `:8080`, allowing Fly's edge proxy to handle public TLS termination.
