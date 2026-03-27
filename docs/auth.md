# Authentication

## Priority

1. Environment variable (`ANTHROPIC_API_KEY`)
2. OAuth token from macOS Keychain (auto-refreshed)
3. API key from macOS Keychain

## Keychain storage

Credentials are stored in macOS Keychain via the `security` CLI.
No files on disk. Each provider gets a keychain entry:

- **Service**: `nerv-{provider}` (e.g., `nerv-anthropic`)
- **Account**: `nerv`
- **Value**: JSON-serialized `Credential` enum

```rust
enum Credential {
    ApiKey { key: String },
    OAuth { refresh: String, access: String, expires: u64 },
}
```

## Anthropic OAuth flow

PKCE (Proof Key for Code Exchange) with a local callback server.

### Steps

1. Generate random 32-byte verifier, SHA-256 hash it for the challenge
2. Start TCP listener on `127.0.0.1:53692`
3. Open browser to `claude.ai/oauth/authorize` with PKCE params
4. User authenticates in browser
5. Browser redirects to `localhost:53692/callback?code=...&state=...`
6. Exchange code for tokens at `platform.claude.com/v1/oauth/token`
7. Store `{refresh, access, expires}` in Keychain
8. Register Anthropic provider with the access token

### Required headers for OAuth

The Anthropic API requires these headers when using OAuth tokens:

```
Authorization: Bearer {access_token}
anthropic-beta: claude-code-20250219,oauth-2025-04-20
user-agent: claude-cli/1.0.0
x-app: cli
```

Additionally, the system prompt must begin with:
```
You are Claude Code, Anthropic's official CLI for Claude.
```

Without the beta headers, the API returns 401. Without the identity
prefix in the system prompt, Sonnet and Opus return 400 (Haiku works
without it). These are server-side checks — the identity is prepended
automatically in `AnthropicProvider::build_request_body` when
`use_bearer = true`.

### Token refresh

Tokens expire after ~10 hours (minus a 5-minute safety margin). On
startup and before each prompt, `AuthStorage::api_key()` checks the
expiry timestamp. If expired, it calls `refresh_anthropic_token()` with
the stored refresh token to get new access + refresh tokens, then updates
the Keychain entry.

### Provider construction

`AnthropicProvider::new(key)` uses `x-api-key` header (API key auth).
`AnthropicProvider::new_oauth(token)` uses `Authorization: Bearer` with
the OAuth beta headers. `ModelRegistry` checks `auth.is_oauth()` to
choose the right constructor.

## Configurable headers

Extra headers per provider can be set in `~/.nerv/config.json`:

```jsonc
{
  "headers": {
    "anthropic": {
      "x-custom": "value"
    }
  }
}
```

These are applied after the built-in headers, so they can override
defaults without recompiling.
