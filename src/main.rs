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
use state::{BranchBundle, StateDoc, StateFile};

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
    /// Re-render the saved bundle from the state file.
    Show(ShowArgs),
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
    /// Branch whose bundle is marked. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
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
struct ShowArgs {
    /// State file. Defaults to <root>/.filer/bundle.toml.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Branch to show. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
    /// Bundle title shown in the header.
    #[arg(long)]
    title: Option<String>,
    /// Write the bundle to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct StateArgs {
    /// Delete the bundle for one branch (see --branch).
    #[arg(long)]
    clear: bool,
    /// Delete every branch's bundle.
    #[arg(long)]
    all: bool,
    /// Branch to clear or list. Defaults to the current branch.
    #[arg(long, value_name = "BRANCH")]
    branch: Option<String>,
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
        Command::Show(args) => show_cmd(&root, args),
        Command::Index => index_cmd(&root),
        Command::State(args) => state_cmd(&root, args),
    }
}

fn search(root: &Path, args: SearchArgs) {
    let index = Index::build(root);
    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| state::current_branch(&index.root));
    let included = args
        .state
        .as_ref()
        .and_then(|path| state::load(path))
        .and_then(|doc| doc.bundle(&branch).cloned())
        .map(|bundle| {
            bundle
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

    let branch = state::current_branch(&root);

    // Files carried over from this branch's previous bundle.
    let mut previous: HashMap<usize, (u32, Reason)> = HashMap::new();
    let doc = state_path.as_ref().and_then(|path| state::load(path));
    if let Some(saved) = doc.as_ref() {
        if !args.reset {
            if let Some(bundle) = saved.bundle(&branch) {
                for file in &bundle.files {
                    if let Some(&position) = index.by_rel.get(&file.path) {
                        previous.insert(position, (file.depth, Reason::from_str(&file.reason)));
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

    let branch_bundle = if state_path.is_some() {
        Some(BranchBundle {
            name: branch.clone(),
            created: state::now(),
            depth: args.depth,
            files: files
                .iter()
                .map(|file| StateFile {
                    path: file.rel.clone(),
                    depth: file.depth,
                    reason: file.reason.as_str().to_string(),
                })
                .collect(),
        })
    } else {
        None
    };

    // The appendix of a bundle carries only the branch it was built for.
    let appendix = branch_bundle.as_ref().map(|bundle| {
        let mut shown = StateDoc::new(&root);
        shown.upsert(bundle.clone());
        state::to_toml(&shown)
    });

    let bundle_options = BundleOptions {
        title: args.title,
        depth: args.depth,
        appendix,
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    if let (Some(path), Some(bundle)) = (&state_path, branch_bundle) {
        let mut doc = doc.unwrap_or_else(|| StateDoc::new(&root));
        doc.upsert(bundle);
        if let Err(error) = state::save(path, &doc) {
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

/// Re-renders one branch's bundle from the state file: same files, same depths
/// and reasons, current file contents.
fn show_cmd(root: &Path, args: ShowArgs) {
    let index = Index::build(root);
    let root = index.root.clone();
    let path = args
        .state
        .clone()
        .unwrap_or_else(|| state::default_path(&root));

    let Some(doc) = state::load(&path) else {
        eprintln!(
            "filer: no bundle state at {} — bundle something first",
            path.display()
        );
        std::process::exit(1);
    };

    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| state::current_branch(&root));

    let Some(bundle) = doc.bundle(&branch).cloned() else {
        let available: Vec<String> = doc
            .bundles
            .iter()
            .map(|bundle| {
                format!(
                    "{} ({})",
                    state::branch_label(&bundle.name),
                    bundle.files.len()
                )
            })
            .collect();
        eprintln!(
            "filer: no bundle for branch {} (have: {})",
            state::branch_label(&branch),
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        );
        std::process::exit(1);
    };

    let mut files: Vec<BundleFile> = Vec::new();
    let mut shown = bundle.clone();
    shown.files.clear();

    for file in &bundle.files {
        let abs = root.join(&file.path);
        let reason = Reason::from_str(&file.reason);
        match index.by_rel.get(&file.path) {
            Some(&position) => files.push(BundleFile {
                rel: index.files[position].rel.clone(),
                abs: index.files[position].abs.clone(),
                depth: file.depth,
                reason,
            }),
            None if abs.is_file() => files.push(BundleFile {
                rel: file.path.clone(),
                abs,
                depth: file.depth,
                reason,
            }),
            None => {
                eprintln!("filer: dropping {}: no longer in the project", file.path);
                continue;
            }
        }
        shown.files.push(file.clone());
    }

    if files.is_empty() {
        eprintln!("filer: bundle is empty ({})", path.display());
        std::process::exit(1);
    }

    let appendix = {
        let mut single = StateDoc::new(&root);
        single.upsert(shown);
        state::to_toml(&single)
    };

    let bundle_options = BundleOptions {
        title: args.title,
        depth: bundle.depth,
        appendix: Some(appendix),
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    match args.out {
        Some(out) => {
            if let Err(error) = std::fs::write(&out, &markdown) {
                eprintln!("filer: cannot write {}: {error}", out.display());
                std::process::exit(1);
            }
            eprintln!(
                "filer: wrote {} ({} files for branch {} from {})",
                out.display(),
                files.len(),
                state::branch_label(&branch),
                path.display()
            );
        }
        None => {
            print!("{markdown}");
            eprintln!(
                "filer: {} files for branch {} from {}",
                files.len(),
                state::branch_label(&branch),
                path.display()
            );
        }
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
    let branch = state::current_branch(&index.root);
    match state::load(&path) {
        Some(doc) => {
            let current = doc.bundle(&branch).map_or(0, |bundle| bundle.files.len());
            println!(
                "bundle:     {} files on {}",
                current,
                state::branch_label(&branch)
            );
            let others: Vec<String> = doc
                .bundles
                .iter()
                .filter(|bundle| bundle.name != branch)
                .map(|bundle| {
                    format!(
                        "{} ({})",
                        state::branch_label(&bundle.name),
                        bundle.files.len()
                    )
                })
                .collect();
            if !others.is_empty() {
                println!("other branches: {}", others.join(", "));
            }
        }
        None => println!("bundle:     none at {}", path.display()),
    }
}

fn state_cmd(root: &Path, args: StateArgs) {
    let path = args.file.clone().unwrap_or_else(|| {
        state::default_path(&root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
    });
    let current = state::current_branch(root);

    if args.all {
        match state::clear(&path) {
            Ok(true) => println!("filer: cleared every branch ({})", path.display()),
            Ok(false) => println!("filer: no state at {}", path.display()),
            Err(error) => {
                eprintln!("filer: cannot remove {}: {error}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    if args.clear {
        let target = args.branch.clone().unwrap_or_else(|| current.clone());
        let Some(mut doc) = state::load(&path) else {
            println!("filer: no state at {}", path.display());
            return;
        };
        if !doc.remove(&target) {
            println!(
                "filer: no bundle for {} in {}",
                state::branch_label(&target),
                path.display()
            );
            return;
        }
        if let Err(error) = state::save(&path, &doc) {
            eprintln!("filer: cannot write {}: {error}", path.display());
            std::process::exit(1);
        }
        println!(
            "filer: cleared bundle for {} ({})",
            state::branch_label(&target),
            path.display()
        );
        return;
    }

    let Some(doc) = state::load(&path) else {
        println!("filer: no state at {}", path.display());
        return;
    };
    println!("state:    {}", path.display());
    println!("current:  {}", state::branch_label(&current));
    println!("branches: {}", doc.bundles.len());

    let target = args.branch.clone().unwrap_or_else(|| current.clone());
    let Some(bundle) = doc.bundle(&target) else {
        println!(
            "filer: no bundle for {} — stored: {}",
            state::branch_label(&target),
            doc.bundles
                .iter()
                .map(|bundle| format!(
                    "{} ({})",
                    state::branch_label(&bundle.name),
                    bundle.files.len()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return;
    };

    println!(
        "bundle:   {} files, depth {}, created {}",
        bundle.files.len(),
        bundle.depth,
        bundle.created
    );
    for file in &bundle.files {
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
