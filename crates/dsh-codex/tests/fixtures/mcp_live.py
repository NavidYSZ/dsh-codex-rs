#!/usr/bin/env python3
"""MCP fixture with gated startup and same-name schema updates; no network."""
import json
import os
from pathlib import Path
import sys
import time

config_path, calls_path = map(Path, sys.argv[1:3])
config = json.loads(config_path.read_text())
ready = Path(config["ready"])
deadline = time.monotonic() + 30
while not ready.exists():
    if time.monotonic() >= deadline:
        raise RuntimeError("fixture startup gate timed out")
    time.sleep(0.01)
field = "old"


def send(value):
    print(json.dumps(value), flush=True)


def text(value):
    return {"type": "text", "text": value}


def tool(name, properties=None, required=None):
    return {
        "name": name,
        "description": "Live fixture " + name,
        "inputSchema": {
            "type": "object", "properties": properties or {},
            "required": required or [], "additionalProperties": False,
        },
    }


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if "id" not in request:
        continue
    if method == "initialize":
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {"listChanged": True}},
            "serverInfo": {"name": "live-fixture", "version": "1"},
            "instructions": config["instructions"],
        }
    elif method == "tools/list":
        result = {"tools": [
            tool("echo", {field: {"type": "string"}}, [field]),
            tool("change", {"field": {"type": "string"}}, ["field"]),
            tool("json_text"), tool("structured"), tool("fail"),
        ]}
    elif method == "tools/call":
        params = request["params"]
        name, args = params["name"], params.get("arguments", {})
        with calls_path.open("a") as calls:
            calls.write(json.dumps({"pid": os.getpid(), "name": name, "args": args}) + "\n")
        if name == "echo":
            result = {"content": [text("plain " + field + ": " + args[field])]}
        elif name == "change":
            field = args["field"]
            send({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
            result = {"content": [text("schema changed")]}
        elif name == "json_text":
            result = {"content": [text('{"n":7}')]}
        elif name == "structured":
            result = {"content": [text("structured fixture")], "structuredContent": {"n": 9}}
        else:
            result = {"content": [text("fixture failure")], "isError": True}
    else:
        result = {}
    send({"jsonrpc": "2.0", "id": request["id"], "result": result})
