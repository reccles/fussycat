"""A minimal MCP server over stdio (newline-delimited JSON-RPC), for
fussycat's tests: a read-only tool, a destructive one, a slow one, one that
returns an image, and a resource. Standard library only."""
import json
import sys
import time

STATE = {"in_progress": 3, "blocked": 1}

TOOLS = [
    {
        "name": "get_current_state",
        "description": "The board's current state.",
        "inputSchema": {"type": "object", "properties": {"team": {"type": "string"}}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "move_card",
        "description": "Moves a card to a column.",
        "inputSchema": {
            "type": "object",
            "properties": {"card": {"type": "integer"}, "to": {"type": "string"}},
            "required": ["card", "to"],
        },
        "annotations": {"destructiveHint": True},
    },
    {
        "name": "wait",
        "description": "Waits five seconds.",
        "inputSchema": {"type": "object"},
    },
    {
        "name": "snapshot",
        "description": "A picture of the board.",
        "inputSchema": {"type": "object"},
    },
]


def reply(id_, result=None, error=None):
    message = {"jsonrpc": "2.0", "id": id_}
    if error is not None:
        message["error"] = error
    else:
        message["result"] = result
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def text(value, is_error=False):
    return {"content": [{"type": "text", "text": value}], "isError": is_error}


for line in sys.stdin:
    request = json.loads(line)
    method, id_ = request.get("method"), request.get("id")
    params = request.get("params") or {}
    if id_ is None:
        continue  # a notification
    if method == "initialize":
        reply(id_, {
            "protocolVersion": params.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "fake-board", "version": "0.1"},
        })
    elif method == "tools/list":
        reply(id_, {"tools": TOOLS})
    elif method == "tools/call":
        name, args = params.get("name"), params.get("arguments") or {}
        if name == "get_current_state":
            reply(id_, text(f"team {args.get('team', 'all')}: {json.dumps(STATE)}"))
        elif name == "move_card":
            if args.get("to") not in ("todo", "doing", "done"):
                reply(id_, text(f"no column `{args.get('to')}`", is_error=True))
            else:
                STATE["in_progress"] += {"todo": 0, "doing": 1, "done": -1}[args["to"]]
                reply(id_, text(f"moved card {args['card']} to {args['to']}"))
        elif name == "wait":
            time.sleep(5)
            reply(id_, text("waited"))
        elif name == "snapshot":
            reply(id_, {"content": [{"type": "text", "text": "the board:"},
                                    {"type": "image", "data": "AAAA", "mimeType": "image/png"}]})
        else:
            reply(id_, error={"code": -32602, "message": f"unknown tool {name}"})
    elif method == "resources/list":
        reply(id_, {"resources": [{"uri": "board://current", "name": "board"}]})
    elif method == "resources/read":
        reply(id_, {"contents": [{"uri": params["uri"], "mimeType": "application/json",
                                  "text": json.dumps(STATE)}]})
    else:
        reply(id_, error={"code": -32601, "message": f"no method {method}"})
