#!/usr/bin/env python3
"""Mock of the OpenAI-compatible /v1/chat/completions endpoint served by vLLM.

Lets the whole analyze-screen pipeline be tested without the real DGX box.

Response modes (toggle per-request with ?mode=..., or globally via POST /_mode
or the MOCK_MODE env var — per-request query param wins):

  valid    -> content is exactly the schema-conformant JSON object
  fenced   -> valid JSON wrapped in ```json fences plus prose (tests the
              sidecar's extraction pass)
  invalid  -> JSON with missing/mistyped fields (tests the LLM repair pass;
              the follow-up repair request is answered with valid JSON)
  broken   -> everything, including the repair pass, returns garbage (tests
              the structured-error path)

Repair requests are recognized by the REPAIR_MARKER string that the sidecar
puts in its repair prompt.

Run:  python3 mock_vlm_server.py [--port 8008]
"""

import argparse
import copy
import json
import os
import time
import uuid

import uvicorn
from fastapi import FastAPI, Request

REPAIR_MARKER = "Repair the following text"

VALID_ANALYSIS = {
    "screen_type": "streaming_app",
    "title": "Movie selection screen",
    "summary": (
        "A streaming app browse screen showing a grid of movie posters. "
        "A sci-fi title is currently focused."
    ),
    "detected_elements": [
        {
            "name": "poster_grid",
            "description": "Grid of 12 movie poster thumbnails",
            "confidence": 0.95,
        },
        {
            "name": "focused_title",
            "description": "Highlighted poster: 'Orbital Dawn' (2025)",
            "confidence": 0.88,
        },
    ],
    "suggested_actions": [
        "Press ENTER to open the focused title",
        "Scroll right to see more titles",
    ],
    "error": None,
}

# Parseable JSON, but violates the schema: missing summary/suggested_actions,
# confidence is a string, detected_elements items malformed. Extraction alone
# cannot fix this; only the repair pass can.
INVALID_ANALYSIS_TEXT = json.dumps(
    {
        "screen_type": "streaming_app",
        "title": "Movie selection screen",
        "detected_elements": [{"label": "poster_grid", "confidence": "high"}],
    }
)

GARBAGE_TEXT = "I looked at the screen but honestly { it's hard to say <<<"

app = FastAPI()
state = {"mode": os.environ.get("MOCK_MODE", "valid")}


def content_for(mode: str, is_repair: bool) -> str:
    if is_repair:
        # The repair LLM fixes things — unless the whole backend is 'broken'.
        if mode == "broken":
            return GARBAGE_TEXT
        return json.dumps(VALID_ANALYSIS)
    if mode == "valid":
        return json.dumps(VALID_ANALYSIS)
    if mode == "fenced":
        return (
            "Here is the analysis you asked for:\n\n"
            "```json\n" + json.dumps(VALID_ANALYSIS, indent=2) + "\n```\n\n"
            "Let me know if you need anything else!"
        )
    if mode == "invalid":
        return INVALID_ANALYSIS_TEXT
    if mode == "broken":
        return GARBAGE_TEXT
    return json.dumps(VALID_ANALYSIS)


@app.post("/v1/chat/completions")
async def chat_completions(request: Request):
    body = await request.json()
    mode = request.query_params.get("mode", state["mode"])
    flat = json.dumps(body.get("messages", []))
    is_repair = REPAIR_MARKER in flat
    has_image = '"image_url"' in flat
    print(
        f"[mock-vlm] {time.strftime('%H:%M:%S')} mode={mode} "
        f"repair={is_repair} image={has_image} model={body.get('model')}"
    )
    return {
        "id": f"chatcmpl-{uuid.uuid4().hex[:12]}",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": body.get("model", "mock"),
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content_for(mode, is_repair),
                },
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


@app.post("/_mode")
async def set_mode(request: Request):
    body = await request.json()
    state["mode"] = body.get("mode", "valid")
    return {"mode": state["mode"]}


@app.get("/_mode")
async def get_mode():
    return copy.copy(state)


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8008)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")
