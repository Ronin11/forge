# echo-tools

Throwaway third-party MCP plugin used by the M7 smoke (SMOKE.md §M7 step 4).
It is a newline-delimited JSON-RPC 2.0 MCP server on stdio exposing one tool,
`ping`, which answers `pong: <msg>`.

To use it as a third-party plugin:

    cp -r plugins/examples/echo-tools ~/.forge/plugins/echo-tools

then `forge plugin enable echo-tools` (scope `tools:provide`). Its tool
surfaces namespaced as `echo-tools_ping`. Remove with
`forge plugin disable echo-tools && rm -rf ~/.forge/plugins/echo-tools`.
