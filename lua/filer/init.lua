--- filer.nvim — bundle Java sources (plus their imports) for pasting into a chat.
local M = {}

M.config = {
  -- Path to the `filer` binary. nil = look up PATH, then this repo's target/.
  bin = nil,
  -- Project root. nil = detect from the current buffer (git/maven/gradle markers).
  root = nil,
  -- Import hops to follow when bundling.
  depth = 2,
  -- Also include files sharing a package with a bundled file.
  same_package = false,
  -- Also include pom.xml / build.gradle / settings.gradle.
  with_config = false,
  -- How the bundle buffer is opened ("vsplit", "split", "tabnew", ...).
  split = "vsplit",
  -- Also put the bundle into the system clipboard.
  copy = false,
}

local bin_cache = nil

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
  local markers = {
    ".git",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradlew",
    "mvnw",
  }
  return vim.fs.root(0, markers) or vim.fn.getcwd()
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
  error("filer: binary not found. Run `cargo build --release` in " .. plugin_root .. " or set opts.bin")
end

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})
  bin_cache = nil
  return M
end

--- Runs the binary and returns (stdout, stderr, code).
local function run(args, root)
  local command = { M.bin() }
  vim.list_extend(command, args)
  local result = vim.system(command, { text = true, cwd = root }):wait()
  return result.stdout or "", result.stderr or "", result.code
end

--- Fuzzy-pick Java types and bundle them (plus imports) into a split.
function M.search(opts)
  opts = opts or {}
  local ok, pickers = pcall(require, "telescope.pickers")
  if not ok then
    vim.notify("filer: Telescope is required for :FilerSearch", vim.log.levels.ERROR)
    return
  end
  local finders = require("telescope.finders")
  local conf = require("telescope.config").values
  local actions = require("telescope.actions")
  local action_state = require("telescope.actions.state")

  local root = opts.root or M.root()
  local depth = opts.depth or M.config.depth

  local entry_maker = function(line)
    local decoded_ok, item = pcall(vim.json.decode, line)
    if not decoded_ok or type(item) ~= "table" or type(item.path) ~= "string" then
      return nil
    end
    local fqn = item.fqn or ""
    return {
      value = item,
      path = item.path,
      fqn = fqn,
      filename = root .. "/" .. item.path,
      ordinal = (fqn ~= "" and (fqn .. " ") or "") .. item.path,
      display = fqn ~= "" and fqn or item.path,
    }
  end

  local picker = pickers.new({}, {
    prompt_title = "filer: Java types (" .. vim.fn.fnamemodify(root, ":t") .. ")",
    results_title = "files · <Tab> to multi-select",
    finder = finders.new_job(function(prompt)
      return { M.bin(), "search", "--root", root, "--query", prompt or "" }
    end, entry_maker, 5000),
    previewer = conf.file_previewer({}),
    sorter = conf.generic_sorter({}),
    attach_mappings = function(prompt_bufnr)
      actions.select_default:replace(function()
        local current = action_state.get_current_picker(prompt_bufnr)
        local selected = current:get_multi_selection()
        local paths = {}
        if #selected == 0 then
          local entry = action_state.get_selected_entry()
          if not entry then
            actions.close(prompt_bufnr)
            return
          end
          paths[1] = entry.path
        else
          for _, entry in ipairs(selected) do
            paths[#paths + 1] = entry.path
          end
        end
        actions.close(prompt_bufnr)
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
    vim.notify("filer: no files selected", vim.log.levels.WARN)
    return
  end

  local root = opts.root or M.root()
  local depth = opts.depth or M.config.depth
  local same_package = opts.same_package == nil and M.config.same_package or opts.same_package
  local with_config = opts.with_config == nil and M.config.with_config or opts.with_config

  local args = { "bundle", "--root", root, "--depth", tostring(depth) }
  if same_package then
    table.insert(args, "--with-same-package")
  end
  if with_config then
    table.insert(args, "--with-config")
  end
  table.insert(args, "--")

  local stdout, stderr, code = run(vim.list_extend(args, paths), root)
  if code ~= 0 then
    vim.notify("filer: " .. (stderr ~= "" and vim.trim(stderr) or "bundle failed"), vim.log.levels.ERROR)
    return
  end
  M.open(stdout, opts)
end

--- Bundles the file in the current buffer together with its imports.
function M.here(opts)
  opts = opts or {}
  local name = vim.api.nvim_buf_get_name(0)
  if name == "" then
    vim.notify("filer: current buffer is not a file", vim.log.levels.WARN)
    return
  end
  M.bundle({ name }, opts)
end

--- Opens markdown text in a scratch split.
function M.open(text, opts)
  opts = opts or {}
  local lines = vim.split(text, "\n", { plain = true })
  if lines[#lines] == "" then
    table.remove(lines)
  end

  local split = opts.split or M.config.split
  vim.cmd(split)
  local buf = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_win_set_buf(0, buf)
  M._bundle_count = (M._bundle_count or 0) + 1
  vim.api.nvim_buf_set_name(buf, "filer://bundle-" .. M._bundle_count)
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].bufhidden = "wipe"
  vim.bo[buf].swapfile = false
  vim.bo[buf].modifiable = false
  vim.wo[0].wrap = true

  if opts.keymaps ~= false then
    vim.keymap.set("n", "q", "<cmd>close<cr>", { buffer = buf, desc = "Close filer bundle" })
  end

  if (opts.copy ~= nil and opts.copy) or (opts.copy == nil and M.config.copy) then
    vim.fn.setreg("+", text)
    vim.notify("filer: bundle copied to clipboard")
  end
end

return M
