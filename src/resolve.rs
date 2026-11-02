//! Import resolution and transitive closure.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::index::{FileEntry, Index};
use crate::lang::{Import, ImportKind};

/// Why a file ended up in a bundle. Serialized as a kebab-case string in the
/// state file, e.g. `reason = "same-package"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Picked explicitly.
    Seed,
    Import,
    StaticImport,
    WildcardImport,
    /// Referenced by name without an import, in the same package.
    SamePackage,
    /// Referenced by name and unique in the repo.
    Usage,
    /// Pulled in by `--with-same-package`.
    Package,
    /// Build file (pom.xml, build.gradle).
    Config,
    /// Handed in but not part of the index.
    External,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Seed => "seed",
            Reason::Import => "import",
            Reason::StaticImport => "static-import",
            Reason::WildcardImport => "wildcard-import",
            Reason::SamePackage => "same-package",
            Reason::Usage => "usage",
            Reason::Package => "package",
            Reason::Config => "config",
            Reason::External => "external",
        }
    }

    pub fn from_str(text: &str) -> Reason {
        match text {
            "seed" => Reason::Seed,
            "import" => Reason::Import,
            "static-import" => Reason::StaticImport,
            "wildcard-import" => Reason::WildcardImport,
            "same-package" => Reason::SamePackage,
            "package" => Reason::Package,
            "config" => Reason::Config,
            "external" => Reason::External,
            _ => Reason::Usage,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolveOptions {
    /// How many import hops away from the seeds to include. 0 = seeds only.
    pub depth: u32,
    /// Also pull in every file sharing a package with an included file.
    pub same_package: bool,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            depth: 2,
            same_package: false,
        }
    }
}

/// Resolves one import to file indexes in the project.
///
/// Static member imports and nested types (`a.b.C.Inner`) are handled by
/// trimming trailing segments until a known type name is hit.
pub fn resolve_import(index: &Index, import: &Import) -> Vec<usize> {
    if import.is_wildcard() {
        // `a.b._` is usually a package, but Scala also lets you import the
        // members of an object (`a.b.C._`, `a.b.C.given`) and Java has static
        // wildcard imports. A path can be both a package and a type, so take
        // the union instead of guessing.
        let mut resolved = index
            .by_package
            .get(&import.path)
            .cloned()
            .unwrap_or_default();
        if let Some(&position) = index.by_fqn.get(&import.path) {
            if !resolved.contains(&position) {
                resolved.push(position);
            }
        }
        return resolved;
    }

    let segments: Vec<&str> = import.path.split('.').collect();
    if segments.len() < 2 {
        // A single-segment import can only be a type in the default package.
        return index
            .by_fqn
            .get(&import.path)
            .map(|&position| vec![position])
            .unwrap_or_default();
    }

    // `a.b.C.member` -> try `a.b.C.member`, `a.b.C`, `a.b`, ... stop at 2.
    for end in (2..=segments.len()).rev() {
        let candidate = segments[..end].join(".");
        if let Some(&position) = index.by_fqn.get(&candidate) {
            return vec![position];
        }
    }

    // Fall back to the conventional package-layout path.
    if let Some(name) = segments.last() {
        let package = segments[..segments.len() - 1].join(".");
        if let Some(files) = index.by_package.get(&package) {
            if let Some(&position) = files
                .iter()
                .find(|&&position| index.files[position].types.iter().any(|ty| ty == name))
            {
                return vec![position];
            }

            // Scala package objects and files with only top-level definitions
            // declare no indexed type, but they are where the imported member
            // lives (`import a.b.Version` with `package object b`).
            let definition_only: Vec<usize> = files
                .iter()
                .copied()
                .filter(|&position| index.files[position].types.is_empty())
                .collect();
            if !definition_only.is_empty() {
                return definition_only;
            }
        }
    }

    Vec::new()
}

/// Resolves type names *used* in the file. Java needs no import for types in
/// the same package, so a class that only references `DemoDocument.class`
/// still depends on `DemoDocument.java`.
///
/// Explicitly imported names are skipped (the import resolution handles those),
/// and ambiguous simple names are left out rather than guessed wrong.
pub fn resolve_uses(index: &Index, file: &FileEntry) -> Vec<(usize, Reason)> {
    let imported: HashSet<&str> = file
        .imports
        .iter()
        .filter(|import| !import.is_wildcard())
        .filter_map(|import| import.path.rsplit('.').next())
        .collect();

    let mut resolved = Vec::new();
    for name in &file.uses {
        if imported.contains(name.as_str()) {
            continue;
        }
        let Some(candidates) = index.by_simple.get(name) else {
            continue;
        };
        let same_package: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&position| index.files[position].package == file.package)
            .collect();
        match same_package.len() {
            1 => resolved.push((same_package[0], Reason::SamePackage)),
            0 if candidates.len() == 1 => resolved.push((candidates[0], Reason::Usage)),
            _ => {}
        }
    }
    resolved
}

/// Breadth-first closure over imports and type usages, keeping track of why
/// each file was pulled in. Returns `(file index, depth, reason)` in discovery
/// order, seeds first (depth 0), duplicates removed.
pub fn closure_detailed(
    index: &Index,
    seeds: &[usize],
    options: ResolveOptions,
) -> Vec<(usize, u32, Reason)> {
    let mut depth_of: HashMap<usize, u32> = HashMap::new();
    let mut order: Vec<(usize, u32, Reason)> = Vec::new();
    let mut queue: VecDeque<usize> = VecDeque::new();

    for &seed in seeds {
        if depth_of.insert(seed, 0).is_none() {
            order.push((seed, 0, Reason::Seed));
            queue.push_back(seed);
        }
    }

    while let Some(current) = queue.pop_front() {
        let current_depth = depth_of[&current];
        if current_depth >= options.depth {
            continue;
        }
        let Some(file) = index.files.get(current) else {
            continue;
        };

        let mut neighbours: Vec<(usize, Reason)> = Vec::new();
        for import in &file.imports {
            let reason = match import.kind {
                ImportKind::Type => Reason::Import,
                ImportKind::Static => Reason::StaticImport,
                ImportKind::Wildcard => Reason::WildcardImport,
            };
            neighbours.extend(
                resolve_import(index, import)
                    .into_iter()
                    .map(|p| (p, reason)),
            );
        }
        neighbours.extend(resolve_uses(index, file));

        if options.same_package {
            if let Some(package) = &file.package {
                if let Some(siblings) = index.by_package.get(package) {
                    neighbours.extend(siblings.iter().copied().map(|p| (p, Reason::Package)));
                }
            }
        }

        for (neighbour, reason) in neighbours {
            if depth_of.contains_key(&neighbour) {
                continue;
            }
            let depth = current_depth + 1;
            depth_of.insert(neighbour, depth);
            order.push((neighbour, depth, reason));
            queue.push_back(neighbour);
        }
    }

    order
}

/// Closure without provenance, for plain listings.
#[cfg(test)]
pub fn closure(index: &Index, seeds: &[usize], options: ResolveOptions) -> Vec<(usize, u32)> {
    closure_detailed(index, seeds, options)
        .into_iter()
        .map(|(position, depth, _)| (position, depth))
        .collect()
}

/// All imports of `file` that could not be resolved inside the project (JDK,
/// third-party libraries). Useful for heads-up messages.
pub fn unresolved_imports(index: &Index, file_position: usize) -> Vec<String> {
    let Some(file) = index.files.get(file_position) else {
        return Vec::new();
    };
    file.imports
        .iter()
        .filter(|import| resolve_import(index, import).is_empty())
        .map(|import| import.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FileEntry;
    use std::path::PathBuf;

    fn entry(rel: &str, package: &str, types: &[&str], imports: Vec<Import>) -> FileEntry {
        FileEntry {
            rel: rel.to_string(),
            abs: PathBuf::from(format!("/repo/{rel}")),
            package: Some(package.to_string()),
            types: types.iter().map(|t| t.to_string()).collect(),
            imports,
            uses: Vec::new(),
        }
    }

    fn import(path: &str, kind: ImportKind) -> Import {
        Import {
            path: path.to_string(),
            kind,
        }
    }

    fn fixture() -> Index {
        let files = vec![
            entry(
                "src/main/java/app/Service.java",
                "app",
                &["Service"],
                vec![
                    import("app.repo.UserRepository", ImportKind::Type),
                    import("app.model.User", ImportKind::Type),
                    import("java.util.List", ImportKind::Type),
                ],
            ),
            entry(
                "src/main/java/app/repo/UserRepository.java",
                "app.repo",
                &["UserRepository"],
                vec![import("app.model.User", ImportKind::Type)],
            ),
            entry(
                "src/main/java/app/model/User.java",
                "app.model",
                &["User"],
                vec![import("java.util.Objects", ImportKind::Type)],
            ),
            entry(
                "src/main/java/app/model/Unused.java",
                "app.model",
                &["Unused"],
                vec![],
            ),
        ];
        let mut index = Index {
            root: PathBuf::from("/repo"),
            files,
            ..Default::default()
        };
        for (position, file) in index.files.iter().enumerate() {
            index.by_rel.insert(file.rel.clone(), position);
            if let Some(package) = &file.package {
                index
                    .by_package
                    .entry(package.clone())
                    .or_default()
                    .push(position);
            }
            for fqn in file.fqns() {
                index.by_fqn.entry(fqn).or_insert(position);
            }
            for ty in &file.types {
                index
                    .by_simple
                    .entry(ty.clone())
                    .or_default()
                    .push(position);
            }
        }
        index
    }

    #[test]
    fn resolves_transitive_closure() {
        let index = fixture();
        let order = closure(
            &index,
            &[0],
            ResolveOptions {
                depth: 2,
                same_package: false,
            },
        );
        let paths: Vec<(&str, u32)> = order
            .iter()
            .map(|&(position, depth)| (index.files[position].rel.as_str(), depth))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("src/main/java/app/Service.java", 0),
                ("src/main/java/app/repo/UserRepository.java", 1),
                ("src/main/java/app/model/User.java", 1),
            ]
        );
    }

    #[test]
    fn resolves_same_package_references_without_imports() {
        let mut index = fixture();
        // RunService references Unused in its own package, with no import at all.
        index.files.push(FileEntry {
            rel: "src/main/java/app/model/RunService.java".to_string(),
            abs: PathBuf::from("/repo/src/main/java/app/model/RunService.java"),
            package: Some("app.model".to_string()),
            types: vec!["RunService".to_string()],
            imports: vec![],
            uses: vec!["Unused".to_string(), "String".to_string()],
        });
        let position = index.files.len() - 1;

        let resolved = resolve_uses(&index, &index.files[position]);
        assert_eq!(
            resolved,
            vec![(3, Reason::SamePackage)],
            "Unused is found by name, String is not in the repo"
        );
        assert_eq!(index.files[3].rel, "src/main/java/app/model/Unused.java");
    }

    #[test]
    fn explicit_imports_win_over_name_guessing() {
        let mut index = fixture();
        // `Service` refers to its own type and to two explicitly imported ones.
        index.files[0].uses = vec![
            "Service".to_string(),
            "User".to_string(),
            "UserRepository".to_string(),
        ];
        let resolved = resolve_uses(&index, &index.files[0]);
        // Imported names are left to resolve_import; only the self reference is
        // inferred here (and deduplicated by the closure).
        assert_eq!(resolved, vec![(0, Reason::SamePackage)]);
    }

    #[test]
    fn depth_zero_keeps_seeds_only() {
        let index = fixture();
        let order = closure(
            &index,
            &[0],
            ResolveOptions {
                depth: 0,
                same_package: false,
            },
        );
        assert_eq!(order.len(), 1);
    }

    #[test]
    fn static_and_wildcard_imports() {
        let index = fixture();
        assert_eq!(
            resolve_import(&index, &import("app.model.User.of", ImportKind::Static)),
            vec![2]
        );
        assert_eq!(
            resolve_import(&index, &import("app.model", ImportKind::Wildcard)).len(),
            2
        );
        assert!(resolve_import(&index, &import("java.util.List", ImportKind::Type)).is_empty());
    }

    #[test]
    fn member_of_a_package_object_resolves_to_its_file() {
        let files = vec![FileEntry {
            rel: "src/main/scala/app/util/package.scala".to_string(),
            abs: PathBuf::from("/repo/src/main/scala/app/util/package.scala"),
            package: Some("app.util".to_string()),
            types: Vec::new(),
            imports: Vec::new(),
            uses: Vec::new(),
        }];
        let mut index = Index {
            root: PathBuf::from("/repo"),
            files,
            ..Default::default()
        };
        index.by_package.insert("app.util".to_string(), vec![0]);
        assert_eq!(
            resolve_import(&index, &import("app.util.Version", ImportKind::Type)),
            vec![0]
        );
    }

    #[test]
    fn wildcard_can_resolve_to_an_object_path() {
        let index = fixture();
        // Scala's `import app.Service._` names a type, not a package.
        assert_eq!(
            resolve_import(&index, &import("app.Service", ImportKind::Wildcard)),
            vec![0]
        );
        // A real package still expands to its files.
        assert_eq!(
            resolve_import(&index, &import("app.model", ImportKind::Wildcard)),
            vec![2, 3]
        );
    }

    #[test]
    fn same_package_option_includes_siblings() {
        let index = fixture();
        let order = closure(
            &index,
            &[2],
            ResolveOptions {
                depth: 1,
                same_package: true,
            },
        );
        let rels: Vec<&str> = order
            .iter()
            .map(|&(position, _)| index.files[position].rel.as_str())
            .collect();
        assert!(rels.contains(&"src/main/java/app/model/Unused.java"));
    }
}
