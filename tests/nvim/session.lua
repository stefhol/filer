-- Headless checks for the plugin's session handling (see issue #73):
--
--   nvim --headless -l tests/nvim/session.lua
--
-- Exits non-zero when a check fails. No Telescope and no `filer` binary is
-- needed: the browser falls back to `vim.ui.select`, the state helpers are
-- exercised directly, and the one command that shells out (`context clear`) is
-- only reached through its guard.

local source = debug.getinfo(1, "S").source:sub(2)
local repo = vim.fn.fnamemodify(source, ":p:h:h:h")
package.path = repo .. "/lua/?.lua;" .. repo .. "/lua/?/init.lua;" .. package.path

local uv = vim.uv or vim.loop
local filer = require("filer")

local failures = 0
local function check(ok, message)
  if not ok then
    failures = failures + 1
    io.stderr:write("FAIL " .. message .. "\n")
  end
end

local function equal(actual, expected, message)
  check(
    vim.deep_equal(actual, expected),
    ("%s\n  expected: %s\n  actual:   %s"):format(message, vim.inspect(expected), vim.inspect(actual))
  )
end

-- Notifications are collected instead of printed; a couple of checks assert on
-- them.
local messages = {}
vim.notify = function(text)
  table.insert(messages, text)
end
local function last_message()
  return messages[#messages] or ""
end

-- A throwaway project with a sessions directory, no state file and no binary.
local root = vim.fn.tempname() .. "-filer-session"
vim.fn.mkdir(filer.sessions_dir(root), "p")
filer.config.bin = "filer-not-installed"
equal(filer.state_path(root), root .. "/.filer/bundle.toml", "no session means the default state")
equal(filer.session(root), nil, "no session remembered yet")

-- Names -----------------------------------------------------------------------
for _, id in ipairs({ "s1", "refactor-auth", "a.b_c-1", "A9", string.rep("x", 128) }) do
  check(filer.valid_session(id), ("%q is a valid session name"):format(id))
end
for _, id in ipairs({ "", ".", "..", "a/b", "a b", "a:b", "ümlaut", string.rep("x", 129) }) do
  check(not filer.valid_session(id), ("%q is rejected"):format(id))
end
check(not filer.valid_session(nil), "nil is rejected")

-- Listing ---------------------------------------------------------------------
for _, name in ipairs({ "s1.toml", "refactor-auth.toml", "a.b_c-1.toml", "notes.txt", ".gitignore" }) do
  vim.fn.writefile({ "" }, filer.sessions_dir(root) .. "/" .. name)
end
equal(filer.session_ids(root), { "a.b_c-1", "refactor-auth", "s1" }, "ids come from the session files")

-- Switching and remembering ---------------------------------------------------
equal(filer.set_session("s1", { root = root, show = false }), "s1", "set_session returns the id")
equal(filer.state_path(root), root .. "/.filer/sessions/s1.toml", "the state moves to the session file")
equal(vim.fn.readfile(filer.session_file(root)), { "s1" }, "the session is remembered in .filer/session")
equal(vim.fn.readfile(root .. "/.filer/.gitignore"), { "*" }, ".filer is kept out of git status")
check(last_message():find("s1", 1, true) ~= nil, "switching reports the session name")

-- A fresh load (a restarted Neovim) starts on the remembered session.
package.loaded["filer"] = nil
local reloaded = require("filer")
equal(reloaded.session(root), "s1", "the remembered session survives a reload")
filer = reloaded
filer.config.bin = "filer-not-installed"

-- Switching re-renders the saved bundle.
local shown = {}
filer.show = function(opts)
  table.insert(shown, opts)
end
filer.set_session("refactor-auth", { root = root })
equal(#shown, 1, "switching re-renders the bundle")
equal(shown[1].quiet_empty, true, "an empty session does not warn")

-- Back to the per-branch state.
filer.set_session("default", { root = root, show = false })
equal(filer.state_path(root), root .. "/.filer/bundle.toml", "default restores the branch state")
check(uv.fs_stat(filer.session_file(root)) == nil, "default forgets the pointer file")

-- Rejected names leave the state alone.
equal(filer.set_session("../evil", { root = root, show = false }), nil, "an invalid name is rejected")
equal(filer.state_path(root), root .. "/.filer/bundle.toml", "a rejected name does not switch")

-- Rows for the browser: default, the active (still unsaved) session, the saved
-- ones, then the create row. `context list` fails here, so the fallback listing
-- from disk is used.
filer.set_session("agent-2", { root = root, show = false })
local rows = filer.sessions(root)
equal(rows[1].id, "default", "the default state comes first")
equal(rows[#rows].create, true, "the last row creates a session")
local ids = vim.tbl_map(function(row)
  return row.id
end, vim.list_slice(rows, 2, #rows - 1))
equal(ids, { "a.b_c-1", "refactor-auth", "s1", "agent-2" }, "the saved sessions, then the unsaved active one")
local active_row = rows[#rows - 1]
check(active_row.current, "the active session is marked")
equal(active_row.path, nil, "an unsaved session has no path yet")

-- Telescope entries must never carry a table as `path`/`filename`: a nil
-- filename makes Telescope's previewer fall back to `entry.value`, which is the
-- row table, and `path_expand` errors on it (the "+ new session…" crash).
for _, row in ipairs(rows) do
  local entry = filer.session_entry(row, root)
  check(entry.value == row, "the entry keeps its row for the mappings")
  check(entry.path == nil or type(entry.path) == "string", "the entry path is a string or nil")
  check(entry.filename == nil or type(entry.filename) == "string", "the entry filename is a string or nil")
  -- The previewer must resolve the file itself: Telescope's `from_entry.path`
  -- would fall back to `entry.value` (the row table) and crash.
  local target = filer.session_preview_target(entry)
  check(target == nil or type(target) == "string", "the preview target is a string or nil")
end
local create_entry = filer.session_entry(rows[#rows], root)
check(create_entry.value.create, "the + row keeps its create flag")
equal(create_entry.filename, nil, "the + row has no state file to preview")
equal(filer.session_preview_target(create_entry), nil, "the + row previews no file")
local unsaved_entry = filer.session_entry(active_row, root)
equal(unsaved_entry.filename, nil, "an unsaved session has nothing to preview")
equal(filer.session_preview_target(unsaved_entry), nil, "an unsaved session previews no file")
local saved_entry = filer.session_entry({ id = "s1", path = ".filer/sessions/s1.toml" }, root)
equal(
  saved_entry.filename,
  root .. "/.filer/sessions/s1.toml",
  "a saved session previews its state file"
)
equal(
  filer.session_preview_target(saved_entry),
  root .. "/.filer/sessions/s1.toml",
  "the saved session previews the existing state file"
)
local stale_entry = filer.session_entry({ id = "gone", path = ".filer/sessions/gone.toml" }, root)
equal(stale_entry.filename, nil, "a missing state file is not previewed")
equal(filer.session_preview_target(stale_entry), nil, "a missing state file has no preview target")

-- Creating prompts for the name; the prompt is the name.
local prompted
vim.ui.input = function(_, callback)
  prompted = true
  callback("  my session ")
end
filer.session_create({ root = root })
check(prompted, "creating asks for a name")
equal(filer.session(root), "agent-2", "a name with a space is rejected")
check(last_message():find("invalid session name", 1, true) ~= nil, "the rejection explains the alphabet")

vim.ui.input = function(_, callback)
  callback("  team-a  ")
end
filer.session_create({ root = root })
equal(filer.session(root), "team-a", "the typed name is trimmed and becomes the session")

-- The default state is not a session, so deleting it is refused before the
-- binary runs.
equal(filer.session_delete("default", { root = root, force = true }), false, "the default state cannot be deleted")

-- `opts.state_file` wins over sessions, and switching says so.
filer.config.state_file = root .. "/custom.toml"
equal(filer.state_path(root), root .. "/custom.toml", "state_file still wins")
equal(filer.set_session("s1", { root = root, show = false }), nil, "state_file disables switching")
filer.config.state_file = nil
filer.config.state = false
equal(filer.set_session("s1", { root = root, show = false }), nil, "state = false disables switching")
filer.config.state = true

-- The user command exists and completes session names.
vim.g.loaded_filer = nil
dofile(repo .. "/plugin/filer.lua")
local command = vim.api.nvim_get_commands({})["FilerSession"]
check(command ~= nil, ":FilerSession is defined")
if command ~= nil then
  equal(command.nargs, "?", ":FilerSession takes an optional name")
end
-- Completion resolves the project root from the working directory.
vim.fn.chdir(root)
local complete = vim.fn.getcompletion("FilerSession ", "cmdline")
vim.fn.chdir(repo)
equal(vim.tbl_contains(complete, "default"), true, "completion offers the default state")
equal(vim.tbl_contains(complete, "s1"), true, "completion offers saved sessions")

vim.fn.delete(root, "rf")

if failures > 0 then
  io.stderr:write(("%d check(s) failed\n"):format(failures))
  vim.cmd("cquit 1")
end
print("ok — session checks passed")
