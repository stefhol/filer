//! `filer` — bundle Java sources and their import closure for a chat LLM.

mod bundle;
mod index;
mod java;
mod resolve;

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use bundle::{BundleFile, BundleOptions};
use index::{FileEntry, Index};
use resolve::ResolveOptions;

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
    }
}

fn search(root: &Path, args: SearchArgs) {
    let index = Index::build(root);
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
        if args.plain {
            println!("{}\t{}", file.label(), file.rel);
        } else {
            let value = serde_json::json!({
                "path": file.rel,
                "fqn": file.primary_fqn(),
                "package": file.package,
                "types": file.types,
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
                    note: Some("not indexed, no imports followed".to_string()),
                }),
            },
            None => eprintln!("filer: skipping {input}: not inside {}", root.display()),
        }
    }

    let options = ResolveOptions {
        depth: args.depth,
        same_package: args.with_same_package,
    };
    let order = resolve::closure(&index, &seeds, options);

    let mut files: Vec<BundleFile> = order
        .iter()
        .map(|&(position, depth)| {
            let entry = &index.files[position];
            BundleFile {
                rel: entry.rel.clone(),
                abs: entry.abs.clone(),
                depth,
                note: None,
            }
        })
        .collect();

    if args.with_config {
        files.extend(config_files(&root));
    }
    files.extend(extras);

    if files.is_empty() {
        eprintln!("filer: nothing to bundle");
        std::process::exit(1);
    }

    let bundle_options = BundleOptions {
        title: args.title,
        depth: args.depth,
        ..Default::default()
    };
    let markdown = bundle::render(&index, &files, &bundle_options);

    match args.out {
        Some(path) => {
            if let Err(error) = std::fs::write(&path, &markdown) {
                eprintln!("filer: cannot write {}: {error}", path.display());
                std::process::exit(1);
            }
            eprintln!("filer: wrote {} ({} files)", path.display(), files.len());
        }
        None => print!("{markdown}"),
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

    let order = resolve::closure(
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
            .map(|&(position, depth)| {
                let file = &index.files[position];
                serde_json::json!({ "path": file.rel, "fqn": file.primary_fqn(), "depth": depth })
            })
            .collect();
        println!("{}", serde_json::json!(value));
        return;
    }

    for &(position, depth) in &order {
        println!(
            "{}{}",
            "  ".repeat(depth as usize),
            index.files[position].rel
        );
    }

    for import in resolve::unresolved_imports(&index, position) {
        eprintln!("unresolved (probably JDK/third-party): {import}");
    }
}

fn index_cmd(root: &Path) {
    let index = Index::build(root);
    println!("root:      {}", index.root.display());
    println!("java files: {}", index.files.len());
    println!("packages:   {}", index.by_package.len());
    println!("types:      {}", index.by_fqn.len());
    println!(
        "cache:      {} reused, {} parsed",
        index.reused_count, index.parsed_count
    );
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
                note: Some("build config".to_string()),
            })
        })
        .collect()
}
