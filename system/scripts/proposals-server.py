"""
proposals-server — serves brand proposals as a browsable gallery.
Port 8898. Proposals dir: projects/brand/proposals/
Samples dir: projects/brand/proposals/samples/ (JSON files, auto-rendered)
Comments: proxied to universal comments service (port 8901)
"""

import html
import http.server
import json
import os
import re
import socketserver
import urllib.request
from pathlib import Path
from datetime import datetime, timezone

import sys as _sys
_sys.path.insert(0, str(Path(__file__).parent))
from lib.hex_utils import get_hex_root

PORT = 8898
PROPOSALS_DIR = get_hex_root() / "projects" / "brand" / "proposals"
SAMPLES_DIR = PROPOSALS_DIR / "samples"
SAMPLES_V3_DIR = PROPOSALS_DIR / "samples-v3"
REACTIONS_FILE = PROPOSALS_DIR / "reactions.json"
UNIVERSAL_URL = "http://127.0.0.1:8901/api/comments"


def _load_reactions() -> dict:
    try:
        return json.loads(REACTIONS_FILE.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}

def _save_reactions(data: dict):
    REACTIONS_FILE.write_text(json.dumps(data, indent=2))

def _fetch_comments_for_surface(surface: str) -> dict:
    """Fetch comments from universal service keyed by item_id within the surface."""
    try:
        with urllib.request.urlopen(UNIVERSAL_URL + "/all", timeout=2) as r:
            data = json.loads(r.read())
        prefix = f"{surface}:"
        result: dict = {}
        for c in data.get("comments", []):
            asset = c.get("asset", "")
            if asset.startswith(prefix):
                item_id = asset[len(prefix):]
                result.setdefault(item_id, []).append(c)
        return result
    except Exception:
        return {}


INDEX_HTML = """<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>hex proposals</title>
<script src="/comments/widget.js"></script>
<link href="https://fonts.googleapis.com/css2?family=Fraunces:wght@500;700&family=Work+Sans:wght@300;400;500&display=swap" rel="stylesheet">
<style>
body {{
  background: #faf7f0; color: #1c1a16; margin: 0; min-height: 100vh;
  font-family: 'Work Sans', -apple-system, sans-serif;
  display: flex; align-items: center; justify-content: center;
}}
main {{ max-width: 640px; padding: 3rem 2rem; width: 100%; }}
h1 {{
  font-family: 'Fraunces', serif; font-size: 1.8rem;
  font-weight: 700; letter-spacing: -0.02em; margin: 0 0 0.3rem;
}}
h1 span {{ color: #c4553a; }}
.sub {{ color: #6e695f; font-size: 0.9rem; margin: 0 0 2rem; }}
.empty {{ color: #8a8880; font-style: italic; font-size: 0.9rem; }}
.list {{ display: grid; gap: 0; border: 1px solid #e8e0d0; border-radius: 8px; overflow: hidden; background: #fff; }}
a.card {{
  display: block; padding: 1.1rem 1.3rem;
  text-decoration: none; color: #1c1a16;
  border-bottom: 1px solid #e8e0d0; transition: background 0.15s;
}}
a.card:last-child {{ border-bottom: 0; }}
a.card:hover {{ background: #fcf5e5; }}
a.card .title {{
  font-family: 'Fraunces', serif; font-size: 1.05rem;
  font-weight: 600; margin: 0 0 0.2rem; color: #1c1a16;
}}
a.card .meta {{
  font-size: 0.78rem; color: #8a8880;
}}
a.card .arrow {{ float: right; color: #c4553a; opacity: 0.6; font-size: 1.1rem; margin-top: 0.2rem; }}
footer {{
  margin-top: 1.5rem;
  font-family: "SF Mono", Menlo, Consolas, monospace;
  font-size: 0.7rem; color: #8a8880;
}}
</style>
</head><body>
<main>
  <h1><span>hex</span> proposals</h1>
  <p class="sub">Brand strategy decks and pitch materials.</p>
  {content}
  <footer>hex-router /proposals</footer>
</main>
</body></html>"""


SAMPLES_CSS = """
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
body {
    background: #FAF6F0; color: #2C2C2C;
    font-family: 'Work Sans', -apple-system, sans-serif; line-height: 1.5;
}
.header { max-width: 800px; margin: 0 auto; padding: 4rem 2rem 2rem; }
.header .tag {
    display: inline-block; font-size: 0.72rem; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.12em; padding: 0.2em 0.7em;
    border: 1.5px solid #C4553A; color: #C4553A;
}
.header h1 {
    font-family: 'Fraunces', serif; font-size: clamp(1.8rem, 4vw, 3rem);
    font-weight: 700; letter-spacing: -0.02em; margin: 0.4em 0 0.2em;
}
.header .sub { font-size: 1rem; color: #5A5A5A; max-width: 55ch; }
.divider { width: 60px; height: 3px; background: #C4553A; margin: 1.5rem 0; }
.section { max-width: 800px; margin: 0 auto; padding: 1rem 2rem 3rem; }
.section-label { font-family: 'Fraunces', serif; font-size: 1.3rem; font-weight: 600; margin-bottom: 0.3rem; }
.section-desc { font-size: 0.85rem; color: #5A5A5A; margin-bottom: 1.5rem; }
.post-card {
    background: #fff; border: 1px solid #D9D3CB; border-radius: 12px;
    padding: 1.5rem; margin-bottom: 1.5rem; position: relative;
}
.post-card::before {
    content: ''; position: absolute; top: 0; left: 0; right: 0;
    height: 3px; border-radius: 12px 12px 0 0;
}
.post-card.x::before { background: #1D9BF0; }
.post-card.linkedin::before { background: #0A66C2; }
.post-card.thread-card::before { background: #C4553A; }
.post-meta { display: flex; align-items: center; gap: 0.6rem; margin-bottom: 0.8rem; }
.post-avatar {
    width: 40px; height: 40px; border-radius: 50%; background: #1E1E1E;
    display: flex; align-items: center; justify-content: center;
    color: #FAF6F0; font-family: 'Fraunces', serif; font-weight: 700; font-size: 0.9rem;
}
.post-name { font-weight: 600; font-size: 0.9rem; }
.post-handle { font-size: 0.8rem; color: #5A5A5A; }
.post-platform {
    margin-left: auto; font-size: 0.7rem; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.08em; padding: 0.2em 0.5em; border-radius: 4px;
}
.post-platform.x-plat { background: #E8F5FE; color: #1D9BF0; }
.post-platform.li-plat { background: #E8F0F8; color: #0A66C2; }
.post-platform.thread-plat { background: #FBEEE8; color: #C4553A; }
.post-body { font-size: 0.95rem; line-height: 1.55; white-space: pre-line; }
.post-body strong { font-weight: 600; }
.post-context {
    margin-top: 1rem; padding-top: 0.8rem; border-top: 1px solid #D9D3CB;
    font-size: 0.78rem; color: #5A5A5A;
}
.post-context .label {
    font-weight: 600; color: #7A8B6F; text-transform: uppercase;
    letter-spacing: 0.06em; font-size: 0.7rem;
}
.post-image {
    margin: 0.8rem 0; border-radius: 8px; overflow: hidden;
    border: 1px solid #D9D3CB;
}
.post-image img {
    width: 100%; height: auto; display: block;
    max-height: 500px; object-fit: contain; background: #F5F0E8;
}
.thread-container {
    background: #fff; border: 1px solid #D9D3CB; border-radius: 12px;
    overflow: hidden; margin-bottom: 1.5rem; position: relative;
}
.thread-container::before {
    content: ''; position: absolute; top: 0; left: 0; right: 0;
    height: 3px; background: #C4553A; border-radius: 12px 12px 0 0;
}
.thread-header { padding: 1.2rem 1.5rem 0.8rem; display: flex; align-items: center; gap: 0.6rem; }
.thread-tweet { padding: 0 1.5rem 1rem; position: relative; }
.thread-tweet:not(:last-of-type) { padding-bottom: 1.2rem; }
.thread-tweet:not(:last-of-type)::after {
    content: ''; position: absolute; left: 2.7rem; bottom: 0;
    width: 2px; height: 0.8rem; background: #D9D3CB;
}
.thread-tweet .num {
    position: absolute; left: 1.5rem; top: 0;
    font-family: 'Fraunces', serif; font-size: 0.75rem; color: #C4553A;
    font-weight: 600; width: 20px; text-align: center;
}
.thread-tweet .text { margin-left: 2rem; font-size: 0.92rem; line-height: 1.5; }
.thread-context {
    padding: 0.8rem 1.5rem; border-top: 1px solid #D9D3CB;
    background: #FAF6F0; font-size: 0.78rem; color: #5A5A5A;
}
.format-tag {
    display: inline-block; font-size: 0.65rem; font-weight: 600;
    text-transform: uppercase; letter-spacing: 0.1em; padding: 0.15em 0.5em;
    border-radius: 3px; margin-left: 0.5rem; vertical-align: middle;
}
.format-tag.hook { background: #FEF3C7; color: #92400E; }
.format-tag.numbers { background: #DBEAFE; color: #1E40AF; }
.format-tag.failure { background: #FEE2E2; color: #991B1B; }
.format-tag.framework { background: #D1FAE5; color: #065F46; }
.format-tag.contrarian { background: #EDE9FE; color: #5B21B6; }

/* ─── Shortcut help ─── */
.kb-help {
    position: fixed; bottom: 1rem; right: 1rem;
    color: #b0a99f; border-radius: 6px;
    padding: 0.4rem 0.7rem; font-size: 0.65rem; line-height: 1.6;
    z-index: 100; opacity: 0.5; transition: opacity 0.2s;
}
.kb-help:hover { opacity: 0.85; }
.kb-help .title { display: none; }
.kb-help kbd {
    display: inline-block; min-width: 1.1em; text-align: center;
    color: #8a8880; font-family: 'SF Mono', Menlo, monospace; font-size: 0.6rem;
    margin-right: 0.2rem;
}
.kb-help-close { display: none; }

/* ─── Keyboard selection ─── */
.post-card.kb-selected, .thread-container.kb-selected {
    outline: 2.5px solid #C4553A; outline-offset: 2px;
}

/* ─── Reaction buttons ─── */
.reaction-row {
    display: flex; gap: 0.4rem; margin-top: 0.8rem; align-items: center;
}
.react-btn {
    display: inline-flex; align-items: center; gap: 0.25rem;
    padding: 0.35em 0.7em; font-size: 0.8rem; font-weight: 500;
    border: 1.5px solid #D9D3CB; border-radius: 20px; background: #fff;
    cursor: pointer; color: #5A5A5A; transition: all 0.15s;
}
.react-btn:hover { border-color: #8a8880; color: #2C2C2C; }
.react-btn.active-like { border-color: #4CAF50; color: #4CAF50; background: #E8F5E9; }
.react-btn.active-dislike { border-color: #E57373; color: #E57373; background: #FFEBEE; }
.react-btn.active-meh { border-color: #BDBDBD; color: #9E9E9E; background: #F5F5F5; }
.react-btn .emoji { font-size: 1rem; }
.react-divider { width: 1px; height: 18px; background: #D9D3CB; margin: 0 0.2rem; }

/* ─── Comment thread ─── */
.comment-toggle {
    display: inline-flex; align-items: center; gap: 0.3rem;
    margin-top: 0.6rem; padding: 0.3em 0.6em;
    font-size: 0.75rem; font-weight: 500; color: #5A5A5A;
    background: none; border: 1px solid #D9D3CB; border-radius: 6px;
    cursor: pointer; transition: all 0.15s;
}
.comment-toggle:hover { background: #F0EBE3; color: #2C2C2C; }
.comment-toggle .count { font-weight: 600; color: #C4553A; }
.comment-thread {
    display: none; margin-top: 0.8rem; padding-top: 0.8rem;
    border-top: 1px solid #E8E0D0;
}
.comment-thread.open { display: block; }
.comment-list { display: flex; flex-direction: column; gap: 0.6rem; margin-bottom: 0.8rem; }
.comment {
    display: flex; gap: 0.5rem; align-items: flex-start;
}
.comment-avatar {
    width: 26px; height: 26px; min-width: 26px; border-radius: 50%;
    display: flex; align-items: center; justify-content: center;
    font-size: 0.6rem; font-weight: 700; color: #FAF6F0;
}
.comment-avatar.mike { background: #1E1E1E; }
.comment-avatar.brand { background: #C4553A; }
.comment-bubble {
    background: #F5F0E8; border-radius: 8px; padding: 0.5rem 0.7rem;
    font-size: 0.82rem; line-height: 1.45; flex: 1; position: relative;
}
.comment-bubble.brand-bubble { background: #FBEEE8; }
.comment-bubble .comment-author {
    font-weight: 600; font-size: 0.72rem; margin-bottom: 0.15rem;
}
.comment-bubble .comment-time {
    font-size: 0.65rem; color: #8a8880; margin-top: 0.2rem;
}
.comment-input-row {
    display: flex; gap: 0.4rem; align-items: flex-end;
}
.comment-input {
    flex: 1; border: 1px solid #D9D3CB; border-radius: 8px;
    padding: 0.5rem 0.7rem; font-size: 0.82rem; font-family: inherit;
    resize: none; min-height: 36px; max-height: 120px;
    background: #fff; outline: none; transition: border-color 0.15s;
}
.comment-input:focus { border-color: #C4553A; }
.comment-send {
    padding: 0.45rem 0.8rem; background: #C4553A; color: #FAF6F0;
    border: none; border-radius: 8px; font-size: 0.78rem; font-weight: 600;
    cursor: pointer; white-space: nowrap; transition: background 0.15s;
}
.comment-send:hover { background: #a8442e; }
.comment-send:disabled { background: #D9D3CB; cursor: default; }

footer {
    max-width: 800px; margin: 0 auto; padding: 1rem 2rem 3rem;
    font-family: "SF Mono", Menlo, monospace; font-size: 0.7rem; color: #8a8880;
}
"""

COMMENTS_JS = """
function react(id, reaction) {
    fetch('/proposals/api/reactions', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({item: id, reaction: reaction})
    })
    .then(r => r.json())
    .then(data => {
        const row = document.querySelector('[data-reaction-id="' + id + '"]');
        if (!row) return;
        const likeBtn = row.querySelector('.like-btn');
        const dislikeBtn = row.querySelector('.dislike-btn');
        likeBtn.classList.toggle('active-like', data.reaction === 'like');
        dislikeBtn.classList.toggle('active-dislike', data.reaction === 'dislike');
        advanceIfComplete();
    });
}

function toggleThread(id) {
    const el = document.getElementById('thread-' + id);
    el.classList.toggle('open');
}

function formatTime(ts) {
    const d = new Date(ts);
    const now = new Date();
    const diff = (now - d) / 1000;
    if (diff < 60) return 'just now';
    if (diff < 3600) return Math.floor(diff/60) + 'm ago';
    if (diff < 86400) return Math.floor(diff/3600) + 'h ago';
    return d.toLocaleDateString('en-US', {month:'short', day:'numeric'});
}

function renderComment(c) {
    const isMike = c.author === 'mike';
    const avatarClass = isMike ? 'mike' : 'brand';
    const bubbleClass = isMike ? '' : 'brand-bubble';
    const initials = isMike ? 'MR' : 'B';
    const name = isMike ? 'Mike' : 'Brand Agent';
    return `<div class="comment">
        <div class="comment-avatar ${avatarClass}">${initials}</div>
        <div class="comment-bubble ${bubbleClass}">
            <div class="comment-author">${name}</div>
            <div>${c.text.replace(/\\n/g, '<br>')}</div>
            <div class="comment-time">${formatTime(c.ts)}</div>
        </div>
    </div>`;
}

function loadComments(id) {
    fetch('/proposals/api/comments/' + id)
        .then(r => r.json())
        .then(comments => {
            const list = document.getElementById('comments-' + id);
            list.innerHTML = comments.map(renderComment).join('');
            const btn = document.querySelector('[data-id="' + id + '"] .count');
            if (btn) btn.textContent = comments.length || '';
        });
}

function postComment(id) {
    const input = document.getElementById('input-' + id);
    const text = input.value.trim();
    if (!text) return;
    const btn = input.nextElementSibling;
    btn.disabled = true;
    fetch('/proposals/api/comments', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({item: id, text: text, author: 'mike'})
    })
    .then(r => r.json())
    .then(() => {
        input.value = '';
        input.style.height = '36px';
        btn.disabled = false;
        input.blur();
        loadComments(id);
        advanceIfComplete();
    })
    .catch(() => { btn.disabled = false; });
}

// Auto-resize textareas
document.addEventListener('input', function(e) {
    if (e.target.classList.contains('comment-input')) {
        e.target.style.height = '36px';
        e.target.style.height = e.target.scrollHeight + 'px';
    }
});

// ─── Keyboard navigation ───
let currentIdx = -1;
let allCards = [];

function isReviewed(card) {
    const row = card.querySelector('[data-reaction-id]');
    const hasReaction = row && (row.querySelector('.active-like') || row.querySelector('.active-dislike'));
    const countEl = card.querySelector('.comment-toggle .count');
    const hasComment = countEl && countEl.textContent.trim() !== '';
    return hasReaction && hasComment;
}

function findNextUnreviewed() {
    for (let i = currentIdx + 1; i < allCards.length; i++) {
        if (!isReviewed(allCards[i])) return i;
    }
    return currentIdx;
}

function reactAndAdvance(id, reaction) {
    fetch('/proposals/api/reactions', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({item: id, reaction: reaction})
    })
    .then(r => r.json())
    .then(data => {
        const row = document.querySelector('[data-reaction-id="' + id + '"]');
        if (row) {
            const likeBtn = row.querySelector('.like-btn');
            const dislikeBtn = row.querySelector('.dislike-btn');
            const mehBtn = row.querySelector('.meh-btn');
            likeBtn.classList.toggle('active-like', data.reaction === 'like');
            dislikeBtn.classList.toggle('active-dislike', data.reaction === 'dislike');
            if (mehBtn) mehBtn.classList.toggle('active-meh', data.reaction === 'meh');
        }
        selectCard(currentIdx + 1);
    });
}

function advanceIfComplete() {
    if (currentIdx >= 0 && isReviewed(allCards[currentIdx])) {
        setTimeout(() => { selectCard(findNextUnreviewed()); }, 150);
    }
}

function initKeyboard() {
    allCards = Array.from(document.querySelectorAll('.post-card, .thread-container'));
    if (allCards.length === 0) return;
    selectCard(findNextUnreviewed());
}

function selectCard(idx) {
    if (idx < 0 || idx >= allCards.length) return;
    if (currentIdx >= 0 && allCards[currentIdx]) allCards[currentIdx].classList.remove('kb-selected');
    currentIdx = idx;
    allCards[currentIdx].classList.add('kb-selected');
    allCards[currentIdx].scrollIntoView({behavior: 'smooth', block: 'center'});
}

document.addEventListener('keydown', function(e) {
    if (e.target.tagName === 'TEXTAREA' || e.target.tagName === 'INPUT') {
        if (e.key === 'Escape') { e.target.blur(); e.preventDefault(); return; }
        if (e.target.classList.contains('comment-input') && e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            const id = e.target.id.replace('input-', '');
            postComment(id);
        }
        return;
    }
    if (e.key === 'j') { e.preventDefault(); selectCard(currentIdx + 1); }
    else if (e.key === 'k') { e.preventDefault(); selectCard(currentIdx - 1); }
    else if (e.key === 'ArrowRight' || e.key === 'l' || e.key === 'y') {
        e.preventDefault();
        if (currentIdx < 0) return;
        const row = allCards[currentIdx].querySelector('[data-reaction-id]');
        if (row) reactAndAdvance(row.dataset.reactionId, 'like');
    }
    else if (e.key === 'ArrowLeft' || e.key === 'd' || e.key === 'n') {
        e.preventDefault();
        if (currentIdx < 0) return;
        const row = allCards[currentIdx].querySelector('[data-reaction-id]');
        if (row) reactAndAdvance(row.dataset.reactionId, 'dislike');
    }
    else if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (currentIdx < 0) return;
        const row = allCards[currentIdx].querySelector('[data-reaction-id]');
        if (row) reactAndAdvance(row.dataset.reactionId, 'meh');
        else selectCard(currentIdx + 1);
    }
    else if (e.key === 'ArrowUp') { e.preventDefault(); selectCard(currentIdx - 1); }
    else if (e.key === '?') {
        const h = document.getElementById('kb-help');
        h.style.display = h.style.display === 'none' ? '' : 'none';
    }
    else if (e.key === 'c') {
        if (currentIdx < 0) return;
        const toggle = allCards[currentIdx].querySelector('.comment-toggle');
        if (toggle) { toggle.click(); setTimeout(() => { const inp = allCards[currentIdx].querySelector('.comment-input'); if (inp) inp.focus(); }, 100); }
    }
});

document.addEventListener('DOMContentLoaded', function() {
    initKeyboard();
    let scrollTimer = null;
    window.addEventListener('scroll', function() {
        if (scrollTimer) clearTimeout(scrollTimer);
        scrollTimer = setTimeout(function() {
            const mid = window.innerHeight / 2;
            let closest = 0, closestDist = Infinity;
            allCards.forEach(function(card, i) {
                const rect = card.getBoundingClientRect();
                const dist = Math.abs(rect.top + rect.height / 2 - mid);
                if (dist < closestDist) { closestDist = dist; closest = i; }
            });
            if (closest !== currentIdx) {
                if (currentIdx >= 0 && allCards[currentIdx]) allCards[currentIdx].classList.remove('kb-selected');
                currentIdx = closest;
                allCards[currentIdx].classList.add('kb-selected');
            }
        }, 100);
    });
});

// Load all comment counts on page load
document.addEventListener('DOMContentLoaded', function() {
    document.querySelectorAll('.comment-toggle').forEach(btn => {
        const id = btn.dataset.id;
        fetch('/proposals/api/comments/' + id)
            .then(r => r.json())
            .then(comments => {
                const countEl = btn.querySelector('.count');
                if (comments.length > 0) {
                    countEl.textContent = comments.length;
                    btn.style.borderColor = '#C4553A';
                    btn.style.color = '#C4553A';
                }
            });
    });
});
"""


def build_index():
    files = sorted(PROPOSALS_DIR.glob("*.html"), key=lambda f: f.stat().st_mtime, reverse=True)
    if not files:
        content = '<p class="empty">No proposals yet.</p>'
    else:
        cards = []
        for f in files:
            name = f.stem.replace("-", " ").title()
            mtime = datetime.fromtimestamp(f.stat().st_mtime).strftime("%b %d, %Y")
            size_kb = f.stat().st_size // 1024
            cards.append(
                f'<a class="card" href="/proposals/{f.name}">'
                f'<span class="arrow">→</span>'
                f'<div class="title">{name}</div>'
                f'<div class="meta">{mtime} · {size_kb} KB</div>'
                f'</a>'
            )
        v3_files = sorted(SAMPLES_V3_DIR.glob("*.json")) if SAMPLES_V3_DIR.is_dir() else []
        if v3_files:
            cards.insert(0,
                f'<a class="card" href="/proposals/v3-voice">'
                f'<span class="arrow">→</span>'
                f'<div class="title">V3 Voice — Content Proposals</div>'
                f'<div class="meta">{len(v3_files)} posts · journals + vlogs + Medium + hex</div>'
                f'</a>'
            )
        sample_files = sorted(SAMPLES_DIR.glob("*.json"))
        if sample_files:
            cards.append(
                f'<a class="card" href="/proposals/content-samples">'
                f'<span class="arrow">→</span>'
                f'<div class="title">Content Samples (v1-v2)</div>'
                f'<div class="meta">{len(sample_files)} posts · earlier versions</div>'
                f'</a>'
            )
        content = '<div class="list">' + "\n".join(cards) + '</div>'
    return INDEX_HTML.format(content=content).encode("utf-8")


def _render_bold(s: str) -> str:
    escaped = html.escape(s)
    escaped = re.sub(r'\*\*(.+?)\*\*', r'<strong>\1</strong>', escaped)
    return escaped


def _render_image(sample: dict) -> str:
    img = sample.get("image", "")
    if not img:
        return ""
    return f'<div class="post-image"><img src="{html.escape(img)}" alt="Infographic"></div>'


def _render_reactions(sample_id: str, current_reaction: str) -> str:
    like_class = "react-btn like-btn active-like" if current_reaction == "like" else "react-btn like-btn"
    dislike_class = "react-btn dislike-btn active-dislike" if current_reaction == "dislike" else "react-btn dislike-btn"
    meh_class = "react-btn meh-btn active-meh" if current_reaction == "meh" else "react-btn meh-btn"
    return f'''<div class="reaction-row" data-reaction-id="{sample_id}">
        <button class="{dislike_class}" onclick="react('{sample_id}','dislike')"><span class="emoji">\U0001F44E</span></button>
        <button class="{meh_class}" onclick="react('{sample_id}','meh')"><span class="emoji">\U0001F610</span></button>
        <button class="{like_class}" onclick="react('{sample_id}','like')"><span class="emoji">\U0001F44D</span></button>
        <span class="react-divider"></span>
    </div>'''


def _render_comment_thread(sample_id: str, comment_count: int) -> str:
    count_text = str(comment_count) if comment_count > 0 else ""
    return f'''
    <button class="comment-toggle" data-id="{sample_id}" onclick="toggleThread('{sample_id}'); loadComments('{sample_id}');">
        \U0001F4AC <span class="count">{count_text}</span> Comments
    </button>
    <div class="comment-thread" id="thread-{sample_id}">
        <div class="comment-list" id="comments-{sample_id}"></div>
        <div class="comment-input-row">
            <textarea class="comment-input" id="input-{sample_id}" placeholder="Leave a comment..." rows="1"></textarea>
            <button class="comment-send" onclick="postComment('{sample_id}')">Send</button>
        </div>
    </div>'''


def _platform_label(platform: str) -> tuple:
    if platform == "linkedin":
        return ("LinkedIn", "li-plat")
    return ("X / Twitter", "x-plat")


def _render_single(sample: dict, sample_id: str, comment_count: int, reaction: str = "") -> str:
    plat_name, plat_class = _platform_label(sample.get("platform", "x"))
    handle = "@mikerapadas" if sample.get("platform") != "linkedin" else "Staff SWE at Meta"
    body = _render_bold(sample["body"])
    ctx_label = html.escape(sample.get("context_label", "Source data"))
    ctx = html.escape(sample.get("context", ""))
    card_class = sample.get("platform", "x")

    return f'''<div class="post-card {card_class}">
        <div class="post-meta">
            <div class="post-avatar">MR</div>
            <div><div class="post-name">Mike Rapadas</div><div class="post-handle">{handle}</div></div>
            <span class="post-platform {plat_class}">{plat_name}</span>
        </div>
        <div class="post-body">{body}</div>
        {_render_image(sample)}
        <div class="post-context">
            <span class="label">{ctx_label}</span> &mdash; {ctx}
        </div>
        {_render_reactions(sample_id, reaction)}
        {_render_comment_thread(sample_id, comment_count)}
    </div>'''


def _render_thread(sample: dict, sample_id: str, comment_count: int, reaction: str = "") -> str:
    tweets_html = []
    for i, tweet in enumerate(sample.get("tweets", []), 1):
        text = _render_bold(tweet)
        tweets_html.append(
            f'<div class="thread-tweet">'
            f'<span class="num">{i}/</span>'
            f'<div class="text">{text}</div>'
            f'</div>'
        )
    ctx_label = html.escape(sample.get("context_label", "Strategy note"))
    ctx = html.escape(sample.get("context", ""))
    count = len(sample.get("tweets", []))

    return f'''<div class="thread-container">
        <div class="thread-header">
            <div class="post-avatar">MR</div>
            <div><div class="post-name">Mike Rapadas</div><div class="post-handle">@mikerapadas</div></div>
            <span class="post-platform thread-plat">Thread ({count})</span>
        </div>
        {"".join(tweets_html)}
        <div class="thread-context">
            <span class="label" style="color:#7A8B6F;">{ctx_label}</span> &mdash; {ctx}
        </div>
        <div style="padding: 0 1.5rem 1rem;">
            {_render_reactions(sample_id, reaction)}
            {_render_comment_thread(sample_id, comment_count)}
        </div>
    </div>'''


def build_samples_page(samples_dir: Path = None, title: str = "What \"Building in Public\" looks like", subtitle: str = "Real posts, Mike's voice, from actual hex data.") -> bytes:
    if samples_dir is None:
        samples_dir = SAMPLES_DIR
    comments = _fetch_comments_for_surface("proposals")
    reactions = _load_reactions()
    sample_files = sorted(samples_dir.glob("*.json"))
    if not sample_files:
        body = '<div class="section"><p style="color:#8a8880;font-style:italic;">No samples yet.</p></div>'
    else:
        categories: dict[str, list] = {}
        cat_meta: dict[str, dict] = {}
        for f in sample_files:
            try:
                s = json.loads(f.read_text())
            except (json.JSONDecodeError, OSError):
                continue
            s["_id"] = f.stem
            cat = s.get("category", "Uncategorized")
            categories.setdefault(cat, []).append(s)
            if cat not in cat_meta:
                cat_meta[cat] = {
                    "tag": s.get("category_tag", ""),
                    "tag_class": s.get("category_tag_class", "hook"),
                }

        sections = []
        for cat, samples in categories.items():
            meta = cat_meta[cat]
            tag_html = f'<span class="format-tag {meta["tag_class"]}">{html.escape(meta["tag"])}</span>' if meta["tag"] else ""
            cards = []
            for s in samples:
                sid = s["_id"]
                cc = len(comments.get(sid, []))
                rxn = reactions.get(sid, "")
                if s.get("type") == "thread":
                    cards.append(_render_thread(s, sid, cc, rxn))
                else:
                    cards.append(_render_single(s, sid, cc, rxn))
            sections.append(
                f'<div class="section">'
                f'<div class="section-label">{html.escape(cat)} {tag_html}</div>'
                f'<div class="section-desc">{len(samples)} sample{"s" if len(samples) != 1 else ""}</div>'
                f'{"".join(cards)}'
                f'</div>'
            )
        body = "".join(sections)

    count = len(list(samples_dir.glob("*.json")))
    page = f'''<!DOCTYPE html>
<html lang="en"><head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{html.escape(title)}</title>
<link href="https://fonts.googleapis.com/css2?family=Fraunces:ital,opsz,wght@0,9..144,400;0,9..144,600;0,9..144,700;1,9..144,400&family=Work+Sans:wght@300;400;500;600&display=swap" rel="stylesheet">
<style>{SAMPLES_CSS}</style>
</head><body>
<div class="header">
    <span class="tag">Content Samples</span>
    <h1>{html.escape(title)}</h1>
    <p class="sub">{html.escape(subtitle)} {count} samples.</p>
    <div class="divider"></div>
</div>
{body}
<div class="kb-help" id="kb-help">
    <button class="kb-help-close" onclick="document.getElementById('kb-help').style.display='none'">&times;</button>
    <div class="title">Shortcuts</div>
    <kbd>→</kbd> yes &nbsp; <kbd>←</kbd> no &nbsp; <kbd>↓</kbd> meh<br>
    <kbd>j</kbd><kbd>k</kbd> navigate &nbsp; <kbd>c</kbd> comment<br>
    <kbd>?</kbd> toggle this
</div>
<footer>hex brand agent &middot; {count} content samples &middot; all data sourced from live fleet telemetry</footer>
<script>{COMMENTS_JS}</script>
</body></html>'''
    return page.encode("utf-8")


class ProposalHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(PROPOSALS_DIR), **kwargs)

    def do_GET(self):
        if self.path == "/" or self.path == "":
            self._send_html(build_index())
        elif self.path in ("/content-samples", "/content-samples.html"):
            self._send_html(build_samples_page())
        elif self.path in ("/v3-voice", "/v3-voice.html"):
            self._send_html(build_samples_page(
                samples_dir=SAMPLES_V3_DIR,
                title="V3 Voice — Content Proposals",
                subtitle="Posts written from journals, vlogs, Medium, newsletters, and hex activity. Like what hits, comment on what needs work.",
            ))
        elif self.path.startswith("/api/comments/"):
            self._handle_get_comments()
        else:
            super().do_GET()

    def do_POST(self):
        if self.path == "/api/comments":
            self._handle_post_comment()
        elif self.path == "/api/reactions":
            self._handle_post_reaction()
        else:
            self.send_response(404)
            self.end_headers()

    def _handle_get_comments(self):
        item_id = self.path.split("/api/comments/", 1)[1].strip("/")
        asset = f"proposals:{item_id}"
        try:
            with urllib.request.urlopen(UNIVERSAL_URL + "/all", timeout=2) as r:
                data = json.loads(r.read())
            items = [
                {"author": c.get("author", "mike"), "text": c.get("text", ""), "ts": c.get("created_at", "")}
                for c in data.get("comments", [])
                if c.get("asset") == asset
            ]
        except Exception:
            items = []
        body = json.dumps(items).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def _handle_post_comment(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            self.send_response(400)
            self.end_headers()
            return

        item_id = data.get("item", "")
        text = data.get("text", "").strip()
        author = data.get("author", "mike")

        if not item_id or not text:
            self.send_response(400)
            self.end_headers()
            return

        payload = json.dumps({"asset": f"proposals:{item_id}", "text": text, "author": author}).encode("utf-8")
        req = urllib.request.Request(
            UNIVERSAL_URL,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=2):
                pass
        except Exception:
            pass

        body = json.dumps({"ok": True}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def _handle_post_reaction(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            self.send_response(400)
            self.end_headers()
            return

        item_id = data.get("item", "")
        reaction = data.get("reaction", "")

        if not item_id or reaction not in ("like", "dislike", "meh"):
            self.send_response(400)
            self.end_headers()
            return

        reactions = _load_reactions()
        current = reactions.get(item_id, "")
        if current == reaction:
            reactions.pop(item_id, None)
            new_reaction = ""
        else:
            reactions[item_id] = reaction
            new_reaction = reaction
        _save_reactions(reactions)

        body = json.dumps({"ok": True, "reaction": new_reaction}).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def _send_html(self, body: bytes):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    with socketserver.TCPServer(("0.0.0.0", PORT), ProposalHandler) as httpd:
        httpd.allow_reuse_address = True
        print(f"proposals-server on :{PORT} serving {PROPOSALS_DIR}")
        httpd.serve_forever()
