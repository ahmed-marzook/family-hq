"""Turns Claude Code's `--output-format stream-json` into readable lines.

Used by claude-local.sh so a run can be followed live (`tail -f` on its log)
instead of only showing Claude's final answer when it ends. Lines that
aren't JSON are passed through unchanged.
"""

import json
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace", line_buffering=True)

# The input field worth showing for each tool; anything else shows its name.
KEY = {
    "Bash": "command",
    "Read": "file_path",
    "Edit": "file_path",
    "Write": "file_path",
    "Glob": "pattern",
    "Grep": "pattern",
}


def short(text, limit=200):
    text = " ".join(str(text).split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def show(event):
    kind = event.get("type")
    if kind == "system" and event.get("subtype") == "init":
        print(f"● session {event.get('session_id')} ({event.get('model')})")
    elif kind == "assistant":
        for block in event.get("message", {}).get("content", []):
            if block.get("type") == "text" and block.get("text", "").strip():
                print(f"  {block['text'].strip()}")
            elif block.get("type") == "tool_use":
                name = block.get("name", "?")
                arg = block.get("input", {}).get(KEY.get(name, ""), "")
                print(f"▸ {name} {short(arg)}".rstrip())
    elif kind == "user":
        for block in event.get("message", {}).get("content", []):
            if isinstance(block, dict) and block.get("is_error"):
                content = block.get("content")
                if isinstance(content, list):
                    content = " ".join(c.get("text", "") for c in content if isinstance(c, dict))
                print(f"  ✗ {short(content)}")
    elif kind == "result":
        print(f"■ {event.get('subtype')} after {event.get('num_turns')} turns")
        if event.get("result"):
            print(event["result"])


for line in sys.stdin:
    line = line.rstrip("\n")
    try:
        event = json.loads(line)
    except ValueError:
        print(line)
        continue
    if isinstance(event, dict):
        show(event)
