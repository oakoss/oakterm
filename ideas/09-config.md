# Configuration

## Two Tiers

Community pain point: config format wars (Lua vs TOML vs KDL vs key-value).

Solution: flat file for simple cases, Lua for complex ones.

### Flat Config (basics)

`~/.config/phantom/config` — key-value pairs, no ceremony:

```
font-family = JetBrains Mono
font-size = 14
theme = catppuccin-mocha
cursor-style = block
scrollback-lines = 10000
```

Familiar to Ghostty users. Covers 80% of configuration needs.

### Lua Config (programmable)

`~/.config/phantom/config.lua` — full programming language when you need logic:

```lua
-- Dynamic font size based on display
if display.scale > 1 then
  font_size = 13
else
  font_size = 15
end

-- SSH domains
ssh_domains = {
  { name = "homelab", host = "proxmox.local", user = "jace" },
}

-- Layouts
layout.define("dev", {
  tabs = {
    { name = "code", panes = {
      { command = "nvim", split = "left", size = "65%" },
      { command = "npm run dev", split = "right" },
    }},
  },
})

-- Plugins
plugins = {
  ["agent-manager"]  = { enabled = true },
  ["context-engine"] = { enabled = true, ai = { backend = "ollama" } },
}

-- Project detection for auto-populating sidebar
project.detect = {
  { file = "docker-compose.yml", services = { "docker compose up -d" } },
  { file = "package.json", script = "dev", services = { "npm run dev" } },
  { file = "vitest.config.ts", watchers = { "vitest --watch" } },
}

-- Workspace setup scripts
workspace.on_create = function(ws)
  if ws:has_file("package.json") then ws:run("pnpm install") end
  if ws:has_file(".env.example") and not ws:has_file(".env") then
    ws:run("cp .env.example .env")
  end
end
```

### Precedence

Lua config takes priority if both exist. The flat config is syntactic sugar — it maps 1:1 to Lua settings.

### Migration

Reads Ghostty config format as a migration path. Warns about unsupported keys, maps the rest automatically.
