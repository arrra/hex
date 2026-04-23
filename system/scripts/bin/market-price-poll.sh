#!/usr/bin/env bash
# market-price-poll.sh — poll live market prices from Kalshi or Polymarket
# Usage: market-price-poll.sh <url-or-ticker>
# Output: JSON with yes_price, no_price, volume, last_trade

set -uo pipefail

INPUT="${1:-}"

if [[ -z "$INPUT" ]]; then
  printf '{"error":"no input provided","usage":"market-price-poll.sh <url-or-ticker>","yes_price":null,"no_price":null}\n' >&2
  exit 1
fi

# ── Platform detection ──────────────────────────────────────────────────────

PLATFORM=""
TICKER=""
SLUG=""

if [[ "$INPUT" == *"kalshi.com"* ]]; then
  PLATFORM="kalshi"
  SLUG="${INPUT%%\?*}"   # strip query string
  SLUG="${SLUG%/}"        # strip trailing slash
  SLUG="${SLUG##*/}"      # last path segment
  TICKER="${SLUG^^}"      # uppercase
elif [[ "$INPUT" == *"polymarket.com"* ]]; then
  PLATFORM="polymarket"
  SLUG="${INPUT%%\?*}"
  SLUG="${SLUG%/}"
  SLUG="${SLUG##*/}"
elif [[ "$INPUT" =~ ^[A-Za-z][A-Za-z0-9_-]+$ ]]; then
  # Raw ticker — assume Kalshi
  PLATFORM="kalshi"
  TICKER="${INPUT^^}"
else
  printf '{"error":"cannot detect platform from input","input":"%s","yes_price":null,"no_price":null}\n' "$INPUT" >&2
  exit 1
fi

# ── Helpers ──────────────────────────────────────────────────────────────────

err_json() {
  # err_json <platform> <key> <value> <message>
  printf '{"error":"%s","platform":"%s","%s":"%s","yes_price":null,"no_price":null}\n' \
    "$4" "$1" "$2" "$3"
}

fetch_url() {
  # fetch_url <url> <outfile>  → sets FETCH_HTTP_CODE, returns curl exit
  local url="$1" outfile="$2"
  FETCH_HTTP_CODE=$(curl -s --max-time 15 -w "%{http_code}" -o "$outfile" "$url" 2>/dev/null)
  return $?
}

# ── Kalshi parser (python3 reads JSON from file, script from heredoc/stdin) ──

parse_kalshi() {
  local json_file="$1" ticker="$2"
  python3 - "$ticker" "$json_file" <<'PYEOF'
import sys, json

ticker = sys.argv[1]
json_file = sys.argv[2]

try:
    with open(json_file) as f:
        data = json.load(f)
except Exception as e:
    print(json.dumps({"error": f"JSON parse error: {e}", "platform": "kalshi",
                      "ticker": ticker, "yes_price": None, "no_price": None}))
    sys.exit(0)

markets = data.get("markets", [])
if not markets:
    # Some endpoints return a single market object
    m_single = data.get("market")
    if m_single:
        markets = [m_single]

if not markets:
    print(json.dumps({"error": "no markets found", "platform": "kalshi",
                      "ticker": ticker, "yes_price": None, "no_price": None}))
    sys.exit(0)

m = markets[0]

def cents_to_dec(v):
    """Kalshi prices come as integers 0-100 (cents). Convert to 0.0-1.0."""
    if v is None:
        return None
    if isinstance(v, (int, float)) and v > 1:
        return round(v / 100.0, 4)
    return round(float(v), 4)

def midpoint(a, b):
    a, b = cents_to_dec(a), cents_to_dec(b)
    if a is None and b is None:
        return None
    if a is None: return b
    if b is None: return a
    return round((a + b) / 2, 4)

yes_price = midpoint(m.get("yes_bid"), m.get("yes_ask"))
if yes_price is None:
    yes_price = cents_to_dec(m.get("last_price"))
if yes_price is None:
    yes_price = 0.0

no_price = midpoint(m.get("no_bid"), m.get("no_ask"))
if no_price is None:
    no_price = round(1.0 - yes_price, 4)

print(json.dumps({
    "platform": "kalshi",
    "ticker": m.get("ticker", ticker),
    "yes_sub_title": m.get("yes_sub_title", ""),
    "yes_price": yes_price,
    "no_price": no_price,
    "volume": m.get("volume", 0),
    "last_trade": m.get("last_traded_at", m.get("last_trade_at", ""))
}))
PYEOF
}

# ── Polymarket parser ─────────────────────────────────────────────────────────

parse_polymarket() {
  local json_file="$1" slug="$2"
  python3 - "$slug" "$json_file" <<'PYEOF'
import sys, json

slug = sys.argv[1]
json_file = sys.argv[2]

try:
    with open(json_file) as f:
        data = json.load(f)
except Exception as e:
    print(json.dumps({"error": f"JSON parse error: {e}", "platform": "polymarket",
                      "slug": slug, "yes_price": None, "no_price": None}))
    sys.exit(0)

markets = data if isinstance(data, list) else data.get("markets", [])
if not markets:
    print(json.dumps({"error": "no markets found", "platform": "polymarket",
                      "slug": slug, "yes_price": None, "no_price": None}))
    sys.exit(0)

m = markets[0]

def to_dec(v):
    if v is None:
        return None
    try:
        f = float(v)
        # Polymarket prices are already 0-1 decimals
        return round(f, 4)
    except Exception:
        return None

yes_price = None
no_price = None

# Try tokens array
tokens = m.get("tokens", [])
for t in tokens:
    outcome = (t.get("outcome") or "").lower()
    price = to_dec(t.get("price"))
    if outcome == "yes":
        yes_price = price
    elif outcome == "no":
        no_price = price

# Fallback: outcomePrices list
if yes_price is None:
    outcome_prices = m.get("outcomePrices", [])
    if len(outcome_prices) >= 2:
        yes_price = to_dec(outcome_prices[0])
        no_price = to_dec(outcome_prices[1])

# Fallback: lastTradePrice
if yes_price is None:
    yes_price = to_dec(m.get("lastTradePrice"))
if no_price is None and yes_price is not None:
    no_price = round(1.0 - yes_price, 4)

print(json.dumps({
    "platform": "polymarket",
    "ticker": m.get("conditionId", slug),
    "slug": m.get("slug", slug),
    "yes_price": yes_price,
    "no_price": no_price,
    "volume": m.get("volume", m.get("volumeNum", 0)),
    "last_trade": m.get("lastTradeTime", m.get("updatedAt", ""))
}))
PYEOF
}

# ── Poll Kalshi ───────────────────────────────────────────────────────────────

poll_kalshi() {
  local ticker="$1"
  local url="https://api.elections.kalshi.com/trade-api/v2/markets?ticker=${ticker}&limit=5"
  local tmp
  tmp=$(mktemp)

  printf '[market-price-poll] kalshi: GET %s\n' "$url" >&2

  if ! fetch_url "$url" "$tmp"; then
    err_json "kalshi" "ticker" "$ticker" "network error or timeout"
    rm -f "$tmp"
    return 0
  fi

  if [[ "$FETCH_HTTP_CODE" != "200" ]]; then
    err_json "kalshi" "ticker" "$ticker" "API returned HTTP $FETCH_HTTP_CODE"
    rm -f "$tmp"
    return 0
  fi

  parse_kalshi "$tmp" "$ticker"
  rm -f "$tmp"
}

# ── Poll Polymarket ───────────────────────────────────────────────────────────

poll_polymarket() {
  local slug="$1"
  local url="https://gamma-api.polymarket.com/markets?slug=${slug}&limit=5"
  local tmp
  tmp=$(mktemp)

  printf '[market-price-poll] polymarket: GET %s\n' "$url" >&2

  if ! fetch_url "$url" "$tmp"; then
    err_json "polymarket" "slug" "$slug" "network error or timeout"
    rm -f "$tmp"
    return 0
  fi

  if [[ "$FETCH_HTTP_CODE" != "200" ]]; then
    err_json "polymarket" "slug" "$slug" "API returned HTTP $FETCH_HTTP_CODE"
    rm -f "$tmp"
    return 0
  fi

  parse_polymarket "$tmp" "$slug"
  rm -f "$tmp"
}

# ── Dispatch ──────────────────────────────────────────────────────────────────

case "$PLATFORM" in
  kalshi)
    poll_kalshi "$TICKER"
    ;;
  polymarket)
    poll_polymarket "$SLUG"
    ;;
  *)
    printf '{"error":"unknown platform","yes_price":null,"no_price":null}\n'
    exit 1
    ;;
esac
