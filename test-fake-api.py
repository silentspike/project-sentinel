"""Fake Anthropic API Endpoint — testet ob claude -p synthetisierte Responses akzeptiert."""
import json
import os
import uuid
from http.server import HTTPServer, BaseHTTPRequestHandler

class FakeAnthropicHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)

        # Log was reinkommt
        print(f"\n{'='*60}")
        print(f"POST {self.path}")
        print(f"Headers:")
        for k, v in self.headers.items():
            # API Key redacten
            if 'api-key' in k.lower() or 'authorization' in k.lower():
                v = v[:20] + '...[REDACTED]'
            print(f"  {k}: {v}")

        try:
            req = json.loads(body)
            print(f"\nRequest Body:")
            print(f"  model: {req.get('model')}")
            print(f"  max_tokens: {req.get('max_tokens')}")
            print(f"  stream: {req.get('stream')}")
            print(f"  system length: {len(json.dumps(req.get('system', '')))} chars")
            messages = req.get('messages', [])
            print(f"  messages: {len(messages)}")
            for i, msg in enumerate(messages):
                role = msg.get('role', '?')
                content = msg.get('content', '')
                if isinstance(content, str):
                    print(f"    [{i}] {role}: {content[:200]}")
                elif isinstance(content, list):
                    for block in content:
                        if block.get('type') == 'text':
                            print(f"    [{i}] {role}: {block['text'][:200]}")
        except Exception as e:
            print(f"  Body parse error: {e}")
            print(f"  Raw: {body[:500]}")

        # Synthetisierte Response
        fake_response = {
            "id": f"msg_{uuid.uuid4().hex[:24]}",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": os.environ.get(
                        "FAKE_ANTHROPIC_TEXT",
                        "Morgen wird es in Wien voraussichtlich sonnig mit Temperaturen um die 18 Grad, ideal fuer einen Spaziergang im Prater."
                    )
                }
            ],
            "model": req.get("model", "claude-opus-4-6"),
            "stop_reason": "end_turn",
            "stop_sequence": None,
            "usage": {
                "input_tokens": 150,
                "output_tokens": 35,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }

        response_body = json.dumps(fake_response)
        print(f"\nSending fake response: {len(response_body)} bytes")
        print(f"  content: {fake_response['content'][0]['text'][:100]}")

        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body.encode())

    def do_GET(self):
        # Health check
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"status":"fake-api-ok"}')

    def log_message(self, format, *args):
        pass  # Suppress default logging

if __name__ == '__main__':
    port = int(os.environ.get("FAKE_ANTHROPIC_PORT", "19876"))
    server = HTTPServer(('127.0.0.1', port), FakeAnthropicHandler)
    print(f"Fake Anthropic API listening on http://127.0.0.1:{port}")
    print(f"Route claude with: ANTHROPIC_BASE_URL=http://127.0.0.1:{port}")
    server.serve_forever()
