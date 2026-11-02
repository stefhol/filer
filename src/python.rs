//! Python parsing via tree-sitter: imports (`import`, `from ... import`,
//! relative and `TYPE_CHECKING` imports), classes with their methods, and
//! module-level functions.
//!
//! A Python module's dotted name is its directory, so the parser needs the file
//! path relative to the project root (`app/model/user.py` -> `app.model`).

use tree_sitter::{Node, Parser};

use crate::lang::{
    end_line_of, finish_symbols, leading_comment_start, line_of, symbol_from_node, Call, Import,
    ImportKind, Member, MemberKind, Receiver, Source, Symbol, SymbolKind,
};

const COMMENTS: &[&str] = &["comment"];

pub struct PythonParser {
    parser: Parser,
}

impl PythonParser {
    pub fn new() -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .expect("tree-sitter-python grammar failed to load");
        Self { parser }
    }

    pub fn parse(&mut self, rel: &str, src: &str) -> Option<Source> {
        let tree = self.parser.parse(src, None)?;
        let root = tree.root_node();
        let mut out = Source {
            package: package_of(rel),
            ..Source::default()
        };

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            collect_statement(child, src, &mut out);
        }

        out.symbols.sort_by_key(|symbol| symbol.start);
        let mut uses = Vec::new();
        collect_uses(root, src, &mut uses);
        finish_symbols(&mut out, &uses);

        Some(out)
    }
}

impl Default for PythonParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Builtins no indexed file declares. A bare call to one is not an unresolved
/// call, it is simply not ours to resolve.
pub fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "__import__"
            | "abs"
            | "aiter"
            | "all"
            | "anext"
            | "any"
            | "ascii"
            | "bin"
            | "bool"
            | "breakpoint"
            | "bytearray"
            | "bytes"
            | "callable"
            | "chr"
            | "classmethod"
            | "compile"
            | "complex"
            | "delattr"
            | "dict"
            | "dir"
            | "divmod"
            | "enumerate"
            | "eval"
            | "exec"
            | "exit"
            | "filter"
            | "float"
            | "format"
            | "frozenset"
            | "getattr"
            | "globals"
            | "hasattr"
            | "hash"
            | "help"
            | "hex"
            | "id"
            | "input"
            | "int"
            | "isinstance"
            | "issubclass"
            | "iter"
            | "len"
            | "list"
            | "locals"
            | "map"
            | "max"
            | "memoryview"
            | "min"
            | "next"
            | "object"
            | "oct"
            | "open"
            | "ord"
            | "pow"
            | "print"
            | "property"
            | "quit"
            | "range"
            | "repr"
            | "reversed"
            | "round"
            | "set"
            | "setattr"
            | "slice"
            | "sorted"
            | "staticmethod"
            | "str"
            | "sum"
            | "super"
            | "tuple"
            | "type"
            | "vars"
            | "zip"
            // Builtin exceptions are raised, not called, but `raise X(...)` and
            // `except X(...)` parse as calls.
            | "ArithmeticError"
            | "AssertionError"
            | "AttributeError"
            | "BaseException"
            | "Exception"
            | "FileNotFoundError"
            | "ImportError"
            | "IndexError"
            | "KeyError"
            | "KeyboardInterrupt"
            | "LookupError"
            | "MemoryError"
            | "NameError"
            | "NotImplementedError"
            | "OSError"
            | "OverflowError"
            | "RuntimeError"
            | "StopIteration"
            | "SystemExit"
            | "TimeoutError"
            | "TypeError"
            | "UnicodeDecodeError"
            | "ValueError"
            | "ZeroDivisionError"
    )
}

/// Dotted package/module path of a file's directory (`app/model/user.py` is
/// `app.model`). Root-level modules have no package, like the default package
/// in Java.
fn package_of(rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop()?;
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

/// One top-level statement: declarations become symbols, imports are recorded.
fn collect_statement(node: Node<'_>, src: &str, out: &mut Source) {
    match node.kind() {
        "import_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                let path = imported_name(child, src);
                if !path.is_empty() {
                    out.imports.push(Import {
                        path,
                        kind: ImportKind::Module,
                    });
                }
            }
        }
        "import_from_statement" | "future_import_statement" => collect_from_import(node, src, out),
        // `if TYPE_CHECKING:` blocks hold the type-only imports of a module and
        // are the Python equivalent of an import that never runs.
        "if_statement" => {
            let condition = node
                .child_by_field_name("condition")
                .map(|condition| text_of(condition, src))
                .unwrap_or_default();
            if condition.contains("TYPE_CHECKING") {
                collect_nested_imports(node, src, out);
            }
        }
        "class_definition" => push_class(node, node, src, out),
        "function_definition" => push_function(node, node, src, out),
        "decorated_definition" => match node.child_by_field_name("definition") {
            Some(definition) if definition.kind() == "class_definition" => {
                push_class(definition, node, src, out);
            }
            Some(definition) if definition.kind() == "function_definition" => {
                push_function(definition, node, src, out);
            }
            _ => {}
        },
        _ => {}
    }
}

/// `from a.b import C, D as E` keeps one import per bound name (`a.b.C`), so the
/// resolver can map it to a class/function in `a.b`, or to the submodule
/// `a/b/C.py`. Relative modules keep their leading dots (`.`, `..a`).
fn collect_from_import(node: Node<'_>, src: &str, out: &mut Source) {
    let module = node
        .child_by_field_name("module_name")
        .map(|module| text_of(module, src))
        .unwrap_or_else(|| "__future__".to_string());
    let mut cursor = node.walk();
    let names: Vec<Node<'_>> = node.children_by_field_name("name", &mut cursor).collect();
    if names.is_empty() {
        // `from a.b import *`: the wildcard imports every public name of a.b.
        out.imports.push(Import {
            path: module,
            kind: ImportKind::Wildcard,
        });
        return;
    }
    for name in names {
        let leaf = imported_name(name, src);
        if leaf.is_empty() {
            continue;
        }
        // `from . import x` keeps the dot prefix as `path` (`.x`), not `..x`.
        let path = if module.ends_with('.') {
            format!("{module}{leaf}")
        } else {
            format!("{module}.{leaf}")
        };
        out.imports.push(Import {
            path,
            kind: ImportKind::Type,
        });
    }
}

/// Dotted name of one imported binding (`a.b.c`, or `b` in `x as b`).
fn imported_name(node: Node<'_>, src: &str) -> String {
    match node.kind() {
        "aliased_import" => node
            .child_by_field_name("name")
            .map(|name| text_of(name, src))
            .unwrap_or_default(),
        _ => text_of(node, src),
    }
}

/// Imports inside an `if TYPE_CHECKING:` block (including nested `if`/`elif`).
fn collect_nested_imports(node: Node<'_>, src: &str, out: &mut Source) {
    let mut stack = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        stack.push(child);
    }
    while let Some(current) = stack.pop() {
        match current.kind() {
            "import_statement" | "import_from_statement" | "future_import_statement" => {
                collect_statement(current, src, out);
            }
            // Declarations keep their own imports; do not steal them.
            "class_definition" | "function_definition" | "decorated_definition" => {}
            _ => {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    stack.push(child);
                }
            }
        }
    }
}

/// A `class` declaration: the type's span (decorators and doc comment
/// included), its base classes, its fields and one prunable member per method.
fn push_class(definition: Node<'_>, outer: Node<'_>, src: &str, out: &mut Source) {
    let Some(name) = definition
        .child_by_field_name("name")
        .map(|name| text_of(name, src))
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let mut symbol = symbol_from_node(outer, &name, SymbolKind::Type, COMMENTS, src);
    if let Some(superclasses) = definition.child_by_field_name("superclasses") {
        symbol.supertypes = bases_of(superclasses, src);
    }
    if let Some(body) = definition.child_by_field_name("body") {
        collect_class_body(body, src, &mut symbol);
    }
    out.symbols.push(symbol);
    if !out.types.contains(&name) {
        out.types.push(name);
    }
}

/// A module-level `def`: a symbol with its own calls and no members.
fn push_function(definition: Node<'_>, outer: Node<'_>, src: &str, out: &mut Source) {
    let Some(name) = definition
        .child_by_field_name("name")
        .map(|name| text_of(name, src))
    else {
        return;
    };
    if name.is_empty() {
        return;
    }
    let mut symbol = symbol_from_node(outer, &name, SymbolKind::Function, COMMENTS, src);
    symbol.params = parameters_of(definition, src);
    symbol.locals = locals_of(definition, src);
    symbol.calls = calls_in(definition, src);
    out.symbols.push(symbol);
}

/// Methods and class-level fields of one class body. Nested classes and
/// conditionally defined methods stay part of the enclosing class span.
fn collect_class_body(body: Node<'_>, src: &str, symbol: &mut Symbol) {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        let (definition, outer) = match child.kind() {
            "function_definition" => (child, child),
            "decorated_definition" => match child.child_by_field_name("definition") {
                Some(definition) if definition.kind() == "function_definition" => {
                    (definition, child)
                }
                _ => continue,
            },
            "expression_statement" => {
                class_fields(child, src, &mut symbol.fields);
                continue;
            }
            _ => continue,
        };
        let Some(member) = method_of(definition, outer, src) else {
            continue;
        };
        self_fields(
            definition,
            src,
            &member.params,
            &member.locals,
            &mut symbol.fields,
        );
        symbol.members.push(member);
    }
    let mut seen = std::collections::HashSet::new();
    symbol.fields.retain(|(name, _)| seen.insert(name.clone()));
}

fn method_of(node: Node<'_>, outer: Node<'_>, src: &str) -> Option<Member> {
    let name = node.child_by_field_name("name")?;
    let name = text_of(name, src);
    if name.is_empty() {
        return None;
    }
    let start = leading_comment_start(outer, COMMENTS);
    Some(Member {
        name,
        kind: MemberKind::Method,
        start,
        end: outer.end_byte(),
        line: line_of(src, start),
        end_line: end_line_of(src, start, outer.end_byte()),
        body_start: node
            .child_by_field_name("body")
            .map(|body| body.start_byte()),
        params: parameters_of(node, src),
        locals: locals_of(node, src),
        calls: calls_in(node, src),
    })
}

/// Simple names of the base classes in `class Foo(Bar, pkg.Baz[T], **kwargs)`.
fn bases_of(superclasses: Node<'_>, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = superclasses.walk();
    for base in superclasses.named_children(&mut cursor) {
        if base.kind() == "keyword_argument" {
            continue;
        }
        if let Some(name) = simple_type_name(base, src) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Class-level annotated assignments (`repo: Repo`) are the fields a method
/// reaches through `self.repo`.
fn class_fields(statement: Node<'_>, src: &str, out: &mut Vec<(String, String)>) {
    let mut cursor = statement.walk();
    for assignment in statement.named_children(&mut cursor) {
        if assignment.kind() != "assignment" {
            continue;
        }
        let Some(left) = assignment.child_by_field_name("left") else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }
        let Some(ty) = assignment
            .child_by_field_name("type")
            .and_then(|ty| simple_type_name(ty, src))
        else {
            continue;
        };
        out.push((text_of(left, src), ty));
    }
}

/// Fields assigned through `self.x`: `self.repo = repo` takes the type of the
/// `repo` parameter, `self.repo: Repo` its annotation. `self.repo.find()` then
/// resolves like a field access in Java or Rust.
fn self_fields(
    node: Node<'_>,
    src: &str,
    params: &[(String, String)],
    locals: &[(String, String)],
    out: &mut Vec<(String, String)>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "assignment" {
            if let Some((name, ty)) = self_field(&current, src, params, locals) {
                out.push((name, ty));
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn self_field(
    assignment: &Node<'_>,
    src: &str,
    params: &[(String, String)],
    locals: &[(String, String)],
) -> Option<(String, String)> {
    let left = assignment.child_by_field_name("left")?;
    if left.kind() != "attribute" {
        return None;
    }
    let object = left.child_by_field_name("object")?;
    if object.kind() != "identifier" || text_of(object, src) != "self" {
        return None;
    }
    let name = text_of(left.child_by_field_name("attribute")?, src);
    if name.is_empty() {
        return None;
    }
    // `self.repo: Repo` carries its own annotation.
    if let Some(ty) = assignment
        .child_by_field_name("type")
        .and_then(|ty| simple_type_name(ty, src))
    {
        return Some((name, ty));
    }
    let right = assignment.child_by_field_name("right")?;
    // `self.repo = repo`: the parameter's annotation is the field's type.
    if right.kind() == "identifier" {
        let source = text_of(right, src);
        let ty = params
            .iter()
            .chain(locals.iter())
            .find(|(local, _)| *local == source)
            .map(|(_, ty)| ty.clone())?;
        return Some((name, ty));
    }
    // `self.repo = Repo()` types itself.
    inferred_type(right, src).map(|ty| (name, ty))
}

/// Local variables with an annotation (`x: Repo = ...`) or a constructor value
/// (`x = Repo()`), for receiver typing inside a method or function body.
fn locals_of(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "assignment" {
            if let Some(left) = current.child_by_field_name("left") {
                if left.kind() == "identifier" {
                    let name = text_of(left, src);
                    let ty = current
                        .child_by_field_name("type")
                        .and_then(|ty| simple_type_name(ty, src))
                        .or_else(|| {
                            current
                                .child_by_field_name("right")
                                .and_then(|right| inferred_type(right, src))
                        });
                    if let (false, Some(ty)) = (name.is_empty(), ty) {
                        out.push((name, ty));
                    }
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Typed parameters (`helper: Helper`, `repo: Repo = None`). Unannotated ones
/// carry no type, so they are left out.
fn parameters_of(node: Node<'_>, src: &str) -> Vec<(String, String)> {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        let (name, ty) = match parameter.kind() {
            "typed_parameter" => (
                parameter_name(parameter, src),
                parameter
                    .child_by_field_name("type")
                    .and_then(|ty| simple_type_name(ty, src)),
            ),
            "typed_default_parameter" => (
                parameter
                    .child_by_field_name("name")
                    .map(|name| text_of(name, src)),
                parameter
                    .child_by_field_name("type")
                    .and_then(|ty| simple_type_name(ty, src)),
            ),
            _ => continue,
        };
        if let (Some(name), Some(ty)) = (name, ty) {
            if !name.is_empty() {
                out.push((name, ty));
            }
        }
    }
    out
}

fn parameter_name(node: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let named = node
        .named_children(&mut cursor)
        .find(|child| child.kind() != "type")?;
    match named.kind() {
        "list_splat_pattern" | "dictionary_splat_pattern" => named
            .named_child(0)
            .map(|inner| text_of(inner, src))
            .filter(|name| !name.is_empty()),
        _ => {
            let name = text_of(named, src);
            (!name.is_empty()).then_some(name)
        }
    }
}

/// Simple name of a type: `Repo` for `Repo`, `pkg.Repo`, `Repo[T]`, `Repo | None`
/// and `"Repo"` (a forward reference). Only the outer type matters for receiver
/// resolution; the full annotation is still recorded in `uses`.
fn simple_type_name(node: Node<'_>, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => {
            let name = text_of(node, src);
            (!name.is_empty()).then_some(name)
        }
        "attribute" => node
            .child_by_field_name("attribute")
            .map(|attribute| text_of(attribute, src))
            .filter(|name| !name.is_empty()),
        "string" => {
            let name = text_of(node, src);
            let name = name.trim_matches(|character| matches!(character, '"' | '\'' | 'f' | 'r'));
            is_identifier(name).then(|| name.to_string())
        }
        // The grammar wraps every annotation in one `type` node.
        "type" => node
            .named_child(0)
            .and_then(|inner| simple_type_name(inner, src)),
        // `list[Foo]` -> `list`, `Foo | None` -> `Foo`, `Foo.Bar` -> `Bar`.
        "subscript"
        | "union_type"
        | "generic_type"
        | "constrained_type"
        | "member_type"
        | "parenthesized_expression" => node
            .child_by_field_name("value")
            .and_then(|value| simple_type_name(value, src))
            .or_else(|| {
                let mut cursor = node.walk();
                let found = node
                    .named_children(&mut cursor)
                    .find_map(|child| simple_type_name(child, src));
                found
            }),
        _ => None,
    }
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
}

/// `Repo()` constructs a `Repo`: enough to type `repo = Repo()` without an
/// annotation.
fn inferred_type(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() != "call" {
        return None;
    }
    let function = node.child_by_field_name("function")?;
    let name = match function.kind() {
        "identifier" => text_of(function, src),
        "attribute" => text_of(function.child_by_field_name("attribute")?, src),
        _ => return None,
    };
    let class_like = name.starts_with(|character: char| character.is_ascii_uppercase());
    class_like.then_some(name)
}

fn calls_in(node: Node<'_>, src: &str) -> Vec<Call> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "call" && constructed_type(current, src).is_none() {
            if let Some(function) = current.child_by_field_name("function") {
                if let Some(call) = call_of(function, src, line_of(src, current.start_byte())) {
                    out.push(call);
                }
            }
        }
        let mut cursor = current.walk();
        for child in current.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// `Repo()`, `pkg.Repo()` and `Repo(...)` construct a type: that is a type use,
/// not a call, exactly like Java's `new Repo()`. Python methods are snake_case,
/// so a capitalised callee is a class far more often than a function.
fn constructed_type(node: Node<'_>, src: &str) -> Option<String> {
    let function = node.child_by_field_name("function")?;
    let name = match function.kind() {
        "identifier" => text_of(function, src),
        "attribute" => text_of(function.child_by_field_name("attribute")?, src),
        _ => return None,
    };
    name.starts_with(|character: char| character.is_ascii_uppercase())
        .then_some(name)
}

fn call_of(function: Node<'_>, src: &str, line: usize) -> Option<Call> {
    match function.kind() {
        "identifier" => {
            let name = text_of(function, src);
            (!name.is_empty()).then_some(Call {
                name,
                receiver: Receiver::Bare,
                line,
            })
        }
        "attribute" => {
            let name = text_of(function.child_by_field_name("attribute")?, src);
            if name.is_empty() {
                return None;
            }
            let receiver = function
                .child_by_field_name("object")
                .map(|object| receiver_of(object, src))
                .unwrap_or(Receiver::Unknown);
            Some(Call {
                name,
                receiver,
                line,
            })
        }
        _ => None,
    }
}

fn receiver_of(node: Node<'_>, src: &str) -> Receiver {
    match node.kind() {
        "identifier" => match text_of(node, src).as_str() {
            // `cls.helper()` inside a classmethod is the enclosing class.
            "self" | "cls" => Receiver::This,
            name => Receiver::Name(name.to_string()),
        },
        // `self.repo.find()` -> receiver `repo`; `self.a.b()` and `a.b.c()` stay
        // unknown because we do not infer field chains.
        "attribute" => {
            let Some(object) = node.child_by_field_name("object") else {
                return Receiver::Unknown;
            };
            if object.kind() != "identifier" || text_of(object, src) != "self" {
                return Receiver::Unknown;
            }
            node.child_by_field_name("attribute")
                .map(|attribute| Receiver::Name(text_of(attribute, src)))
                .unwrap_or(Receiver::Unknown)
        }
        "call" => {
            let Some(function) = node.child_by_field_name("function") else {
                return Receiver::Unknown;
            };
            if function.kind() == "identifier" && text_of(function, src) == "super" {
                Receiver::Super
            } else {
                Receiver::Unknown
            }
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|inner| receiver_of(inner, src))
            .unwrap_or(Receiver::Unknown),
        _ => Receiver::Unknown,
    }
}

/// Collects the names in type annotations (`x: Foo`, `-> Foo`, `list[Foo]`,
/// `Foo | None`, base classes), so a class used only in an annotation still
/// pulls its module in.
fn collect_uses(root: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "typed_parameter" | "typed_default_parameter" | "assignment" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    collect_type_names(ty, src, out);
                }
            }
            "function_definition" => {
                if let Some(ty) = node.child_by_field_name("return_type") {
                    collect_type_names(ty, src, out);
                }
            }
            "class_definition" => {
                if let Some(bases) = node.child_by_field_name("superclasses") {
                    collect_type_names(bases, src, out);
                }
            }
            "call" => {
                if let Some(name) = constructed_type(node, src) {
                    out.push((name, node.start_byte()));
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn collect_type_names(node: Node<'_>, src: &str, out: &mut Vec<(String, usize)>) {
    match node.kind() {
        "identifier" => {
            let name = text_of(node, src);
            if !name.is_empty() {
                out.push((name, node.start_byte()));
            }
            return;
        }
        // `pkg.Repo` names `Repo`; the module qualifier is not a type.
        "attribute" => {
            if let Some(attribute) = node.child_by_field_name("attribute") {
                let name = text_of(attribute, src);
                if !name.is_empty() {
                    out.push((name, node.start_byte()));
                }
            }
            return;
        }
        // Forward references: `x: "Repo"`.
        "string" => {
            let name = text_of(node, src);
            let name = name.trim_matches(|character| matches!(character, '"' | '\'' | 'f' | 'r'));
            if is_identifier(name) {
                out.push((name.to_string(), node.start_byte()));
            }
            return;
        }
        // `metaclass=ABCMeta` is a keyword argument, its value is a type.
        "keyword_argument" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_type_names(value, src, out);
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_names(child, src, out);
    }
}

fn text_of(node: Node<'_>, src: &str) -> String {
    src.get(node.byte_range()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Source {
        PythonParser::new()
            .parse("app/service.py", src)
            .expect("parsed")
    }

    #[test]
    fn collects_imports_in_every_form() {
        let source = parse(
            r#"
import os
import app.models
import app.models.user as user
from app.services import Repo, Helper as H
from . import sibling
from ..base import Base
from app.helpers import *
from __future__ import annotations
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from app.types import Payload
"#,
        );
        let paths: Vec<&str> = source
            .imports
            .iter()
            .map(|import| import.path.as_str())
            .collect();
        for expected in [
            "os",
            "app.models",
            "app.models.user",
            "app.services.Repo",
            "app.services.Helper",
            ".sibling",
            "..base.Base",
            "app.helpers",
            "__future__.annotations",
            "typing.TYPE_CHECKING",
            "app.types.Payload",
        ] {
            assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
        }
        assert_eq!(
            source
                .imports
                .iter()
                .filter(|import| import.kind == ImportKind::Wildcard)
                .map(|import| import.path.as_str())
                .collect::<Vec<_>>(),
            vec!["app.helpers"]
        );
        assert_eq!(source.package.as_deref(), Some("app"));
    }

    #[test]
    fn collects_classes_methods_and_fields() {
        let source = parse(
            r#"
class Service(Base, pkg.Mixin):
    """Docs."""

    repo: Repo
    count: int = 0

    def __init__(self, repo: Repo) -> None:
        self.repo = repo

    @property
    def size(self) -> int:
        return self.repo.size()

    @staticmethod
    def other(helper: Helper) -> int:
        return helper.pong()
"#,
        );
        assert_eq!(source.types, vec!["Service"]);
        let service = &source.symbols[0];
        assert_eq!(service.supertypes, vec!["Base", "Mixin"]);
        assert_eq!(
            service.fields,
            vec![
                ("repo".to_string(), "Repo".to_string()),
                ("count".to_string(), "int".to_string()),
            ]
        );
        for name in ["__init__", "size", "other"] {
            let member = service
                .members
                .iter()
                .find(|member| member.name == name)
                .unwrap_or_else(|| panic!("missing {name}: {:?}", service.members));
            assert!(member.body_start.is_some(), "{name}");
        }
        let size = service
            .members
            .iter()
            .find(|member| member.name == "size")
            .expect("size");
        assert!(
            size.calls
                .iter()
                .any(|call| call.name == "size" && call.receiver == Receiver::Name("repo".into())),
            "{:?}",
            size.calls
        );
        let other = service
            .members
            .iter()
            .find(|member| member.name == "other")
            .expect("other");
        assert_eq!(
            other.params,
            vec![("helper".to_string(), "Helper".to_string())]
        );
        assert!(other
            .calls
            .iter()
            .any(|call| call.name == "pong" && call.receiver == Receiver::Name("helper".into())));
        // The decorator and the docstring are part of the class span.
        assert_eq!(service.line, 2);
        for expected in ["Base", "Mixin", "Repo"] {
            assert!(
                service.uses.iter().any(|name| name == expected),
                "missing {expected} in {:?}",
                service.uses
            );
        }
    }

    #[test]
    fn module_functions_record_calls_and_locals() {
        let source = parse(
            r#"
def run(helper: Helper) -> int:
    repo = Repo()
    return repo.size() + helper.pong() + helper.lookup().tick()
"#,
        );
        let run = &source.symbols[0];
        assert_eq!(run.kind, SymbolKind::Function);
        assert_eq!(run.name, "run");
        assert_eq!(
            run.params,
            vec![("helper".to_string(), "Helper".to_string())]
        );
        assert!(run
            .locals
            .iter()
            .any(|(name, ty)| name == "repo" && ty == "Repo"));
        assert!(run
            .calls
            .iter()
            .any(|call| call.name == "pong" && call.receiver == Receiver::Name("helper".into())));
        assert!(run
            .calls
            .iter()
            .any(|call| call.name == "size" && call.receiver == Receiver::Name("repo".into())));
        assert!(run
            .calls
            .iter()
            .any(|call| call.name == "lookup" && call.receiver == Receiver::Name("helper".into())));
        // A call chained onto another call has no inferable receiver.
        assert_eq!(
            run.calls
                .iter()
                .find(|call| call.name == "tick")
                .map(|call| call.receiver.clone()),
            Some(Receiver::Unknown)
        );
    }

    #[test]
    fn receiver_kinds() {
        let source = parse(
            r#"
class Base:
    def parent(self) -> None: ...

class Service(Base):
    def run(self, helper: Helper) -> None:
        self.inside()
        super().parent()
        helper.pong()
        Service.create()
        pkg.Repository.load()
"#,
        );
        let service = &source.symbols[1];
        let run = &service.members[0];
        for (name, receiver) in [
            ("inside", Receiver::This),
            ("parent", Receiver::Super),
            ("pong", Receiver::Name("helper".to_string())),
            ("create", Receiver::Name("Service".to_string())),
            ("load", Receiver::Unknown),
        ] {
            assert!(
                run.calls
                    .iter()
                    .any(|call| call.name == name && call.receiver == receiver),
                "missing {name} {receiver:?}: {:?}",
                run.calls
            );
        }
    }

    #[test]
    fn stubs_and_decorators_span_the_declaration() {
        let text = "@overload\ndef size(self) -> int: ...\n";
        let source = parse(text);
        let size = &source.symbols[0];
        assert_eq!(size.start, 0);
        assert_eq!(size.kind, SymbolKind::Function);
        assert!(size.calls.is_empty());
        assert!(size.params.is_empty(), "`self` has no annotation");

        // A decorator with arguments belongs to the declaration, and the call
        // inside it is not a call the body makes.
        let text = "@app.route(\"/x\", methods=[\"GET\"])\ndef handler(request: Request) -> Response:\n    return Response()\n";
        let source = parse(text);
        let handler = &source.symbols[0];
        assert_eq!(handler.start, 0);
        assert_eq!(
            handler.params,
            vec![("request".to_string(), "Request".to_string())]
        );
        assert!(handler.calls.is_empty(), "{:?}", handler.calls);

        let text =
            "class Service:\n    @cached(ttl=5)\n    def size(self) -> int:\n        return 1\n";
        let source = parse(text);
        let service = &source.symbols[0];
        assert_eq!(service.line, 1);
        assert_eq!(
            service.members[0].start,
            text.find("@cached").expect("decorator")
        );
        assert_eq!(service.members[0].line, 2);
    }

    #[test]
    fn docstring_and_imports_become_the_preamble() {
        let text = "\"\"\"Module docs.\"\"\"\n\nimport os\n\nclass Service:\n    def run(self) -> None:\n        pass\n";
        let source = parse(text);
        let service = &source.symbols[0];
        let preamble = &text[..source.preamble_end];
        assert!(preamble.contains("Module docs"));
        assert!(preamble.contains("import os"));
        assert_eq!(service.start, source.preamble_end);
    }

    #[test]
    fn package_comes_from_the_directory() {
        let mut parser = PythonParser::new();
        assert_eq!(
            parser
                .parse("app/model/user.py", "")
                .expect("parsed")
                .package,
            Some("app.model".to_string())
        );
        assert_eq!(parser.parse("user.py", "").expect("parsed").package, None);
    }
}
