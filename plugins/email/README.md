# forge-email

A bidirectional email bridge for Forge — plain SMTP/IMAP, or Microsoft 365 /
Outlook through Graph.

- **Outbound:** mails you when a task needs input (a question), a target fails
  or goes unverified, the budget hits a hard stop, or a proposal is filed.
  Toggle each in config.
- **Inbound:** turns your mail into work.
  - **Reply to a question mail** → answers that question. The subject marker
    `[forge #3f69f96a]` survives your client's `Re:`, so nothing else is needed —
    no command, no id to copy. Quoted text and signatures are stripped, so only
    what you actually typed becomes the answer.
  - **Any other message** → Forge's concierge, which files it as a task or
    replies to you.

Only mail from `to` (or the addresses in `allowed_senders`) is honored. Mail
that is machine-generated — auto-replies, list mail, and Forge's own
notifications coming back — is ignored, so two mailboxes cannot talk each other
into a loop.

## Which mode

| | `mode = "smtp"` (default) | `mode = "graph"` |
|---|---|---|
| Sends with | SMTP | Graph `sendMail` |
| Reads with | IMAP | Graph, inbox folder |
| Credentials | mailbox username + password (or app password) | Entra app registration |
| Use it for | Gmail with an app password, Fastmail, a company relay, anything | Microsoft 365 / Outlook, where basic-auth SMTP and IMAP are usually disabled |

Outbound works with SMTP alone: leave `[email.imap]` empty and the plugin runs
notification-only.

### Microsoft 365 setup (`mode = "graph"`)

An Entra (Azure AD) app registration with a client secret and the **application**
permissions `Mail.Send` and `Mail.ReadWrite`, admin-consented. Scope them to the
one mailbox with an [application access policy](https://learn.microsoft.com/graph/auth-limit-mailbox-access)
rather than leaving the app able to read every mailbox in the tenant.

### Gmail / other IMAP setup (`mode = "smtp"`)

An app password (Gmail: Account → Security → App passwords), `smtp.gmail.com:587`
and `imap.gmail.com:993`. Verify the mailbox before enabling the plugin:

```sh
python3 -c 'import imaplib,os;c=imaplib.IMAP4_SSL("imap.gmail.com");c.login(os.environ["U"],os.environ["P"]);print(c.select("INBOX"))'
```

## Config — `<FORGE_PLUGIN_DIR>/email.toml`

```toml
[email]
from = "forge@example.com"      # the address Forge sends from
to   = "nate@example.com"       # you — notified, and the only sender honored
# mode            = "smtp"      # or "graph"
# ui              = "http://127.0.0.1:7340"
# allowed_senders = ["nate@example.com", "sam@example.com"]  # replaces `to` as the allow-list
# poll_seconds    = 60          # inbound poll cadence
# questions = true  failures = true  proposals = true  throttling = true
# intake    = true              # set false to keep the bridge outbound-only

[email.smtp]                    # mode "smtp": required
host          = "smtp.example.com"
# port        = 587             # 465 with tls = "implicit"
# tls         = "starttls"      # starttls | implicit | none
# username    = "forge@example.com"
# password_file = "~/.forge/secrets/smtp-password"   # or password = "…"

[email.imap]                    # mode "smtp": optional — omit for outbound only
host          = "imap.example.com"
username      = "forge@example.com"
# port        = 993
# mailbox     = "INBOX"
# password_file = "~/.forge/secrets/imap-password"   # or password = "…"

[email.graph]                   # mode "graph": required
# tenant_id          = "00000000-0000-0000-0000-000000000000"
# client_id          = "00000000-0000-0000-0000-000000000000"
# client_secret_file = "~/.forge/secrets/graph-client-secret"   # or client_secret = "…"
# mailbox            = "forge@example.com"    # defaults to `from`
```

`from`, `to` and a usable transport are required; without them the plugin idles
(it does not crash-loop) and logs what is missing. Passwords and secrets are
best kept in 0600 files named by `*_file`.

Inbound mail is marked read once handled, and the ids handled recently are kept
in `<FORGE_PLUGIN_DIR>/state.json`, so a restart mid-poll neither repeats a
command nor loses one.

## Install

`forge plugin install email`, then edit `email.toml` and
`forge plugin disable email && forge plugin enable email` (or restart the daemon).
