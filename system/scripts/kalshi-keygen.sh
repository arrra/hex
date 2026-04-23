#!/usr/bin/env bash
set -uo pipefail

SECRETS_DIR="$(cd "$(dirname "$0")/../secrets" && pwd)"
PRIVATE_KEY="$SECRETS_DIR/kalshi-private.pem"
PUBLIC_KEY="$SECRETS_DIR/kalshi-public.pem"
ENV_FILE="$SECRETS_DIR/kalshi.env"

if [[ -f "$PRIVATE_KEY" ]]; then
    echo "WARNING: $PRIVATE_KEY already exists. Not overwriting." >&2
    echo "To regenerate, delete the existing key first." >&2
    exit 1
fi

echo "Generating RSA keypair for Kalshi API auth..." >&2
openssl genrsa -out "$PRIVATE_KEY.tmp" 2048 2>/dev/null
chmod 600 "$PRIVATE_KEY.tmp"
openssl rsa -in "$PRIVATE_KEY.tmp" -pubout -out "$PUBLIC_KEY.tmp" 2>/dev/null

mv "$PRIVATE_KEY.tmp" "$PRIVATE_KEY"
mv "$PUBLIC_KEY.tmp" "$PUBLIC_KEY"

# Add placeholder KALSHI_API_KEY_ID to env file (user must fill in after pasting public key)
if grep -q "^KALSHI_KEY_ID=" "$ENV_FILE" 2>/dev/null; then
    # Update existing entry only if it's still the stub value
    if grep -q "^KALSHI_KEY_ID=00000000" "$ENV_FILE"; then
        sed -i '' 's/^KALSHI_KEY_ID=.*/KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE/' "$ENV_FILE"
        echo "Updated KALSHI_KEY_ID placeholder in $ENV_FILE" >&2
    else
        echo "KALSHI_KEY_ID already set in $ENV_FILE — skipping update." >&2
    fi
else
    echo "" >> "$ENV_FILE"
    echo "KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE" >> "$ENV_FILE"
    echo "Added KALSHI_KEY_ID placeholder to $ENV_FILE" >&2
fi

# Update private key path
if grep -q "^KALSHI_PRIVATE_KEY_PATH=" "$ENV_FILE" 2>/dev/null; then
    sed -i '' "s|^KALSHI_PRIVATE_KEY_PATH=.*|KALSHI_PRIVATE_KEY_PATH=$PRIVATE_KEY|" "$ENV_FILE"
else
    echo "KALSHI_PRIVATE_KEY_PATH=$PRIVATE_KEY" >> "$ENV_FILE"
fi

echo "" >&2
echo "=== Kalshi RSA Keypair Generated ===" >&2
echo "Private key: $PRIVATE_KEY (chmod 600)" >&2
echo "Public key:  $PUBLIC_KEY" >&2
echo "" >&2
echo "Paste the following public key into your Kalshi dashboard > API Keys > Add Key:" >&2
echo "" >&2
cat "$PUBLIC_KEY"
echo "" >&2
echo "After pasting, copy the Key ID from the dashboard and update:" >&2
echo "  $ENV_FILE" >&2
echo "  Set KALSHI_KEY_ID=<your-key-id>" >&2
