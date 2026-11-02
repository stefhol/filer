//! Import resolution and transitive closure.

use std::collections::{HashMap, VecDeque};

use crate::index::Index;
use crate::java::Import;
#[cfg(test)]
use crate::java::ImportKind;

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
        return index
            .by_package
            .get(&import.path)
            .cloned()
            .unwrap_or_default();
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
        }
    }

    Vec::new()
}

/// Breadth-first closure over imports. Returns `(file index, depth)` pairs in
/// discovery order, seeds first (depth 0), duplicates removed.
pub fn closure(index: &Index, seeds: &[usize], options: ResolveOptions) -> Vec<(usize, u32)> {
    let mut depth_of: HashMap<usize, u32> = HashMap::new();
    let mut order: Vec<(usize, u32)> = Vec::new();
    let mut queue: VecDeque<usize> = VecDeque::new();

    for &seed in seeds {
        if depth_of.insert(seed, 0).is_none() {
            order.push((seed, 0));
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

        let mut neighbours: Vec<usize> = file
            .imports
            .iter()
            .flat_map(|import| resolve_import(index, import))
            .collect();

        if options.same_package {
            if let Some(package) = &file.package {
                if let Some(siblings) = index.by_package.get(package) {
                    neighbours.extend(siblings.iter().copied());
                }
            }
        }

        for neighbour in neighbours {
            if depth_of.contains_key(&neighbour) {
                continue;
            }
            let depth = current_depth + 1;
            depth_of.insert(neighbour, depth);
            order.push((neighbour, depth));
            queue.push_back(neighbour);
        }
    }

    order
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
