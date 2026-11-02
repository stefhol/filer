if vim.g.loaded_filer == 1 then
  return
end
vim.g.loaded_filer = 1

local function filer()
  return require("filer")
end

vim.api.nvim_create_user_command("FilerSearch", function()
  filer().search()
end, { desc = "filer: pick Java types and bundle them for chat" })

vim.api.nvim_create_user_command("FilerHere", function()
  filer().here()
end, { desc = "filer: bundle current file with its imports" })

vim.api.nvim_create_user_command("FilerBundle", function(args)
  local paths = args.fargs
  if #paths == 1 and paths[1] == "%" then
    paths = { vim.api.nvim_buf_get_name(0) }
  end
  filer().bundle(paths)
end, {
  nargs = "+",
  complete = "file",
  desc = "filer: bundle the given files with their imports",
})
