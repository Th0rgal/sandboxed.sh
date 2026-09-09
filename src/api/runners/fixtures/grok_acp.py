"""Offline ACP peer for Grok runner lifecycle tests; never invokes a model/tool."""

import json
import os
import sys
import time


def emit(value):
    print(json.dumps(value), flush=True)


def update(value):
    emit({"jsonrpc": "2.0", "method": "session/update",
          "params": {"sessionId": "fixture-session", "update": value}})


scenario = sys.argv[1]
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    request_id = request.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1}
    elif method in ("session/new", "session/load"):
        result = {"sessionId": "fixture-session"}
    elif method == "session/set_model":
        result = {}
    elif method == "session/prompt":
        with open("accepted-prompts", "a", encoding="utf-8") as receipt:
            receipt.write("accepted\n")
        print("fixture stderr diagnostic", file=sys.stderr, flush=True)
        if scenario == "silent_success":
            time.sleep(0.30)
        elif scenario in ("long_tool", "cancel", "completed_tool_dead", "missing_status", "expired_tool"):
            call = {"sessionUpdate": "tool_call", "toolCallId": "build-1",
                    "title": "run_terminal_command", "kind": "execute", "status": "pending",
                    "rawInput": {"command": "fixture build", "timeout": 1200}}
            if scenario == "missing_status":
                call.pop("status")
            update(call)
            if scenario != "missing_status":
                update({"sessionUpdate": "tool_call_update", "toolCallId": "build-1",
                        "status": "in_progress"})
            if scenario == "completed_tool_dead":
                update({"sessionUpdate": "tool_call_update", "toolCallId": "build-1",
                        "status": "completed"})
            time.sleep(0.80 if scenario == "long_tool" else 3)
            update({"sessionUpdate": "tool_call_update", "toolCallId": "build-1",
                    "status": "completed"})
        elif scenario == "junk":
            for _ in range(40):
                print("not a protocol event", flush=True)
                emit({"unrelated": True})
                time.sleep(0.05)
        elif scenario == "input":
            emit({"jsonrpc": "2.0", "id": 22, "method": "session/request_input",
                  "params": {"sessionId": "fixture-session"}})
            time.sleep(3)
        elif scenario == "eof":
            os.close(sys.stdout.fileno())
            time.sleep(3)
            break
        elif scenario == "prompt_error":
            emit({"jsonrpc": "2.0", "id": request_id,
                  "error": {"code": -32000, "message": "fixture provider failure"}})
            break
        else:
            time.sleep(3)
        update({"sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "fixture completed"}})
        result = {"stopReason": "end_turn", "_meta": {"modelId": "fixture-model"}}
        emit({"jsonrpc": "2.0", "id": request_id, "result": result})
        break
    else:
        continue
    emit({"jsonrpc": "2.0", "id": request_id, "result": result})
