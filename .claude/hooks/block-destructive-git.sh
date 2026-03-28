#!/bin/bash
# Block destructive git operations unless explicitly requested.
INPUT=$(cat)
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty')

case "$COMMAND" in
  *"git push -f"*|*"git push --force"*)
    echo "Blocked: force push requires explicit user approval" >&2
    exit 2
    ;;
  *"git reset --hard"*)
    echo "Blocked: git reset --hard requires explicit user approval" >&2
    exit 2
    ;;
  *"git checkout -- ."*|*"git restore ."*)
    echo "Blocked: discarding all changes requires explicit user approval" >&2
    exit 2
    ;;
  *"git clean -f"*)
    echo "Blocked: git clean -f requires explicit user approval" >&2
    exit 2
    ;;
  *"git commit --amend"*)
    echo "Blocked: prefer new commits over amending. Only amend if user explicitly requested it." >&2
    exit 2
    ;;
esac

exit 0
