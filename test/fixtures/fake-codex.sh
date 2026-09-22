#!/usr/bin/env bash
# Minimal stand-in for `codex exec --json`.
prompt=$(cat)
tid="fake-codex-thread"
esc=$(printf '%s' "$prompt" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' ' ')
echo "{\"type\":\"thread.started\",\"thread_id\":\"$tid\"}"
echo "{\"type\":\"turn.started\"}"
echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"i1\",\"type\":\"reasoning\",\"text\":\"thinking about it\"}}"
echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"i2\",\"type\":\"command_execution\",\"command\":\"echo hi > out.txt\",\"aggregated_output\":\"\",\"exit_code\":0,\"status\":\"completed\"}}"
printf 'hi\n' > out.txt
echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"i3\",\"type\":\"file_change\",\"changes\":[{\"path\":\"out.txt\",\"kind\":\"add\"}],\"status\":\"completed\"}}"
echo "{\"type\":\"item.completed\",\"item\":{\"id\":\"i4\",\"type\":\"agent_message\",\"text\":\"Handled: $esc\"}}"
echo "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\"output_tokens\":30}}"
