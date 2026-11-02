-- Ready-to-use lazy.nvim spec (LazyVim and kickstart.nvim).
-- LazyVim:   copy to ~/.config/nvim/lua/plugins/filer.lua
-- kickstart: copy to ~/.config/nvim/lua/custom/plugins/filer.lua
-- Adjust `dir` to where this repo lives.
return {
  {
    dir = "~/Work/filer",
    name = "filer",
    -- Commands are defined by plugin/filer.lua, so lazy-loading on them is safe.
    cmd = { "FilerSearch", "FilerHere", "FilerBundle", "FilerShow", "FilerReset", "FilerResetAll" },
    keys = {
      { "<leader>fc", function() require("filer").search() end, desc = "filer: bundle for chat" },
      { "<leader>fh", function() require("filer").here() end, desc = "filer: bundle current file" },
      {
        "<leader>fC",
        function()
          require("filer").bundle({ vim.api.nvim_buf_get_name(0) }, { depth = 0 })
        end,
        desc = "filer: bundle current file only",
      },
    },
    opts = {
      -- Path to the binary. nil = $PATH, then this repo's target/{release,debug}/filer.
      bin = nil,
      -- Project root. nil = detect via .git / pom.xml / build.gradle(.kts) / build.sbt / gradlew / mvnw.
      root = nil,
      -- Import hops to follow when bundling.
      depth = 2,
      -- Also include files sharing a package with a bundled file.
      same_package = false,
      -- Also include pom.xml / build.gradle / settings.gradle / build.sbt.
      with_config = false,
      -- How the bundle buffer opens: "vsplit", "split", "tabnew".
      split = "vsplit",
      -- true = also copy the bundle to the system clipboard.
      copy = false,
      -- Remember what is bundled in <root>/.filer/bundle.toml and mark it in
      -- the picker. Reset with :FilerReset. Branch changes reset it
      -- automatically.
      state = true,
      -- State file path. nil = <root>/.filer/bundle.toml
      state_file = nil,
    },
  },
}
