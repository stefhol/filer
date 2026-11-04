-- Ready-to-use lazy.nvim spec (LazyVim and kickstart.nvim).
-- The plugin is fetched from GitHub master; no local checkout is needed.
-- Install the `filer` binary first with the release installer (see README), so
-- it is on $PATH.
-- LazyVim:   copy to ~/.config/nvim/lua/plugins/filer.lua
-- kickstart: copy to ~/.config/nvim/lua/custom/plugins/filer.lua
return {
  {
    "stefhol/filer",
    name = "filer",
    -- Commands are defined by plugin/filer.lua, so lazy-loading on them is safe.
    cmd = {
      "FilerSearch",
      "FilerHere",
      "FilerBundle",
      "FilerDepth",
      "FilerShow",
      "FilerSession",
      "FilerReset",
      "FilerResetAll",
    },
    keys = {
      { "<leader>fc", function() require("filer").search() end, desc = "filer: bundle for chat" },
      { "<leader>fs", "<cmd>FilerSession<cr>", desc = "filer: switch session" },
      { "<leader>fh", function() require("filer").here() end, desc = "filer: bundle current file" },
      { "<leader>fH", "<cmd>FilerHere!<cr>", desc = "filer: bundle current file (ask depth)" },
      { "<leader>fd", "<cmd>FilerDepth<cr>", desc = "filer: show import depth" },
      {
        "<leader>fC",
        function()
          require("filer").bundle({ vim.api.nvim_buf_get_name(0) }, { depth = 0 })
        end,
        desc = "filer: bundle current file only",
      },
    },
    opts = {
      -- Path to the binary. nil = $PATH (where the release installer puts it).
      bin = nil,
      -- Project root. nil = detect via .git / pom.xml / build.gradle(.kts) / build.sbt / tsconfig / package.json / *.csproj.
      root = nil,
      -- Import hops to follow when bundling. Change live with :FilerDepth [n],
      -- or ask per call with :FilerHere! / :FilerBundle!.
      depth = 2,
      -- Also include files sharing a package with a bundled file.
      same_package = false,
      -- Also include pom.xml / gradle / build.sbt / tsconfig / csproj.
      with_config = false,
      -- How the bundle buffer opens: "vsplit", "split", "tabnew".
      split = "vsplit",
      -- true = also copy the bundle to the system clipboard.
      copy = false,
      -- Remember what is bundled in <root>/.filer/bundle.toml and mark it in
      -- the picker. Reset with :FilerReset. Branch changes reset it
      -- automatically.
      state = true,
      -- State file path. nil = <root>/.filer/bundle.toml. Setting it opts out
      -- of sessions.
      state_file = nil,
    },
  },
}
