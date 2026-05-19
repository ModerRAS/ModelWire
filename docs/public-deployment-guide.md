# ModelWire Public Deployment Guide

ModelWire is designed to sit behind a reverse proxy and should not terminate TLS itself.

## Required baseline

- Run ModelWire behind Caddy, Nginx, Cloudflare, or a managed load balancer.
- Terminate TLS at the edge proxy.
- Set `server.bind` to a private interface or container port.
- Enable downstream authentication.
- Keep admin auth enabled.
- Mount persistent storage for SQLite and archives.

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

## Sensitivity

- API keys and relay keys are sensitive.
- Archive contents may contain user prompts, tool output, and visible assistant text.
- Hidden reasoning is not archived or logged by default.
