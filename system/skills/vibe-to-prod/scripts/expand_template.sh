#!/usr/bin/env bash
set -uo pipefail

# Usage: expand_template.sh <template> <output> KEY=VALUE KEY=VALUE ...
# Replaces {{KEY}} with VALUE in template, writes to output.

TEMPLATE="$1"
OUTPUT="$2"
shift 2

if [ ! -f "$TEMPLATE" ]; then
    echo "ERROR: Template not found: $TEMPLATE" >&2
    exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
cp "$TEMPLATE" "$OUTPUT" || { echo "ERROR: Failed to copy template to $OUTPUT" >&2; exit 1; }

for arg in "$@"; do
    KEY="${arg%%=*}"
    VALUE="${arg#*=}"
    # Escape sed replacement special chars: \ → \\, & → \&
    ESCAPED_VALUE=$(printf '%s' "$VALUE" | sed 's/\\/\\\\/g; s/&/\\&/g')
    sed -i '' "s|{{${KEY}}}|${ESCAPED_VALUE}|g" "$OUTPUT" || { echo "ERROR: sed substitution failed for key ${KEY}" >&2; exit 1; }
done

echo "Expanded $TEMPLATE -> $OUTPUT"
