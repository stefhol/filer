//! `filer` — bundle Java sources and their import closure for a chat LLM.

mod bundle;
mod index;
mod java;
mod resolve;
mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use bundle::{BundleFile, BundleOptions};
use index::{FileEntry, Index};
use resolve::{Reason, ResolveOptions};
use state::{BundleState, StateFile};

#[derive(Parser)]
#[command(
    name = "filer",
    version,
    about = "Bundle Java sources and their import closure into chat-ready markdown"
)]
struct Cli {
    /// Project root. Defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List Java types as JSON lines (used by the Neovim picker).
    Search(SearchArgs),
    /// Emit a markdown bundle for the given files plus their import closure.
    Bundle(BundleArgs),
    /// Print the import closure of a single file.
    Deps(DepsArgs),
    /// Build the index cache and print stats.
    Index,
    /// Show or clear the saved bundle state.
    State(StateArgs),
}

#[derive(Args)]
struct SearchArgs {
    /// Query matched against type names and paths (case-insensitive).
    #[arg(long, default_value = "", value_name = "TEXT")]
    query: String,
    /// Maximum number of results.
    #[arg(long, default_value_t = 200)]
    limit: usize,
    /// Human-readable output instead of JSON lines.
    #[arg(long)]
    plain: bool,
    /// Mark files already in the bundle at this state file.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
}

#[derive(Args)]
struct BundleArgs {
    /// Files to bundle, as paths relative to the root (or absolute paths).
    #[arg(required = true, value_name = "FILE")]
    files: Vec<String>,
    /// Import hops to follow. 0 bundles only the given files.
    #[arg(long, default_value_t = 2)]
    depth: u32,
    /// Also include files that share a package with a bundled file.
    #[arg(long)]
    with_same_package: bool,
    /// Also include build files (pom.xml, build.gradle, ...).
    #[arg(long)]
    with_config: bool,
    /// Bundle title shown in the header.
    #[arg(long)]
    title: Option<String>,
    /// Write the bundle to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Carry the previous bundle over from this state file and update it.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Start from an empty bundle instead of the saved state.
    #[arg(long)]
    reset: bool,
}

#[derive(Args)]
struct DepsArgs {
    /// File to inspect, relative to the root (or absolute).
    #[arg(value_name = "FILE")]
    file: String,
    /// Import hops to follow.
    #[arg(long, default_value_t = 2)]
    depth: u32,
    /// JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct StateArgs {
    /// Delete the state file.
    #[arg(long)]
    clear: bool,
    /// State file. Defaults to <root>/.filer/bundle.toml.
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    let root = cli
        .root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match cli.command {
        Command::Search(args) => search(&root, args),
        Command::Bundle(args) => bundle_cmd(&root, args),
        Command::Deps(args) => deps(&root, args),
        Command::Index => index_cmd(&root),
        Command::State(args) => state_cmd(&root, args),
    }
}

fn search(root: &Path, args: SearchArgs) {
    let index = Index::build(root);
    let included = args
        .state
        .as_ref()
        .and_then(|path| state::load(path))
        .filter(|saved| !saved.is_stale(&index.root))
        .map(|saved| {
            saved
                .files
                .into_iter()
                .map(|file| (file.path, (file.depth, Reason::from_str(&file.reason))))
                .collect::<HashMap<String, (u32, Reason)>>()
        })
        .unwrap_or_default();

    let query = args.query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();

    let mut matches: Vec<(u8, &FileEntry)> = index
        .files
        .iter()
        .filter_map(|file| score(file, &query, &tokens).map(|score| (score, file)))
        .collect();
    matches.sort_by(|(score_a, file_a), (score_b, file_b)| {
        score_a
            .cmp(score_b)
            .then_with(|| file_a.rel.cmp(&file_b.rel))
    });

    for (_, file) in matches.into_iter().take(args.limit) {
        let bundled = included.get(&file.rel).copied();
        if args.plain {
            let marker = if bundled.is_some() { "*" } else { " " };
            println!("{marker}\t{}\t{}", file.label(), file.rel);
        } else {
            let value = serde_json::json!({
                "path": file.rel,
                "fqn": file.primary_fqn(),
                "package": file.package,
                "types": file.types,
                "included": bundled.is_some(),
                "depth": bundled.map(|(depth, _)| depth),
                "reason": bundled.map(|(_, reason)| reason.as_str()),
            });
            println!("{value}");
        }
    }
}

fn score(file: &FileEntry, query: &str, tokens: &[&str]) -> Option<u8> {
    let fqn = file.primary_fqn().to_lowercase();
    let rel = file.rel.to_lowercase();

    if !tokens.is_empty() {
        let haystack = format!("{fqn} {rel}");
        if !tokens.iter().all(|token| haystack.contains(token)) {
            return None;
        }
    }

    let types: Vec<String> = file.types.iter().map(|ty| ty.to_lowercase()).collect();
    Some(if query.is_empty() {
        4
    } else if types.iter().any(|ty| ty == query) {
        0
    } else if types.iter().any(|ty| ty.starts_with(query)) {
        1
    } else if fqn.contains(query) {
        2
    } else if rel.contains(query) {
        3
    } else {
        4
    })
}

fn bundle_cmd(root: &Path, args: BundleArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let state_path = args.state.clone();

    // Files carried over from the previous bundle, unless the branch changed.
    let mut previous: HashMap<usize, (u32, Reason)> = HashMap::new();
    if let Some(path) = &state_path {
        if !args.reset {
            if let Some(saved) = state::load(path) {
                if saved.is_stale(&root) {
                    eprintln!(
                        "filer: dropping bundle from branch {} (now {})",
                        saved.branch,
                        state::current_branch(&root)
                    );
                } else {
                    for file in saved.files {
                        if let Some(&position) = index.by_rel.get(&file.path) {
                            previous.insert(position, (file.depth, Reason::from_str(&file.reason)));
                        }
                    }
                }
            }
        }
    }

    let mut seeds: Vec<usize> = Vec::new();
    let mut extras: Vec<BundleFile> = Vec::new();

    for input in &args.files {
        match locate(&index, &root, input) {
            Some(rel) => match index.by_rel.get(&rel) {
                Some(&position) => {
                    if !seeds.contains(&position) {
                        seeds.push(position);
                    }
                }
                None => extras.push(BundleFile {
                    rel: rel.clone(),
                    abs: root.join(&rel),
                    depth: 0,
                    reason: Reason::External,
                }),
            },
            None => eprintln!("filer: skipping {input}: not inside {}", root.display()),
        }
    }

    let resolved = resolve::closure_detailed(
        &index,
        &seeds,
        ResolveOptions {
            depth: args.depth,
            same_package: args.with_same_package,
        },
    );
    let fresh = resolved.len();

    let mut merged = previous.clone();
    for &(position, depth, reason) in &resolved {
        match merged.get(&position) {
            Some(&(existing_depth, _)) if existing_depth <= depth => {}
            _ => {
                merged.insert(position, (depth, reason));
            }
        }
    }

    let mut files: Vec<BundleFile> = merged
        .into_iter()
        .map(|(position, (depth, reason))| BundleFile {
            rel: index.files[position].rel.clone(),
            abs: index.files[position].abs.clone(),
            depth,
            reason,
        })
        .collect();
    files.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.rel.cmp(&b.rel)));

    if args.with_config {
        files.extend(config_files(&root));
    }
    files.extend(extras);

    if files.is_empty() {
        eprintln!("filer: nothing to bundle");
        std::process::exit(1);
    }

    let saved_state = if state_path.is_some() {
        let mut saved = BundleState::new(&root, args.depth);
        saved.files = files
            .iter()
            .map(|file| StateFile {
                path: file.rel.clone(),
                depth: file.depth,
                reason: file.reason.as_str().to_string(),
            })
            .collect();
        Some(saved)
    } else {
        None
    };

    let bundle_options = BundleOptions {
        title: args.title,
        depth: args.depth,
        appendix: saved_state.as_ref().map(state::to_toml),
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    if let (Some(path), Some(saved)) = (&state_path, &saved_state) {
        if let Err(error) = state::save(path, saved) {
            eprintln!("filer: cannot write state {}: {error}", path.display());
        }
    }

    match args.out {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, &markdown) {
                eprintln!("filer: cannot write {}: {error}", path.display());
                std::process::exit(1);
            }
            eprintln!(
                "filer: wrote {} ({} files, {} new)",
                path.display(),
                files.len(),
                fresh
            );
        }
        None => {
            print!("{markdown}");
            eprintln!("filer: {} files, {} new on this run", files.len(), fresh);
        }
    }
}

fn deps(root: &Path, args: DepsArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let Some(rel) = locate(&index, &root, &args.file) else {
        eprintln!("filer: {} is not inside {}", args.file, root.display());
        std::process::exit(1);
    };
    let Some(&position) = index.by_rel.get(&rel) else {
        eprintln!("filer: {rel} is not indexed (binary or skipped directory?)");
        std::process::exit(1);
    };

    let order = resolve::closure_detailed(
        &index,
        &[position],
        ResolveOptions {
            depth: args.depth,
            same_package: false,
        },
    );

    if args.json {
        let value: Vec<_> = order
            .iter()
            .map(|&(position, depth, reason)| {
                let file = &index.files[position];
                serde_json::json!({
                    "path": file.rel,
                    "fqn": file.primary_fqn(),
                    "depth": depth,
                    "reason": reason.as_str(),
                })
            })
            .collect();
        println!("{}", serde_json::json!(value));
        return;
    }

    for &(position, depth, reason) in &order {
        let suffix = match (depth, reason) {
            (0, _) => String::new(),
            (_, reason) => format!(" ({})", reason.as_str()),
        };
        println!(
            "{}{}{}",
            "  ".repeat(depth as usize),
            index.files[position].rel,
            suffix
        );
    }

    for import in resolve::unresolved_imports(&index, position) {
        eprintln!("unresolved (probably JDK/third-party): {import}");
    }
}

fn index_cmd(root: &Path) {
    let index = Index::build(root);
    println!("root:       {}", index.root.display());
    println!("java files: {}", index.files.len());
    println!("packages:   {}", index.by_package.len());
    println!("types:      {}", index.by_fqn.len());
    println!(
        "cache:      {} reused, {} parsed",
        index.reused_count, index.parsed_count
    );

    let path = state::default_path(&index.root);
    match state::load(&path) {
        Some(saved) => println!(
            "state:      {} files, branch {}, {}",
            saved.files.len(),
            if saved.branch.is_empty() {
                "none"
            } else {
                &saved.branch
            },
            if saved.is_stale(&index.root) {
                "STALE (branch changed)"
            } else {
                "current"
            }
        ),
        None => println!("state:      none at {}", path.display()),
    }
}

fn state_cmd(root: &Path, args: StateArgs) {
    let path = args.file.unwrap_or_else(|| {
        state::default_path(&root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
    });

    if args.clear {
        match state::clear(&path) {
            Ok(true) => println!("filer: cleared {}", path.display()),
            Ok(false) => println!("filer: no state at {}", path.display()),
            Err(error) => {
                eprintln!("filer: cannot remove {}: {error}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    let Some(saved) = state::load(&path) else {
        println!("filer: no state at {}", path.display());
        return;
    };
    println!("state:  {}", path.display());
    println!(
        "branch: {}",
        if saved.branch.is_empty() {
            "(not a git repo)"
        } else {
            &saved.branch
        }
    );
    if saved.is_stale(root) {
        println!(
            "note:   written on {}, now on {} — next bundle starts fresh",
            saved.branch,
            state::current_branch(root)
        );
    }
    println!("files:  {}", saved.files.len());
    for file in &saved.files {
        println!("  {} (depth {}, {})", file.path, file.depth, file.reason);
    }
}

fn locate(index: &Index, root: &Path, input: &str) -> Option<String> {
    let path = PathBuf::from(input);
    let absolute = if path.is_absolute() {
        path
    } else {
        let from_cwd = path
            .canonicalize()
            .ok()
            .filter(|candidate| candidate.starts_with(root));
        match from_cwd {
            Some(candidate) => candidate,
            None => root.join(&path),
        }
    };
    let absolute = absolute.canonicalize().ok()?;
    if !absolute.starts_with(&index.root) {
        return None;
    }
    Some(index::relative(&index.root, &absolute))
}

fn config_files(root: &Path) -> Vec<BundleFile> {
    const CANDIDATES: &[&str] = &[
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
    ];
    CANDIDATES
        .iter()
        .filter_map(|name| {
            let path = root.join(name);
            path.is_file().then(|| BundleFile {
                rel: name.to_string(),
                abs: path,
                depth: 0,
                reason: Reason::Config,
            })
        })
        .collect()
}
