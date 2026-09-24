#!/usr/bin/env python3
"""A deterministic stand-in for the Claude Code / Codex CLI.

It answers `--version`, discovers the per-run MCP launcher the way each real
CLI would (from `--mcp-config` or from a `-c mcp_servers…command=` override),
spawns it, speaks JSON-RPC to it over stdio, and prints provider-shaped JSONL
events to stdout. No network, no credentials, no model.

Point NEEV_BROWSER_CLAUDE_BIN or NEEV_BROWSER_CODEX_BIN at this file to run the
whole companion end to end.
"""

import json
import os
import subprocess
import sys


def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()


def find_launcher(argv):
    for index, arg in enumerate(argv):
        if arg == "--mcp-config" and index + 1 < len(argv):
            with open(argv[index + 1]) as handle:
                config = json.load(handle)
            return config["mcpServers"]["neev_browser"]["command"]
        if arg.startswith("mcp_servers.neev_browser.command="):
            return arg.split("=", 1)[1].strip('"')
    return None


class McpPeer(object):
    def __init__(self, command):
        self._proc = subprocess.Popen([command], stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE)
        self._next_id = 0

    def call(self, method, params=None):
        self._next_id += 1
        request = {"jsonrpc": "2.0", "id": self._next_id, "method": method,
                   "params": params or {}}
        self._proc.stdin.write(json.dumps(request).encode("utf-8") + b"\n")
        self._proc.stdin.flush()
        line = self._proc.stdout.readline()
        if not line:
            raise RuntimeError("the MCP server closed before answering %s" % method)
        return json.loads(line.decode("utf-8"))

    def close(self):
        try:
            self._proc.stdin.close()
            self._proc.wait(timeout=5)
        except Exception:
            self._proc.kill()


def script_from_env():
    raw = os.environ.get("FAKE_AGENT_SCRIPT")
    if not raw:
        return [{"tool": "browser_snapshot", "arguments": {}}]
    return json.loads(raw)


def main(argv):
    if "--version" in argv:
        print("fake-agent 1.0.0")
        return 0
    launcher = find_launcher(argv)
    if launcher is None:
        emit({"type": "error", "text": "no MCP launcher was configured"})
        return 1

    peer = McpPeer(launcher)
    try:
        peer.call("initialize", {"protocolVersion": "2024-11-05"})
        listed = peer.call("tools/list")["result"]["tools"]
        emit({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "tools: " + ",".join(
                sorted(tool["name"] for tool in listed))}]}})
        for step in script_from_env():
            response = peer.call("tools/call", {"name": step["tool"],
                                                "arguments": step.get("arguments", {})})
            result = response.get("result", {})
            first = (result.get("content") or [{}])[0]
            emit({"type": "assistant", "message": {"content": [
                {"type": "text",
                 "text": "%s -> %s%s" % (step["tool"],
                                         "ERROR " if result.get("isError") else "",
                                         first.get("text", first.get("type", "")))}]}})
        emit({"last_agent_message": "fake agent finished"})
        return 0
    finally:
        peer.close()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
