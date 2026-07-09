#!/usr/bin/env python3
"""
Mock VLM server for testing the analyze-screen sidecar.

Simulates a vLLM OpenAI-compatible endpoint with configurable responses:
- valid: Returns proper JSON matching the schema
- fenced: Returns JSON wrapped in markdown fences
- invalid: Returns malformed JSON that needs LLM repair
- broken: Returns completely invalid output

The mode can be changed via POST /_mode endpoint.
"""

import argparse
import json
from http import HTTPStatus

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel

app = FastAPI(title="Mock VLM Server")

# Current mode - determines what kind of response to return
CURRENT_MODE = {"mode": "valid"}

# Valid response template matching the expected schema
VALID_RESPONSE = {
    "screen_type": "home",
    "title": "Main Menu",
    "summary": "TV home screen with app icons and navigation menu",
    "detected_elements": [
        {"name": "Netflix", "description": "Streaming app icon", "confidence": 0.95},
        {"name": "Settings", "description": "System settings button", "confidence": 0.92},
        {"name": "Live TV", "description": "Live television entry point", "confidence": 0.88},
    ],
    "suggested_actions": ["Open Netflix", "Go to Settings", "Watch Live TV"],
}

# Fenced response - JSON wrapped in markdown
FENCED_RESPONSE = """```json
{
    "screen_type": "video",
    "title": "Now Playing",
    "summary": "Video player showing movie content",
    "detected_elements": [
        {"name": "Play/Pause", "description": "Playback control button", "confidence": 0.98},
        {"name": "Progress Bar", "description": "Video timeline scrubber", "confidence": 0.94},
        {"name": "Volume", "description": "Audio level control", "confidence": 0.91}
    ],
    "suggested_actions": ["Pause video", "Adjust volume", "Skip forward"]
}
```"""

# Invalid JSON - missing quotes, wrong structure
INVALID_JSON = """{
    screen_type: "settings",
    "title": Settings Menu,
    "summary": "System configuration screen",
    "detected_elements": [
        {"name": "Network", description: "Network settings", "confidence": 0.9},
        {"name": "Display", "description": "Display settings", "confidence": 0.85}
    ],
    "suggested_actions": ["Configure WiFi", "Adjust brightness"]
}"""

# Broken response - completely invalid
BROKEN_RESPONSE = "This is not JSON at all. Just plain text garbage."

# Repair response - what the LLM should return when fixing invalid JSON
REPAIR_RESPONSE = {
    "screen_type": "settings",
    "title": "Settings Menu",
    "summary": "System configuration screen with network and display options",
    "detected_elements": [
        {"name": "Network", "description": "Network settings", "confidence": 0.9},
        {"name": "Display", "description": "Display settings", "confidence": 0.85}
    ],
    "suggested_actions": ["Configure WiFi", "Adjust brightness"],
}


class ChatRequest(BaseModel):
    model: str
    messages: list[dict]
    max_tokens: int = 2048
    temperature: float = 0.1


@app.get("/health")
async def health():
    """Health check endpoint."""
    return {"status": "ok", "mode": CURRENT_MODE["mode"]}


@app.post("/_mode")
async def set_mode(request: Request):
    """Set the response mode for testing."""
    try:
        body = await request.json()
        mode = body.get("mode", "valid")
        if mode not in ("valid", "fenced", "invalid", "broken"):
            return JSONResponse({"error": f"Unknown mode: {mode}"}, status_code=400)
        CURRENT_MODE["mode"] = mode
        return {"mode": mode}
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=400)


@app.post("/v1/chat/completions")
async def chat_completions(request: ChatRequest):
    """Mock OpenAI-compatible chat completions endpoint."""
    mode = CURRENT_MODE["mode"]

    # Check if this is a repair request (system prompt mentions "repair")
    is_repair = False
    for msg in request.messages:
        if msg.get("role") == "system":
            content = msg.get("content", "")
            # Content can be a string or a list of content parts
            if isinstance(content, list):
                content = " ".join(part.get("text", "") for part in content if isinstance(part, dict))
            if "repair" in content.lower():
                is_repair = True
                break

    if is_repair:
        # For repair requests, always return valid JSON
        content = json.dumps(REPAIR_RESPONSE)
    elif mode == "valid":
        content = json.dumps(VALID_RESPONSE)
    elif mode == "fenced":
        content = FENCED_RESPONSE
    elif mode == "invalid":
        content = INVALID_JSON
    elif mode == "broken":
        content = BROKEN_RESPONSE
    else:
        content = json.dumps(VALID_RESPONSE)

    return {
        "id": "mock-chat-123",
        "object": "chat.completion",
        "created": 1234567890,
        "model": request.model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content,
                },
                "finish_reason": "stop",
            }
        ],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
        },
    }


if __name__ == "__main__":
    import uvicorn

    parser = argparse.ArgumentParser(description="Mock VLM server for testing")
    parser.add_argument("--port", type=int, default=8008, help="Port to listen on")
    parser.add_argument("--host", type=str, default="127.0.0.1", help="Host to bind to")
    args = parser.parse_args()

    uvicorn.run(app, host=args.host, port=args.port, log_level="warning")
