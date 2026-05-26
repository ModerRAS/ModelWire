# ModelWire Public Deployment Guide

ModelWire is designed to sit behind a reverse proxy and should not terminate TLS itself.

## Required baseline

- Run ModelWire behind Caddy, Nginx, Cloudflare, or a managed load balancer.
- Terminate TLS at the edge proxy.
- Set `server.bind` to a private interface or container port.
- Enable downstream authentication.
- Keep admin auth enabled.
- Mount persistent storage for SQLite and archives.
- Set `security.managed_key_encryption_secret` before configuring managed
  provider API keys.

## Minimal safe config shape

Use `modelwire.toml.example` as a local starting point. For a public or Docker
deployment, change only after relay keys and managed-key encryption are ready:

```toml
[server]
bind = "0.0.0.0:8787"
database_url = "sqlite:///app/data/modelwire.db"
data_dir = "/app/data"

[security]
downstream_auth = "relay_key"
admin_auth = "local_password"
admin_password = "replace-with-long-random-admin-password"
public_deployment = true
log_secret = "replace-with-long-random-log-secret"
managed_key_encryption_secret = "replace-with-long-random-secret"

[[security.relay_keys]]
key_hash = "replace-with-hash-produced-from-your-relay-key-and-log-secret"
enabled = true

[archive]
capture_mode = "off"
root = "/app/data/archives"
```

Managed provider keys from file config are rejected unless
`managed_key_encryption_secret` is configured. Admin-created or file-seeded
managed keys are encrypted before they are stored in operational state.

## Docker and GHCR

Release tags publish a Linux archive and a GHCR image from the `Release`
workflow. The image name is the lower-case repository path:

```text
ghcr.io/moderras/modelwire:<tag>
```

Mount a config file and data directory:

```bash
docker run --rm \
  -p 8787:8787 \
  -v "$PWD/modelwire.toml:/app/modelwire.toml:ro" \
  -v "$PWD/data:/app/data" \
  ghcr.io/moderras/modelwire:<tag>
```

## Recommended headers

- `X-Forwarded-For`
- `X-Forwarded-Proto`
- `X-Request-ID`

## Rate limits

- Apply edge rate limiting in front of ModelWire.
- Keep ModelWire per-key and per-IP rate limits enabled.

## Backups

- Back up `modelwire.toml`.
- Back up the SQLite database.
- Back up archive files if capture is enabled.
- Keep `security.managed_key_encryption_secret` in your secret manager; losing it
  means encrypted managed upstream keys cannot be decrypted.

## Sensitivity

- API keys and relay keys are sensitive.
- Archive contents may contain user prompts, tool output, and visible assistant text.
- Hidden reasoning is not archived or logged by default.
