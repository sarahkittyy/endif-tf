#!/usr/bin/env python3
"""Tiny static server for the web build, serving ./dist the way production should.

    python tools/serve.py [--dir dist] [--port 8080] [--host 0.0.0.0]

Compared with `python -m http.server`: HTTP/1.1 with keep-alive and a thread per connection (no
six-connection queue of 75 asset fetches), the precompressed `.br` / `.gz` files written by
build-web.sh are served with the matching Content-Encoding, wasm/glb/ogg/ttf get the right
Content-Type, and cache headers match what web/nginx.example.conf does: `index.html` is
`no-cache`, everything else (build-id'd wasm/js, the content-hashed asset directory) is immutable.
No dependencies beyond the standard library.
"""
import argparse
import email.utils
import mimetypes
import os
import socket
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TYPES = {
    ".wasm": "application/wasm",
    ".js": "text/javascript",
    ".html": "text/html; charset=utf-8",
    ".css": "text/css",
    ".json": "application/json",
    ".png": "image/png",
    ".ogg": "audio/ogg",
    ".ttf": "font/ttf",
    ".glb": "model/gltf-binary",
    ".ico": "image/x-icon",
}
ENCODINGS = (("br", ".br"), ("gzip", ".gz"))
IMMUTABLE = "public, max-age=31536000, immutable"
NO_CACHE = "no-cache"
CHUNK = 1 << 16


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    root = "dist"

    def do_GET(self):
        self.serve(send_body=True)

    def do_HEAD(self):
        self.serve(send_body=False)

    def serve(self, send_body):
        path = urllib.parse.unquote(self.path.split("?", 1)[0].split("#", 1)[0])
        if path.endswith("/"):
            path += "index.html"
        rel = os.path.normpath(path.lstrip("/"))
        full = os.path.join(self.root, rel)
        if rel.startswith("..") or os.path.isabs(rel) or not os.path.isfile(full):
            self.reply(404, b"not found\n", "text/plain")
            return
        ext = os.path.splitext(full)[1].lower()
        ctype = TYPES.get(ext) or mimetypes.guess_type(full)[0] or "application/octet-stream"
        accepted = {t.strip().split(";")[0] for t in self.headers.get("Accept-Encoding", "").split(",")}
        encoding, send = None, full
        compressible = any(os.path.isfile(full + suffix) for _, suffix in ENCODINGS)
        for enc, suffix in ENCODINGS:
            if enc in accepted and os.path.isfile(full + suffix):
                encoding, send = enc, full + suffix
                break
        st = os.stat(send)
        modified = email.utils.formatdate(st.st_mtime, usegmt=True)
        if self.headers.get("If-Modified-Since") == modified:
            self.send_response(304)
            self.send_header("Content-Length", "0")
            self.end_headers()
            self.note(304, rel, encoding, 0)
            return
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(st.st_size))
        self.send_header("Last-Modified", modified)
        self.send_header("Cache-Control", NO_CACHE if rel == "index.html" else IMMUTABLE)
        if compressible:
            self.send_header("Vary", "Accept-Encoding")
        if encoding:
            self.send_header("Content-Encoding", encoding)
        self.end_headers()
        if send_body:
            try:
                with open(send, "rb") as f:
                    while True:
                        chunk = f.read(CHUNK)
                        if not chunk:
                            break
                        self.wfile.write(chunk)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                return
        self.note(200, rel, encoding, st.st_size)

    def reply(self, status, body, ctype):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.note(status, self.path, None, len(body))

    def note(self, status, rel, encoding, size):
        sys.stderr.write(f"{status} {str(rel).replace(os.sep, chr(47))} {encoding or 'identity'} {size / 1048576:.1f} MB\n")

    def log_message(self, *_):
        pass  # `note` above prints one line per response instead


def lan_ip():
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("10.255.255.255", 1))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except OSError:
        return None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--dir", default="dist")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--host", default="0.0.0.0")
    args = ap.parse_args()
    if not os.path.isfile(os.path.join(args.dir, "index.html")):
        sys.exit(f"no index.html in {args.dir!r}: run ./build-web.sh first (or pass --dir)")
    Handler.root = args.dir
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.daemon_threads = True
    print(f"serving {args.dir}/ on http://localhost:{args.port}", end="")
    ip = lan_ip()
    if ip and args.host == "0.0.0.0":
        print(f"  (LAN: http://{ip}:{args.port}; note WebGPU needs https:// or localhost)", end="")
    print()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
