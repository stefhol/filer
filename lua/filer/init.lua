--- filer.nvim — bundle sources (plus their imports) for a chat.
local M = {}

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
    "Cargo.toml",
    "gradlew",
    "mvnw",
    dotnet_project,
  }
  -- The bundle buffer has no path of its own, so fall back to the root the
  -- last bundle was built from before giving up on the cwd.
  return vim.fs.root(0, markers) or M._last_root or vim.fn.getcwd()
end

--- Where the bundle state lives for `root`.
function M.state_path(root)
  if M.config.state_file then
    return (M.config.state_file:gsub("^~", vim.env.HOME or "~"))
  end
  return (root or M.root()) .. "/.filer/bundle.toml"
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

--- Runs the binary and returns (stdout, stderr, code).
local function run(args, root)
  local command = { M.bin() }
  vim.list_extend(command, args)
  local result = vim.system(command, { text = true, cwd = root }):wait()
  return result.stdout or "", result.stderr or "", result.code
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
  -- the cursor. Java, Kotlin, Scala, Groovy, C#, Rust and TS/JS seed the method
  -- under the cursor (or the class when the cursor is not in a method). Other
  -- languages keep whole-file `:FilerHere`.
  local options = opts
  if
    options.at_line == nil
    and (
      name:match("%.fe$")
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
