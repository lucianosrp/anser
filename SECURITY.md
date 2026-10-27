# Security

## Reporting a vulnerability

Report it privately: on the repository's **Security** tab, choose **Report a
vulnerability** (GitHub private vulnerability reporting). Please don't open a
public issue or pull request for a vulnerability.

Include what you found, the version (`anser-server --version`) and how to
reproduce it. Never include real statements, keys or tokens: use the demo
household.

## Scope

- `anser-server` is meant to listen on `127.0.0.1` and be published to a tailnet
  with `tailscale serve`. **Never expose it to the public internet**: the sync
  endpoints serve a whole database.
- API keys the app holds must be read-only; it refuses a key that can trade,
  withdraw or transfer. Nothing in the app places trades or moves money.
- A deployment that put the server on the public internet, or a leaked key or
  token, is a configuration problem rather than a vulnerability in the code.
  Tell us if the docs led you there.
