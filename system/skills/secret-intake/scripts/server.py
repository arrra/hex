#!/usr/bin/env python3
"""Reusable secret intake server. Serves a form, writes to .hex/secrets/, runs sync."""

import http.server
import json
import os
import subprocess
import sys
from pathlib import Path

PORT = int(os.environ.get("PORT", 9877))
HEX_DIR = Path(os.environ.get("HEX_DIR", "/Users/mrap/mrap-hex"))
SECRETS_DIR = HEX_DIR / ".hex" / "secrets"
SYNC_SCRIPT = HEX_DIR / ".hex" / "scripts" / "sync-secrets.sh"


def get_existing_institutions():
    """Return list of institution names already configured (from .env filenames)."""
    if not SECRETS_DIR.exists():
        return []
    names = set()
    for f in SECRETS_DIR.iterdir():
        if f.suffix == ".env" and f.name != ".gitkeep":
            names.add(f.stem.replace("-", " "))
    return sorted(names)


HTML_TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>hex — secret intake</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Text', system-ui, sans-serif;
    background: #FAF8F5;
    color: #1a1a1a;
    display: flex;
    justify-content: center;
    padding: 48px 24px;
    min-height: 100vh;
  }
  .container { max-width: 640px; width: 100%; }
  h1 { font-size: 20px; font-weight: 600; margin-bottom: 8px; }
  .subtitle { font-size: 14px; color: #666; margin-bottom: 24px; }
  .existing {
    background: #f4f2ee;
    border-radius: 10px;
    padding: 16px 20px;
    margin-bottom: 24px;
    font-size: 13px;
    color: #555;
  }
  .existing strong { color: #1a1a1a; }
  .existing .tags { margin-top: 8px; display: flex; flex-wrap: wrap; gap: 6px; }
  .existing .tag {
    background: #fff;
    border: 1px solid #e0ddd8;
    border-radius: 6px;
    padding: 3px 10px;
    font-family: 'SF Mono', 'Fira Code', monospace;
    font-size: 12px;
  }
  .institution {
    background: #fff;
    border: 1px solid #e0ddd8;
    border-radius: 12px;
    padding: 24px;
    margin-bottom: 16px;
  }
  .inst-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
  }
  .inst-header input {
    font-size: 16px;
    font-weight: 600;
    border: none;
    border-bottom: 2px solid #e0ddd8;
    background: transparent;
    padding: 4px 0;
    width: 70%;
    outline: none;
  }
  .inst-header input:focus { border-bottom-color: #1a1a1a; }
  .remove-inst {
    background: none;
    border: none;
    color: #999;
    cursor: pointer;
    font-size: 18px;
    padding: 4px 8px;
  }
  .remove-inst:hover { color: #c00; }
  .kv-row {
    display: flex;
    gap: 12px;
    margin-bottom: 8px;
    align-items: center;
  }
  .kv-row input {
    flex: 1;
    font-family: 'SF Mono', 'Fira Code', monospace;
    font-size: 13px;
    padding: 8px 12px;
    border: 1px solid #e0ddd8;
    border-radius: 8px;
    background: #FDFCFA;
    outline: none;
  }
  .kv-row input:focus { border-color: #1a1a1a; }
  .kv-row input.key-input { flex: 0.4; }
  .kv-row input.val-input { flex: 0.6; }
  .remove-kv {
    background: none;
    border: none;
    color: #ccc;
    cursor: pointer;
    font-size: 16px;
    padding: 0 4px;
    flex-shrink: 0;
  }
  .remove-kv:hover { color: #c00; }
  .add-btn {
    background: none;
    border: none;
    color: #666;
    cursor: pointer;
    font-size: 13px;
    padding: 4px 0;
    margin-top: 4px;
  }
  .add-btn:hover { color: #1a1a1a; }
  .file-upload-section {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid #f0ede8;
  }
  .file-upload-section label {
    font-size: 12px;
    color: #888;
    display: block;
    margin-bottom: 8px;
  }
  .file-row {
    display: flex;
    gap: 12px;
    margin-bottom: 8px;
    align-items: center;
  }
  .file-row input[type="text"] {
    flex: 0.4;
    font-family: 'SF Mono', 'Fira Code', monospace;
    font-size: 13px;
    padding: 8px 12px;
    border: 1px solid #e0ddd8;
    border-radius: 8px;
    background: #FDFCFA;
    outline: none;
  }
  .file-row input[type="file"] { flex: 0.6; font-size: 13px; }
  .actions { margin-top: 24px; }
  .add-inst {
    background: #fff;
    border: 2px dashed #e0ddd8;
    border-radius: 12px;
    padding: 16px;
    width: 100%;
    cursor: pointer;
    color: #999;
    font-size: 14px;
    text-align: center;
  }
  .add-inst:hover { border-color: #999; color: #666; }
  .submit {
    background: #1a1a1a;
    color: #FAF8F5;
    border: none;
    border-radius: 12px;
    padding: 16px 32px;
    font-size: 16px;
    font-weight: 600;
    cursor: pointer;
    width: 100%;
    margin-top: 12px;
  }
  .submit:hover { background: #333; }
  .submit:disabled { background: #999; cursor: not-allowed; }
  .warning {
    font-size: 12px;
    color: #999;
    margin-top: 16px;
    text-align: center;
  }
  .toast {
    position: fixed;
    bottom: 32px;
    left: 50%;
    transform: translateX(-50%) translateY(80px);
    background: #1a1a1a;
    color: #FAF8F5;
    padding: 14px 28px;
    border-radius: 12px;
    font-size: 14px;
    font-weight: 500;
    opacity: 0;
    transition: all 0.3s ease;
    pointer-events: none;
    z-index: 10;
  }
  .toast.show {
    opacity: 1;
    transform: translateX(-50%) translateY(0);
  }
  .toast.error { background: #8b0000; }
</style>
</head>
<body>
<div class="container">
  <h1>secret intake</h1>
  <p class="subtitle">Add or update credentials. Syncs to all hex surfaces on submit.</p>

  %%EXISTING%%

  <div id="institutions"></div>

  <div class="actions">
    <button class="add-inst" onclick="addInstitution()">+ add institution</button>
    <button class="submit" id="submit-btn" onclick="submit()">submit &amp; sync</button>
  </div>
  <p class="warning">Writes to .hex/secrets/ with 600 perms. Runs sync-secrets.sh after. Nothing logged.</p>
</div>

<div class="toast" id="toast"></div>

<script>
let instCount = 0;

function showToast(msg, isError) {
  const t = document.getElementById('toast');
  t.textContent = msg;
  t.className = 'toast show' + (isError ? ' error' : '');
  setTimeout(() => { t.className = 'toast'; }, 3000);
}

function addInstitution(name) {
  instCount++;
  const id = instCount;
  const div = document.createElement('div');
  div.className = 'institution';
  div.id = 'inst-' + id;
  div.innerHTML = `
    <div class="inst-header">
      <input type="text" placeholder="institution name (e.g. alpaca, coinbase)" class="inst-name" value="${name || ''}">
      <button class="remove-inst" onclick="this.closest('.institution').remove()" title="Remove">&times;</button>
    </div>
    <div class="kv-pairs" id="kvs-${id}">
      <div class="kv-row">
        <input type="text" class="key-input" placeholder="KEY_NAME">
        <input type="text" class="val-input" placeholder="value">
        <button class="remove-kv" onclick="this.closest('.kv-row').remove()">&times;</button>
      </div>
    </div>
    <button class="add-btn" onclick="addKV(${id})">+ add key</button>
    <div class="file-upload-section">
      <label>Key files (PEM, JSON, p12, etc.)</label>
      <div class="file-rows" id="files-${id}"></div>
      <button class="add-btn" onclick="addFile(${id})">+ attach file</button>
    </div>
  `;
  document.getElementById('institutions').appendChild(div);
  if (!name) div.querySelector('.inst-name').focus();
}

function addKV(instId) {
  const c = document.getElementById('kvs-' + instId);
  const row = document.createElement('div');
  row.className = 'kv-row';
  row.innerHTML = `
    <input type="text" class="key-input" placeholder="KEY_NAME">
    <input type="text" class="val-input" placeholder="value">
    <button class="remove-kv" onclick="this.closest('.kv-row').remove()">&times;</button>
  `;
  c.appendChild(row);
  row.querySelector('.key-input').focus();
}

function addFile(instId) {
  const c = document.getElementById('files-' + instId);
  const row = document.createElement('div');
  row.className = 'file-row';
  row.innerHTML = `
    <input type="text" placeholder="filename (e.g. key.pem)">
    <input type="file">
    <button class="remove-kv" onclick="this.closest('.file-row').remove()">&times;</button>
  `;
  c.appendChild(row);
}

async function submit() {
  const btn = document.getElementById('submit-btn');
  btn.disabled = true;
  btn.textContent = 'writing...';

  const institutions = document.querySelectorAll('.institution');
  const payload = [];

  for (const inst of institutions) {
    const name = inst.querySelector('.inst-name').value.trim();
    if (!name) continue;

    const kvs = {};
    for (const row of inst.querySelectorAll('.kv-row')) {
      const k = row.querySelector('.key-input').value.trim();
      const v = row.querySelector('.val-input').value.trim();
      if (k && v) kvs[k] = v;
    }

    const files = [];
    for (const fr of inst.querySelectorAll('.file-row')) {
      const fname = fr.querySelector('input[type="text"]').value.trim();
      const fi = fr.querySelector('input[type="file"]');
      if (fname && fi.files.length > 0) {
        files.push({ name: fname, content: await fi.files[0].text() });
      }
    }

    if (Object.keys(kvs).length > 0 || files.length > 0) {
      payload.push({ institution: name, env_vars: kvs, files: files });
    }
  }

  if (payload.length === 0) {
    btn.disabled = false;
    btn.textContent = 'submit & sync';
    showToast('Nothing to write — add at least one key or file.', true);
    return;
  }

  try {
    const resp = await fetch('submit', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });
    const data = await resp.json();
    if (resp.ok) {
      showToast(`Wrote ${data.written} file(s). Sync ${data.synced ? 'done' : 'skipped'}.`);
      document.getElementById('institutions').innerHTML = '';
      addInstitution();
    } else {
      showToast('Error: ' + (data.error || 'unknown'), true);
    }
  } catch (e) {
    showToast('Connection error: ' + e.message, true);
  }

  btn.disabled = false;
  btn.textContent = 'submit & sync';
}

addInstitution();
</script>
</body>
</html>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        if self.path in ("/", "/intake"):
            existing = get_existing_institutions()
            if existing:
                tags = "".join(f'<span class="tag">{n}</span>' for n in existing)
                block = (
                    '<div class="existing">'
                    f"<strong>Already configured ({len(existing)})</strong>"
                    f'<div class="tags">{tags}</div>'
                    "</div>"
                )
            else:
                block = ""
            html = HTML_TEMPLATE.replace("%%EXISTING%%", block)
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(html.encode())
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path != "/submit":
            self.send_error(404)
            return

        length = int(self.headers.get("Content-Length", 0))
        try:
            payload = json.loads(self.rfile.read(length))
        except (json.JSONDecodeError, ValueError):
            self._json(400, {"error": "Invalid JSON"})
            return

        written = 0
        for inst in payload:
            name = inst.get("institution", "").strip().lower().replace(" ", "-")
            if not name:
                continue

            env_vars = inst.get("env_vars", {})
            if env_vars:
                env_path = SECRETS_DIR / f"{name}.env"
                existing = {}
                if env_path.exists():
                    for line in env_path.read_text().splitlines():
                        if "=" in line and not line.startswith("#"):
                            k, v = line.split("=", 1)
                            existing[k.strip()] = v.strip()
                existing.update(env_vars)
                env_path.write_text(
                    "\n".join(f"{k}={v}" for k, v in existing.items()) + "\n"
                )
                env_path.chmod(0o600)
                written += 1

            for f in inst.get("files", []):
                fname = f.get("name", "").strip()
                content = f.get("content", "")
                if fname and content:
                    fpath = SECRETS_DIR / f"{name}-{fname}"
                    fpath.write_text(content)
                    fpath.chmod(0o600)
                    written += 1

        synced = False
        if written > 0 and SYNC_SCRIPT.exists():
            try:
                subprocess.run(
                    ["bash", str(SYNC_SCRIPT)],
                    capture_output=True, timeout=30
                )
                synced = True
            except Exception:
                pass

        self._json(200, {"written": written, "synced": synced})

    def _json(self, code, data):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data).encode())


def main():
    SECRETS_DIR.mkdir(parents=True, exist_ok=True)
    server = http.server.HTTPServer(("0.0.0.0", PORT), Handler)
    print(f"secret-intake listening on :{PORT}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        print("shut down.", flush=True)


if __name__ == "__main__":
    main()
