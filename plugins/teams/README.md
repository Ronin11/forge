# forge-teams

A bidirectional Microsoft Teams bridge for Forge.

- **Outbound (always):** posts a card in a Teams channel when a task needs input
  (a question), a target fails or goes unverified, the budget hits a hard stop,
  or a proposal is filed. Toggle each in config. Uses a channel **incoming
  webhook** — no app registration, no admin consent.
- **Inbound (optional):** polls the same channel through Microsoft Graph and
  turns messages into work.
  - `/answer <task-id> <text>` → answers a waiting question (the id is on the
    card, e.g. `3f69f96a`).
  - `/help` → the command list.
  - anything else → Forge's concierge, which files a task or just replies.

Replies inside a card's thread count too, so answering where the card landed
works. Forge's own cards are posted by the webhook (an application, not a user),
so the bridge never reads its own messages back.

## Setup

### 1. The outbound webhook (required)

In Teams: **channel → ⋯ → Workflows → "Post to a channel when a webhook request
is received"**, then copy the generated URL. That flow expects the Adaptive Card
envelope this plugin sends by default (`format = "adaptive"`). If you still have
a classic Office 365 connector URL, set `format = "messagecard"` instead.

Verify the URL before enabling the plugin:

```sh
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"type":"message","attachments":[{"contentType":"application/vnd.microsoft.card.adaptive","content":{"type":"AdaptiveCard","version":"1.4","body":[{"type":"TextBlock","text":"hello from forge"}]}}]}' \
  "$WEBHOOK_URL"
```

### 2. Graph credentials (only for inbound)

Inbound needs an Entra (Azure AD) app registration with a client secret and the
**application** permission `ChannelMessage.Read.All`, admin-consented. Reading
channel messages app-only is a Microsoft *protected API*: the tenant must also
be approved for it (Teams "protected APIs" request), otherwise the poll gets
`403` — logged, never fatal, and the outbound half keeps working. `team_id` is
the team's group id and `channel_id` the `19:…@thread.tacv2` id; both are in the
channel's "Get link to channel" URL.

Without a complete `[teams.graph]` table the plugin logs that intake is disabled
and runs outbound-only.

## Config — `<FORGE_PLUGIN_DIR>/teams.toml`

```toml
[teams]
webhook_url = "https://prod-00.westus.logic.azure.com:443/workflows/…"
# format         = "adaptive"      # or "messagecard" for a legacy connector URL
# ui             = "http://127.0.0.1:7340"
# command_prefix = "!forge"        # require (and strip) this on inbound messages
# poll_seconds   = 30              # inbound poll cadence
# questions = true  failures = true  proposals = true  throttling = true
# intake    = true                 # set false to keep the bridge outbound-only

[teams.graph]                       # optional; all five needed for intake
# tenant_id          = "00000000-0000-0000-0000-000000000000"
# client_id          = "00000000-0000-0000-0000-000000000000"
# client_secret_file = "~/.forge/secrets/teams-client-secret"   # or client_secret = "…"
# team_id            = "00000000-0000-0000-0000-000000000000"
# channel_id         = "19:abc123@thread.tacv2"
# allowed_users      = ["nate@example.com"]   # empty = anyone in the channel
```

`webhook_url` is required; without it the plugin idles (it does not crash-loop)
until you fill it in and restart it. The client secret may be given inline or,
better, in a 0600 file named by `client_secret_file`.

Inbound progress (the cursor and recent message ids) is kept in
`<FORGE_PLUGIN_DIR>/state.json`, so a restart neither replays commands nor
misses them. A fresh install starts at "now" — channel history is not a backlog.

## Install

`forge plugin install teams`, then edit `teams.toml` and
`forge plugin disable teams && forge plugin enable teams` (or restart the daemon).
