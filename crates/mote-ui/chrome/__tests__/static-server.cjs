/**
 * static-server.cjs — minimal static file server for the Lane 1 harness.
 *
 * Serves crates/mote-ui/chrome/ over http://127.0.0.1:PORT so the Playwright
 * component harness can load real CSS + JS with proper relative paths.
 *
 * Usage: node chrome/__tests__/static-server.cjs [PORT]
 * Default port: 6175 (must match playwright.component.mjs STATIC_PORT).
 */
"use strict";

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

// Serve chrome/ from the repo — resolve relative to this file.
const CHROME_DIR = path.resolve(__dirname, "..");
const PORT = Number(process.argv[2] || 6175);

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css":  "text/css; charset=utf-8",
  ".js":   "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg":  "image/svg+xml",
};

const server = http.createServer(function (req, res) {
  var urlPath = req.url.split("?")[0];
  var decoded;
  try { decoded = decodeURIComponent(urlPath); }
  catch (_) { res.writeHead(400); res.end(); return; }

  var abs = path.resolve(CHROME_DIR, decoded.replace(/^\//, ""));
  // Safety: reject any path that escapes CHROME_DIR.
  if (!abs.startsWith(CHROME_DIR + path.sep) && abs !== CHROME_DIR) {
    res.writeHead(403); res.end(); return;
  }
  if (abs.endsWith(path.sep)) abs += "index.html";

  var data;
  try { data = fs.readFileSync(abs); }
  catch (_) { res.writeHead(404); res.end("not found: " + urlPath); return; }

  var ext = path.extname(abs);
  res.writeHead(200, { "Content-Type": MIME[ext] || "text/plain" });
  res.end(data);
});

server.listen(PORT, "127.0.0.1", function () {
  console.log("static server listening on http://127.0.0.1:" + PORT);
});
