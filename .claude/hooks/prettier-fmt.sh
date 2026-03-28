#!/bin/bash
# Auto-format non-Rust files after edit/write.
INPUT=$(cat)
FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')

if [ -n "$FILE" ]; then
  prettier --write "$FILE" 2>/dev/null
fi
exit 0
