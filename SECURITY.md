# Security

Forge runs coding agents against untrusted repository content, issue and PR
text, tool output, and web content. Those are data, not instructions. The
security boundary is enforced by the sandbox and verification, not by an
agent promising to follow its prompt.

## Attempts and credentials

Attempts run under **bubblewrap**, with a read-only system, private temporary
files, and a **tmpfs home** exposing only the required agent state, task clone,
and configured toolchain/cache mounts. The clone has **no remote**; the
registered checkout and its `.git` are not mounted. The kernel handles landing
and pushing. See [the sandbox implementation](src/sandbox.rs) and
[the task lifecycle](README.md#what-happens-to-a-task).

A private network namespace routes outbound traffic through an allowlist
proxy. Egress is bounded to the **model endpoint and the repository's declared
hosts**, read from `[sandbox] egress` on the trusted base. The model token is
available to the agent inside the sandbox; from there it can reach only those
allowed destinations. This is a destination restriction, not a guarantee that
an allowed host cannot receive secrets. Tokens are not yet scoped to each task
and expired at its end. See [egress enforcement](src/egress.rs) and
[the remaining isolation work](docs/GTM.md).

Sandboxing is the default and missing `bwrap` is an error. Explicitly running
with `FORGE_SANDBOX=0` removes that isolation; it is not the posture described
above. `forge doctor` reports the configured sandbox and egress policy.

## Verification and access

An attempt cannot change **`forge.toml`** and pass verification. Check definitions
come from the trusted base, not the candidate branch. Protected paths declared
under `[verify] protected` are rejected unless the task was explicitly granted
`--allow-protected`; that exception does not permit changing the repository
configuration. The verification namespace belongs to verification, not the
implementer. In this repository, `tests/boundary.rs` and `forge.toml` are
protected. See [verification](src/verify.rs) and [checks](docs/CHECKS.md).

**Every request to the operator web client requires its token**, including
assets and the event stream. The token is stored in `FORGE_HOME/web.token`;
the initial token link establishes a cookie. The client binds to loopback by
default. Keep token links private. See [web access](README.md#clients) and
[the authentication tests](web/tests/server.rs). The separate project portal
has its own [token-based access contract](docs/CLIENT.md#portal).

**Trust by source:** once that initiative lands end to end, the authenticated
source of a task determines its trust tier and permitted capabilities, rather
than assertions in its text. There are already operator/contact/public policy
hooks in [configuration](src/config.rs) and [task creation](src/queue.rs); do
not treat those hooks alone as a guarantee that every intake route enforces
the completed initiative.

## Reporting a problem

Report suspected vulnerabilities privately to the repository maintainer through
an existing private contact channel. If you have no private contact, open an
issue asking for a secure reporting channel without exploit details or secrets.
Include the affected commit/version, configuration with credentials removed,
reproduction steps, and the boundary you believe was crossed. Do not publish
live tokens, private repository content, or sensitive attempt logs.
