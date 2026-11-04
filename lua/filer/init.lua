--- filer.nvim — bundle sources (plus their imports) for a chat.
local M = {}

-- Neovim 0.9 exposes these as `vim.loop`.
local uv = vim.uv or vim.loop

--- Active session per project root; read from `.filer/session` on first use.
M._sessions = {}
M._sessions_loaded = {}

M.config = {
  -- Path to the `filer` binary. nil = $PATH, then a local checkout's target/.
  bin = nil,
  -- Project root. nil = detect from the current buffer (git/maven/gradle markers).
  root = nil,
  -- Import hops to follow when bundling. `:FilerDepth [n]` changes it for good,
  -- `:FilerHere!` / `:FilerBundle!` ask for a one-off depth.
  depth = 2,
  -- Also include files sharing a package with a bundled file.
  same_package = false,
  -- Also include pom.xml / build.gradle / settings.gradle / build.sbt.
  with_config = false,
  -- How the bundle buffer is opened ("vsplit", "split", "tabnew", ...).
  split = "vsplit",
  -- Also put the bundle into the system clipboard.
  copy = false,
  -- Remember what is already bundled in a TOML state file, and mark those
  -- files in the picker.
  state = true,
  -- State file path. nil = <root>/.filer/bundle.toml
  state_file = nil,
}

--- Depth passed to `setup`, so `:FilerDepth reset` has something to go back to.
M._default_depth = M.config.depth

local bin_cache = nil

--- Notifies without doubling the binary's own "filer: " prefix.
local function notify(message, level)
  local text = vim.trim(message or ""):gsub("^filer:%s*", "")
  vim.notify("filer: " .. (text ~= "" and text or "failed"), level or vim.log.levels.INFO)
end

--- Runs the binary and returns (stdout, stderr, code).
local function run(args, root)
  local command = { M.bin() }
  vim.list_extend(command, args)
  local result = vim.system(command, { text = true, cwd = root }):wait()
  return result.stdout or "", result.stderr or "", result.code
end

local function config_root()
  local root = M.config.root
  if type(root) == "function" then
    root = root()
  end
  return root
end

--- Project root for the current buffer.
function M.root()
  local configured = config_root()
  if configured then
    return configured
  end
  local function dotnet_project(name)
    return name:match("%.csproj$") ~= nil or name:match("%.sln$") ~= nil
  end
  local markers = {
    ".git",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "build.sbt",
    "tsconfig.json",
    "jsconfig.json",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "Cargo.toml",
    "gradlew",
    "mvnw",
    dotnet_project,
  }
  -- The bundle buffer has no path of its own, so fall back to the root the
  -- last bundle was built from before giving up on the cwd.
  return vim.fs.root(0, markers) or M._last_root or vim.fn.getcwd()
end

--- Where the bundle state lives for `root`: the active session when one is
--- chosen, otherwise the per-branch `.filer/bundle.toml`.
function M.state_path(root)
  if M.config.state_file then
    return (M.config.state_file:gsub("^~", vim.env.HOME or "~"))
  end
  root = root or M.root()
  local id = M.session(root)
  if id then
    return M.session_state_path(root, id)
  end
  return root .. "/.filer/bundle.toml"
end

-- Sessions -------------------------------------------------------------------
-- The binary keeps one bundle per session in `.filer/sessions/<id>.toml`
-- (`--session <id>`, or `$FILER_SESSION`); the plugin remembers which one this
-- project bundles into in `.filer/session`. The id is the name: letters, digits,
-- `.`, `-` and `_`.

--- `<root>/.filer/sessions`: one TOML file per session.
function M.sessions_dir(root)
  return (root or M.root()) .. "/.filer/sessions"
end

--- The state file `--session <id>` reads and writes.
function M.session_state_path(root, id)
  return M.sessions_dir(root) .. "/" .. id .. ".toml"
end

--- Where this project's active session id is remembered.
function M.session_file(root)
  return (root or M.root()) .. "/.filer/session"
end

--- Session ids become file names, so they follow the binary's rule: letters,
--- digits, `.`, `-`, `_` only, at most 128 characters, never `.` or `..`.
function M.valid_session(id)
  return type(id) == "string"
    and #id > 0
    and #id <= 128
    and id ~= "."
    and id ~= ".."
    and id:match("^[%w._-]+$") ~= nil
end

--- Session ids saved for `root`, sorted. Read from disk, so it also finds the
--- ones another editor, an agent or a `filer` run created.
function M.session_ids(root)
  local ids = {}
  local handle = uv.fs_scandir(M.sessions_dir(root))
  while handle do
    local name = uv.fs_scandir_next(handle)
    if not name then
      break
    end
    local id = name:match("^(.+)%.toml$")
    if id and M.valid_session(id) then
      table.insert(ids, id)
    end
  end
  table.sort(ids)
  return ids
end

--- The session `root` bundles into; nil means the default per-branch state.
function M.session(root)
  root = root or M.root()
  if not M._sessions_loaded[root] then
    M._sessions_loaded[root] = true
    local ok, lines = pcall(vim.fn.readfile, M.session_file(root))
    if ok and type(lines) == "table" and type(lines[1]) == "string" then
      local id = vim.trim(lines[1])
      if id ~= "" and id ~= "default" and M.valid_session(id) then
        M._sessions[root] = id
      end
    end
  end
  return M._sessions[root]
end

--- Remembers `id` (nil = default state) in `.filer/session`. The `*` ignore
--- file is the same trick the binary uses to keep `.filer` out of `git status`.
local function remember_session(root, id)
  local path = M.session_file(root)
  if id == nil then
    pcall(vim.fn.delete, path)
    return
  end
  local ok, err = pcall(function()
    local dir = vim.fn.fnamemodify(path, ":h")
    vim.fn.mkdir(dir, "p")
    local ignore = dir .. "/.gitignore"
    if uv.fs_stat(ignore) == nil then
      vim.fn.writefile({ "*" }, ignore)
    end
    vim.fn.writefile({ id }, path)
  end)
  if not ok then
    notify(("could not remember the session in %s: %s"):format(path, err), vim.log.levels.WARN)
  end
end

--- Switches `root` to session `id` (`nil` or `"default"` = the per-branch
--- state), remembers it for the next Neovim and re-renders its saved bundle
--- (`opts.show = false` skips the re-render). Returns the session in effect, or
--- nil when the name was rejected or sessions do not apply to this setup.
function M.set_session(id, opts)
  opts = opts or {}
  local root = opts.root or M.root()
  if id == "" or id == "default" then
    id = nil
  end
  if id ~= nil and not M.valid_session(id) then
    notify(
      ("invalid session name %q: letters, digits, '.', '-' and '_' only"):format(tostring(id)),
      vim.log.levels.ERROR
    )
    return nil
  end
  if M.config.state_file then
    notify("opts.state_file is set, so sessions are not used", vim.log.levels.WARN)
    return nil
  end
  if not M.config.state then
    notify("sessions need opts.state = true", vim.log.levels.WARN)
    return nil
  end
  M._sessions[root] = id
  M._sessions_loaded[root] = true
  remember_session(root, id)
  notify(id and ("session: " .. id) or "session: default (per branch)")
  if opts.show ~= false then
    M.show({ root = root, quiet_empty = true })
  end
  return id
end

--- Asks for a session name (the id *is* the name) and switches to it.
function M.session_create(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  vim.ui.input({ prompt = "filer: new session name: " }, function(input)
    if input == nil then
      return
    end
    input = vim.trim(input)
    if input == "" then
      return
    end
    if not M.valid_session(input) then
      notify(
        ("invalid session name %q: letters, digits, '.', '-' and '_' only"):format(input),
        vim.log.levels.ERROR
      )
      return
    end
    if uv.fs_stat(M.session_state_path(root, input)) ~= nil then
      notify(("session %q already exists — switching to it"):format(input))
    end
    M.set_session(input, { root = root })
  end)
end

--- Deletes one session through `filer context clear` and leaves the default
--- state alone. Falls back to the default session when the deleted one was
--- active. Returns true when something was removed.
function M.session_delete(id, opts)
  opts = opts or {}
  local root = opts.root or M.root()
  if id == nil or id == "default" then
    notify("the default state is not a session — :FilerReset clears it", vim.log.levels.WARN)
    return false
  end
  if not M.valid_session(id) then
    notify(("invalid session name %q"):format(tostring(id)), vim.log.levels.ERROR)
    return false
  end
  if not opts.force then
    local answer = vim.fn.confirm(("filer: delete session %q?"):format(id), "&Yes\n&No", 2)
    if answer ~= 1 then
      return false
    end
  end
  local stdout, stderr, code = run({ "context", "clear", id, "--root", root }, root)
  if code ~= 0 then
    notify(stderr ~= "" and stderr or "delete failed", vim.log.levels.ERROR)
    return false
  end
  if M.session(root) == id then
    M.set_session(nil, { root = root, show = false })
  end
  notify(stdout ~= "" and stdout or ("session %q deleted"):format(id))
  return true
end

--- One row per session for the browser: the default state, every saved session,
--- the active one when it has no state file yet (a fresh session), then the
--- "new" row. Counts come from `filer context list --json`, so they match the
--- binary.
function M.sessions(root)
  root = root or M.root()
  local rows = {}
  local listed = {}
  local active = M.session(root)
  -- `run` raises when the binary is missing or unusable, so a failed listing
  -- degrades to the session files on disk instead of an error.
  local ok, stdout, _, code = pcall(run, { "context", "list", "--root", root, "--json" }, root)
  local data
  if ok and code == 0 and stdout ~= "" then
    local decoded_ok, decoded = pcall(vim.json.decode, stdout)
    if decoded_ok and type(decoded) == "table" then
      data = decoded
    end
  end
  local branch = data and type(data.label) == "string" and data.label or ""

  local function add(id, value)
    if listed[id] then
      return
    end
    listed[id] = true
    local entries = value and tonumber(value.entries) or 0
    local modified = value and tonumber(value.modified) or 0
    local path = value and type(value.path) == "string" and value.path or nil
    local parts = { ("%d entries"):format(entries) }
    if branch ~= "" then
      table.insert(parts, branch)
    end
    table.insert(parts, path or "(not saved yet)")
    if modified > 0 then
      table.insert(parts, os.date("%Y-%m-%d %H:%M", modified))
    end
    local current = (active or "default") == id
    table.insert(rows, {
      id = id,
      entries = entries,
      path = path,
      current = current,
      ordinal = (current and "current " or "") .. id .. " " .. table.concat(parts, " "),
      display = ("%s %-18s %s"):format(current and "*" or " ", id, table.concat(parts, " · ")),
    })
  end

  add("default", data and data.default)
  local saved = data and type(data.sessions) == "table" and data.sessions or nil
  if saved then
    for _, value in ipairs(saved) do
      if type(value) == "table" and type(value.id) == "string" then
        add(value.id, value)
      end
    end
  else
    for _, id in ipairs(M.session_ids(root)) do
      add(id, nil)
    end
  end
  -- A freshly created session has no state file yet: keep it visible instead of
  -- looking like the switch did nothing.
  if active then
    add(active, nil)
  end
  table.insert(rows, {
    id = nil,
    create = true,
    ordinal = "new session",
    display = "  + new session…",
  })
  return rows
end

--- Session browser without Telescope: switching and creating work, deleting
--- does not (`<C-d>` needs the Telescope picker).
function M.session_select(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  vim.ui.select(M.sessions(root), {
    prompt = "filer: session",
    format_item = function(row)
      return row.display
    end,
  }, function(row)
    if row == nil then
      return
    end
    if row.create then
      M.session_create({ root = root })
      return
    end
    M.set_session(row.id, { root = root })
  end)
end

--- Telescope entry for one browser row. `filename` is the row's state file
--- when it exists; rows without one (a fresh active session, the "+ new
--- session…" row) keep it nil. Telescope's previewer falls back from a nil
--- `filename` to `entry.value` and would call `path_expand` on the row table,
--- which is what the session browser's own previewer avoids.
function M.session_entry(row, root)
  local file = row.path and (root .. "/" .. row.path) or nil
  if file and uv.fs_stat(file) == nil then
    file = nil
  end
  return {
    value = row,
    display = row.display,
    ordinal = row.ordinal or row.display,
    filename = file,
  }
end

--- File the session previewer shows for `entry`: the state file when it exists,
--- nil when there is nothing to preview. It never returns `entry.value`, the row
--- table Telescope would otherwise try to expand as a path.
function M.session_preview_target(entry)
  local file = entry.filename
  if file and uv.fs_stat(file) ~= nil then
    return file
  end
  return nil
end

--- Preview text for rows that have no state file to show yet.
local function session_preview(row)
  if row.create then
    return {
      "New session",
      "",
      "<CR> or <C-n> asks for a name and switches to it.",
      "Letters, digits, '.', '-' and '_' are allowed.",
      "The state lives in .filer/sessions/<name>.toml.",
    }
  end
  return {
    "Session: " .. tostring(row.id),
    "",
    "No state file yet: the first bundle in this session writes one.",
  }
end

--- Session browser: Telescope when it is installed, `vim.ui.select` otherwise.
--- `<CR>` switches, `<C-n>` creates, `<C-d>` deletes.
function M.session_picker(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  local has_telescope = pcall(require, "telescope.pickers")
  if not has_telescope then
    return M.session_select({ root = root })
  end
  local pickers = require("telescope.pickers")
  local finders = require("telescope.finders")
  local previewers = require("telescope.previewers")
  local conf = require("telescope.config").values
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")

  local rows = M.sessions(root)

  -- Saved sessions preview their state file through the configured file
  -- previewer; rows without one get a short summary instead. Delegating to
  -- `conf.file_previewer` directly is not possible here: it would call
  -- `from_entry.path`, whose fallback to `entry.value` (a table) is the crash.
  local previewer = conf.file_previewer({})
    and previewers.new_buffer_previewer({
      title = "filer: session",
      define_preview = function(self, entry)
        local file = M.session_preview_target(entry)
        if file then
          conf.buffer_previewer_maker(file, self.state.bufnr, {
            bufname = self.state.bufname,
            winid = self.state.winid,
          })
          return
        end
        vim.api.nvim_buf_set_lines(self.state.bufnr, 0, -1, false, session_preview(entry.value or {}))
        vim.bo[self.state.bufnr].filetype = "markdown"
      end,
    }) or conf.noop_previewer

  local picker = pickers.new({}, {
    prompt_title = "filer: sessions (" .. vim.fn.fnamemodify(root, ":t") .. ")",
    results_title = "<CR> switch · <C-n> new · <C-d> delete",
    finder = finders.new_table({
      results = rows,
      entry_maker = function(row)
        return M.session_entry(row, root)
      end,
    }),
    previewer = previewer,
    sorter = conf.generic_sorter({}),
    attach_mappings = function(prompt_bufnr, map)
      actions.select_default:replace(function()
        local entry = action_state.get_selected_entry()
        actions.close(prompt_bufnr)
        if not entry then
          return
        end
        local row = entry.value
        if row.create then
          M.session_create({ root = root })
          return
        end
        M.set_session(row.id, { root = root })
      end)
      local function create()
        actions.close(prompt_bufnr)
        M.session_create({ root = root })
      end
      local function delete()
        local entry = action_state.get_selected_entry()
        if not entry then
          return
        end
        local row = entry.value
        actions.close(prompt_bufnr)
        if row.create then
          notify("the + row is not a session", vim.log.levels.WARN)
          return
        end
        -- Reopen either way: the browser stays open for another pick, and the
        -- confirmation is answered outside the (already closed) picker.
        M.session_delete(row.id, { root = root })
        M.session_picker({ root = root })
      end
      for _, mode in ipairs({ "i", "n" }) do
        map(mode, "<C-n>", create)
        map(mode, "<C-d>", delete)
      end
      return true
    end,
  })
  picker:find()
end

--- Absolute path (or bare command) of the filer binary.
function M.bin()
  if M.config.bin then
    return M.config.bin
  end
  if bin_cache then
    return bin_cache
  end
  if vim.fn.executable("filer") == 1 then
    bin_cache = "filer"
    return bin_cache
  end
  local source = debug.getinfo(1, "S").source:sub(2)
  local plugin_root = vim.fn.fnamemodify(source, ":p:h:h:h")
  for _, candidate in ipairs({ "target/release/filer", "target/debug/filer" }) do
    local path = plugin_root .. "/" .. candidate
    if vim.fn.executable(path) == 1 then
      bin_cache = path
      return bin_cache
    end
  end
  error(
    "filer: binary not found. Install it with the release installer "
      .. "(https://github.com/stefhol/filer#install) or set opts.bin"
  )
end

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})
  M._default_depth = M.config.depth
  bin_cache = nil
  return M
end

--- Import depth used by the next bundles.
function M.depth()
  return M.config.depth
end

--- Import depth `:FilerDepth reset` goes back to.
function M.default_depth()
  return M._default_depth
end

--- Changes the import depth for every following bundle. `nil` (or `"reset"`)
--- restores the depth passed to `setup`. Returns the depth now in effect, or nil
--- when `depth` was not a whole number >= 0.
function M.set_depth(depth)
  if depth == nil or depth == "reset" then
    M.config.depth = M._default_depth
    notify(("depth reset to %s"):format(tostring(M.config.depth)))
    return M.config.depth
  end
  local number = tonumber(depth)
  if not number or number < 0 or number % 1 ~= 0 then
    notify(("depth must be a whole number >= 0, not %s"):format(vim.inspect(depth)), vim.log.levels.ERROR)
    return nil
  end
  M.config.depth = math.floor(number)
  notify(("depth set to %d"):format(M.config.depth))
  return M.config.depth
end

--- Fuzzy-pick types and bundle them (plus imports) into a split.
function M.search(opts)
  opts = opts or {}
  local ok, pickers = pcall(require, "telescope.pickers")
  if not ok then
    notify("Telescope is required for :FilerSearch", vim.log.levels.ERROR)
    return
  end
  local finders = require("telescope.finders")
  local conf = require("telescope.config").values
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")

  local root = opts.root or M.root()
  local depth = opts.depth or M.config.depth
  local use_state = opts.state
  if use_state == nil then
    use_state = M.config.state
  end

  local entry_maker = function(line)
    local decoded_ok, item = pcall(vim.json.decode, line)
    if not decoded_ok or type(item) ~= "table" or type(item.path) ~= "string" then
      return nil
    end
    local fqn = item.fqn or ""
    local scope = item.scope
    if scope == vim.NIL then
      scope = nil
    end
    local symbol = item.symbol
    if symbol == vim.NIL then
      symbol = nil
    end
    local member = item.member
    if member == vim.NIL then
      member = nil
    end
    local kind = item.kind
    if kind == vim.NIL then
      kind = nil
    end
    local line_number = item.line
    if line_number == vim.NIL then
      line_number = nil
    end
    local depth = item.depth
    if depth == vim.NIL then
      depth = nil
    end
    local reason = item.reason
    if reason == vim.NIL then
      reason = nil
    end
    -- `[file]`, `[class]`, `[method]`, `[type]`, `[function]`, `[declaration]`.
    local label = "[" .. (kind or "file") .. "] " .. (fqn ~= "" and fqn or item.path)
    -- Functions are not unique across files (one .fe file holds thousands),
    -- so show where the function lives.
    if symbol and kind == "function" then
      label = label .. "  (" .. item.path .. ")"
    end
    local included = item.included == true
    if included then
      local detail = reason or (depth and ("depth " .. depth)) or nil
      if depth and reason then
        detail = "depth " .. depth .. ", " .. reason
      end
      label = "✓ " .. label .. (detail and ("  (" .. detail .. ")") or "")
    end
    return {
      value = item,
      path = item.path,
      scope = scope,
      symbol = symbol,
      member = member,
      kind = kind,
      line = line_number,
      fqn = fqn,
      included = included,
      filename = root .. "/" .. item.path,
      lnum = line_number or 1,
      ordinal = (fqn ~= "" and (fqn .. " ") or "")
        .. item.path
        .. (symbol and (" " .. symbol) or "")
        .. (member and (" " .. member) or ""),
      display = label,
    }
  end

  local picker = pickers.new({}, {
    prompt_title = "filer: symbols (" .. vim.fn.fnamemodify(root, ":t") .. ")",
    results_title = "excerpts · <Tab> to multi-select",
    finder = finders.new_job(function(prompt)
      local args = { M.bin(), "search", "--root", root, "--query", prompt or "" }
      if use_state then
        table.insert(args, "--state")
        table.insert(args, M.state_path(root))
      end
      return args
    end, entry_maker, 5000),
    previewer = conf.file_previewer({}),
    sorter = conf.generic_sorter({}),
    attach_mappings = function(prompt_bufnr)
      actions.select_default:replace(function()
        local current = action_state.get_current_picker(prompt_bufnr)
        local selected = current:get_multi_selection()
        if #selected == 0 then
          local entry = action_state.get_selected_entry()
          selected = entry and { entry } or {}
        end
        local paths = {}
        local already = 0
        for _, entry in ipairs(selected) do
          if entry.included then
            already = already + 1
          elseif entry.scope == "file" or (not entry.scope and not entry.symbol) then
            paths[#paths + 1] = entry.path
          elseif (entry.scope == "methods" or (not entry.scope and entry.member)) and entry.symbol and entry.member then
            paths[#paths + 1] = entry.path .. "::" .. entry.symbol .. "." .. entry.member
          elseif entry.symbol then
            paths[#paths + 1] = entry.path .. "::" .. entry.symbol
          else
            paths[#paths + 1] = entry.path
          end
        end
        actions.close(prompt_bufnr)
        if #paths == 0 then
          notify(already > 0 and "already in the bundle — :FilerReset to start over" or "nothing selected")
          return
        end
        M.bundle(paths, { root = root, depth = depth })
      end)
      return true
    end,
  })
  picker:find()
end

--- Bundles the given files (relative to root, or absolute) and opens the result.
function M.bundle(paths, opts)
  opts = opts or {}
  if type(paths) == "string" then
    paths = { paths }
  end
  if #paths == 0 then
    notify("no files selected", vim.log.levels.WARN)
    return
  end

  local root = opts.root or M.root()
  local depth = opts.depth or M.config.depth
  local same_package = opts.same_package == nil and M.config.same_package or opts.same_package
  local with_config = opts.with_config == nil and M.config.with_config or opts.with_config
  local use_state = opts.state
  if use_state == nil then
    use_state = M.config.state
  end

  local args = { "bundle", "--root", root, "--depth", tostring(depth) }
  if same_package then
    table.insert(args, "--with-same-package")
  end
  if with_config then
    table.insert(args, "--with-config")
  end
  if use_state then
    table.insert(args, "--state")
    table.insert(args, M.state_path(root))
    if opts.reset then
      table.insert(args, "--reset")
    end
  end
  if opts.at_line then
    table.insert(args, "--at-line")
    table.insert(args, tostring(opts.at_line))
  end
  table.insert(args, "--")

  local stdout, stderr, code = run(vim.list_extend(args, paths), root)
  if code ~= 0 then
    notify(stderr ~= "" and stderr or "bundle failed", vim.log.levels.ERROR)
    return
  end
  if stderr ~= "" then
    notify((stderr:gsub("\n", " ")))
  end
  M.open(stdout, vim.tbl_extend("force", opts, { root = root }))
end

--- Bundles the current buffer together with its imports. In a `.fe` file the
--- function under the cursor is the seed, not the whole 20 000-line file.
function M.here(opts)
  opts = opts or {}
  local name = vim.api.nvim_buf_get_name(0)
  if name == "" then
    notify("current buffer is not a file", vim.log.levels.WARN)
    return
  end
  -- `.fe` files are usually tens of thousands of lines: seed the function under
  -- the cursor. Python, Java, Kotlin, Scala, Groovy, C#, Rust and TS/JS seed the
  -- method under the cursor (or the class when the cursor is not in a method).
  -- Other languages keep whole-file `:FilerHere`.
  local options = opts
  if
    options.at_line == nil
    and (
      name:match("%.fe$")
      or name:match("%.py$")
      or name:match("%.pyi$")
      or name:match("%.java$")
      or name:match("%.kt$")
      or name:match("%.kts$")
      or name:match("%.scala$")
      or name:match("%.groovy$")
      or name:match("%.cs$")
      or name:match("%.rs$")
      or name:match("%.ts$")
      or name:match("%.tsx$")
      or name:match("%.js$")
      or name:match("%.jsx$")
    )
  then
    options = vim.tbl_extend("force", { at_line = vim.api.nvim_win_get_cursor(0)[1] }, opts)
  end
  M.bundle({ name }, options)
end

--- Re-renders the saved bundle (same files, current contents) into the buffer.
function M.show(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  local args = { "show", "--root", root, "--state", M.state_path(root) }
  if opts.branch then
    table.insert(args, "--branch")
    table.insert(args, opts.branch)
  end
  if opts.title then
    table.insert(args, "--title")
    table.insert(args, opts.title)
  end

  local stdout, stderr, code = run(args, root)
  if code ~= 0 then
    if opts.quiet_empty then
      -- Switching to a session that has nothing bundled yet: say so in the
      -- bundle buffer itself, and only when one is already open.
      if M.find_bundle_buffer() then
        local id = M.session(root)
        M.open(
          ("# filer: %s\n\nNothing bundled yet on this branch. "
            .. "`:FilerHere` or `:FilerSearch` starts the bundle.\n"):format(
            id and ("session %q"):format(id) or "the default state"
          ),
          { root = root }
        )
      end
      return
    end
    notify(stderr ~= "" and stderr or "nothing bundled yet", vim.log.levels.WARN)
    return
  end
  if stderr ~= "" then
    notify((stderr:gsub("\n", " ")))
  end
  M.open(stdout, vim.tbl_extend("force", opts, { root = root }))
end

--- Drops every branch's bundle, not just the current one.
function M.reset_all(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  local stdout, stderr, code =
    run({ "state", "--clear", "--all", "--file", M.state_path(root), "--root", root }, root)
  if code ~= 0 then
    notify(stderr ~= "" and stderr or "reset failed", vim.log.levels.ERROR)
    return
  end
  notify(stdout ~= "" and stdout or "all bundles cleared")
end

--- Drops the saved bundle state for the current branch, so it starts empty.
function M.reset(opts)
  opts = opts or {}
  local root = opts.root or M.root()
  local stdout, stderr, code = run({ "state", "--clear", "--file", M.state_path(root), "--root", root }, root)
  if code ~= 0 then
    notify(stderr ~= "" and stderr or "reset failed", vim.log.levels.ERROR)
    return
  end
  notify(stdout ~= "" and stdout or "bundle state cleared")
end

--- The bundle buffer, if one is still around.
function M.find_bundle_buffer()
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_valid(buf) then
      local name = vim.api.nvim_buf_get_name(buf)
      if name:match("^filer://bundle") then
        return buf
      end
    end
  end
  return nil
end

local function new_bundle_buffer()
  local name = "filer://bundle"
  local index = 1
  while vim.fn.bufnr(name) ~= -1 do
    index = index + 1
    name = "filer://bundle-" .. index
  end
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_name(buf, name)
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  return buf
end

--- Opens markdown text, reusing the bundle window and buffer when they exist.
function M.open(text, opts)
  opts = opts or {}
  local lines = vim.split(text, "\n", { plain = true })
  if lines[#lines] == "" then
    table.remove(lines)
  end

  M._last_root = opts.root or M._last_root
  local buf = M.find_bundle_buffer()
  local win = buf and vim.fn.bufwinid(buf) or -1

  if buf and win ~= -1 then
    -- Reuse: focus the window that already shows the bundle and refill it.
    if vim.api.nvim_get_current_win() ~= win then
      vim.api.nvim_set_current_win(win)
    end
  else
    vim.cmd(opts.split or M.config.split)
    if buf then
      vim.api.nvim_win_set_buf(0, buf)
    else
      buf = new_bundle_buffer()
      vim.api.nvim_win_set_buf(0, buf)
    end
  end

  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  vim.wo[0].wrap = true
  vim.api.nvim_win_set_cursor(0, { 1, 0 })

  if opts.keymaps ~= false then
    vim.keymap.set("n", "q", "<cmd>close<cr>", { buffer = buf, desc = "Close filer bundle" })
  end

  if (opts.copy ~= nil and opts.copy) or (opts.copy == nil and M.config.copy) then
    vim.fn.setreg("+", text)
    notify("bundle copied to clipboard")
  end

  return buf
end

return M
