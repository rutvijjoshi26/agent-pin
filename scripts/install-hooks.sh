#!/bin/sh
set -eu

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
claude_dir="$HOME/.claude"
codex_dir="$HOME/.codex"
timestamp="$(date +%Y%m%d-%H%M%S)"

mkdir -p "$claude_dir" "$codex_dir"
chmod +x "$repo_dir/bin/agent-pin-status" "$repo_dir/bin/agent-pin-reset"

REPO_DIR="$repo_dir" ruby <<'RUBY'
require "json"
require "fileutils"

repo_dir = ENV.fetch("REPO_DIR")
home = Dir.home
timestamp = Time.now.strftime("%Y%m%d-%H%M%S")

def load_json(path)
  return {} unless File.exist?(path)

  JSON.parse(File.read(path))
end

def backup(path, timestamp)
  return unless File.exist?(path)

  FileUtils.cp(path, "#{path}.agent-pin-backup-#{timestamp}")
end

def render_template(repo_dir, relative_path)
  File.read(File.join(repo_dir, relative_path)).gsub("__REPO__", repo_dir)
end

def agent_pin_entry?(value)
  case value
  when Hash
    value.any? { |key, child| key == "command" && child.to_s.include?("agent-pin-status") || agent_pin_entry?(child) }
  when Array
    value.any? { |child| agent_pin_entry?(child) }
  else
    false
  end
end

def merge_hooks!(target, incoming)
  target["hooks"] ||= {}
  target["hooks"].each do |event_name, entries|
    target["hooks"][event_name] = entries.reject { |entry| agent_pin_entry?(entry) }
  end
  target["hooks"].delete_if { |_event_name, entries| entries.empty? }

  incoming.fetch("hooks").each do |event_name, entries|
    target["hooks"][event_name] ||= []
    target["hooks"][event_name].concat(entries)
  end
end

claude_path = File.join(home, ".claude", "settings.json")
codex_path = File.join(home, ".codex", "hooks.json")

claude_incoming = JSON.parse(render_template(repo_dir, "hooks/claude/settings.json"))
codex_incoming = JSON.parse(render_template(repo_dir, "hooks/codex/hooks.json"))

FileUtils.mkdir_p(File.dirname(claude_path))
FileUtils.mkdir_p(File.dirname(codex_path))

claude_settings = load_json(claude_path)
codex_hooks = load_json(codex_path)

backup(claude_path, timestamp)
backup(codex_path, timestamp)

merge_hooks!(claude_settings, claude_incoming)
merge_hooks!(codex_hooks, codex_incoming)

File.write(claude_path, JSON.pretty_generate(claude_settings) + "\n")
File.write(codex_path, JSON.pretty_generate(codex_hooks) + "\n")
RUBY

cat <<EOF
Installed Agent Pin hooks:
  $claude_dir/settings.json
  $codex_dir/hooks.json

Backups were created next to existing files with this suffix:
  .agent-pin-backup-$timestamp

In Claude Code and Codex, run /hooks once to review and trust these hook commands.
EOF
