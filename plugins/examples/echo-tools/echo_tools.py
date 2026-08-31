#!/usr/bin/env python3
"""Throwaway MCP server for the M7 smoke: newline-delimited JSON-RPC 2.0 on
stdio (the framing forge's mcpserve speaks), one tool: ping."""
import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        rid, method = req.get("id"), req.get("method")
        if rid is None:
            continue  # notification: never answered
        if method == "initialize":
            proto = (req.get("params") or {}).get("protocolVersion") or "2025-06-18"
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "protocolVersion": proto,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "echo-tools", "version": "0.1.0"}}})
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [{
                "name": "ping",
                "description": "Echo the given msg back as 'pong: <msg>'.",
                "inputSchema": {"type": "object",
                                "properties": {"msg": {"type": "string"}},
                                "additionalProperties": False}}]}})
        elif method == "tools/call" and (req.get("params") or {}).get("name") == "ping":
            msg = ((req.get("params") or {}).get("arguments") or {}).get("msg", "")
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": "pong: %s" % msg}], "isError": False}})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": rid, "result": {}})
        else:
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32601, "message": "method not found: %s" % method}})


if __name__ == "__main__":
    main()
