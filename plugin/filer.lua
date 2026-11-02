if vim.g.loaded_filer == 1 then
  return
end
vim.g.loaded_filer = 1

local function filer()
  return require("filer")
end

--- Asks for a depth and hands it to `callback`; `nil` means "not given".
--- Cancelling the prompt aborts without calling back.
local function prompt_depth(callback)
  local current = filer().depth()
  vim.ui.input({ prompt = ("filer: depth (empty = %s): "):format(tostring(current)) }, function(input)
    if input == nil then
      return
    end
    input = vim.trim(input)
    if input == "" then
      callback(nil)
      return
    end
    local number = tonumber(input)
    if not number or number < 0 or number % 1 ~= 0 then
      vim.notify("filer: depth must be a whole number >= 0", vim.log.levels.ERROR)
      return
    end
    callback(math.floor(number))
  end)
end

--- Bundles with a one-off depth: `{}` when none was typed, so the configured
--- depth applies unchanged.
local function with_depth(callback)
  return function(depth)
    callback(depth and { depth = depth } or {})
  end
end

vim.api.nvim_create_user_command("FilerSearch", function()
  filer().search()
end, { desc = "filer: pick types and bundle them for chat" })

vim.api.nvim_create_user_command("FilerHere", function(args)
  if args.bang then
    prompt_depth(with_depth(function(opts)
      filer().here(opts)
    end))
    return
  end
  filer().here()
end, { bang = true, desc = "filer: bundle current file with its imports (! asks for a depth)" })

vim.api.nvim_create_user_command("FilerShow", function(args)
  local branch = args.args ~= "" and args.args or nil
  filer().show({ branch = branch })
end, { nargs = "?", desc = "filer: reopen the saved bundle (optionally for another branch)" })

vim.api.nvim_create_user_command("FilerReset", function()
  filer().reset()
end, { desc = "filer: forget this branch's bundle" })

vim.api.nvim_create_user_command("FilerResetAll", function()
  filer().reset_all()
end, { desc = "filer: forget every branch's bundle" })

vim.api.nvim_create_user_command("FilerDepth", function(args)
  local value = vim.trim(args.args)
  if value == "" then
    local depth = filer().depth()
    local detail = depth == filer().default_depth() and ""
      or (" (setup default: %s)"):format(tostring(filer().default_depth()))
    vim.notify(("filer: depth %s%s"):format(tostring(depth), detail))
    return
  end
  filer().set_depth(value)
end, {
  nargs = "?",
  desc = "filer: show, set (n) or reset the import depth",
  complete = function()
    return { "0", "1", "2", "3", "reset" }
  end,
})

vim.api.nvim_create_user_command("FilerBundle", function(args)
  local paths = args.fargs
  if #paths == 1 and paths[1] == "%" then
    paths = { vim.api.nvim_buf_get_name(0) }
  end
  if args.bang then
    prompt_depth(with_depth(function(opts)
      filer().bundle(paths, opts)
    end))
    return
  end
  filer().bundle(paths)
end, {
  nargs = "+",
  bang = true,
  complete = "file",
  desc = "filer: bundle given files or path::Symbol excerpts (! asks for a depth)",
})
