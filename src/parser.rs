use std::fs;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use crate::entity::{Entity, EntityType, ImportInfo};

// Pre-compiled regexes for import parsing
static NORMALIZE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"import\s*(?:type\s+)?\{([^}]*)\}\s*from"#).unwrap());

static NAMED_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s*(?:type\s+)?\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]"#).unwrap()
});

static DEFAULT_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+(?:type\s+)?(\w+)\s+from\s*['"]([^'"]+)['"]"#).unwrap()
});

static NAMESPACE_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+(?:type\s+)?\*\s+as\s+(\w+)\s+from\s*['"]([^'"]+)['"]"#).unwrap()
});

// import Default, { Named } from '...'
static MIXED_DEFAULT_NAMED_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+(?:type\s+)?(\w+)\s*,\s*\{([^}]*)\}\s*from\s*['"]([^'"]+)['"]"#).unwrap()
});

// import Default, * as Namespace from '...'
static MIXED_DEFAULT_NAMESPACE_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+(?:type\s+)?(\w+)\s*,\s*\*\s+as\s+(\w+)\s+from\s*['"]([^'"]+)['"]"#)
        .unwrap()
});

static LAZY_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s*\(\s*['"]([^'"]+)['"]\s*\)\.then\s*\(\s*\w+\s*=>\s*\w+\.(\w+)\s*\)"#)
        .unwrap()
});

static DYNAMIC_DESTRUCTURED_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)(?:const|let|var)\s*\{([^}]+)\}\s*=\s*(?:await\s+)?import\s*\(\s*['"]([^'"]+)['"]\s*\)"#)
        .unwrap()
});

static WORKER_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"new\s+Worker\s*\(\s*new\s+URL\s*\(\s*['"]([^'"]+)['"]"#).unwrap()
});

pub(crate) struct FileParseResult {
    pub entities: Vec<Entity>,
    pub imports: Vec<ImportInfo>,
}

pub(crate) struct Parser<'a> {
    root_path: &'a Path,
}

impl<'a> Parser<'a> {
    pub fn new(root_path: &'a Path) -> Self {
        Parser { root_path }
    }

    pub fn parse(&self, file_path: &str) -> Result<FileParseResult> {
        let mut file = fs::File::open(file_path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;

        let mut entities = Vec::new();

        // Extract all imports from the file (shared by all entities in this file)
        let imports = self.extract_imports(&content, file_path);
        let deps = Rc::new(imports.clone());

        // If this is a worker file, treat the entire file as a single Worker entity
        if is_worker_file(file_path) {
            if let Some(worker_name) = worker_filename_to_entity_name(file_path) {
                entities.push(Entity::new(
                    worker_name,
                    EntityType::Worker,
                    file_path.to_string(),
                    Rc::clone(&deps),
                ));
                return Ok(FileParseResult { entities, imports });
            }
        }

        // Strip comments before parsing exports
        let content_without_comments = strip_comments(&content);

        // Collect lines for decorator detection (need to look backwards)
        let lines: Vec<&str> = content_without_comments.lines().collect();

        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            if trimmed.is_empty() {
                continue;
            }

            // Check for exported classes
            if trimmed.contains("export") && trimmed.contains("class") {
                if let Some(name) = extract_export_name(trimmed, "class") {
                    // Check for Angular decorators on preceding lines
                    let entity_type =
                        detect_angular_decorator(&lines, line_idx).unwrap_or(EntityType::Class);
                    entities.push(Entity::new(
                        name,
                        entity_type,
                        file_path.to_string(),
                        Rc::clone(&deps),
                    ));
                }
            }

            // Check for exported enums
            if trimmed.contains("export") && trimmed.contains("enum") {
                if let Some(name) = extract_export_name(trimmed, "enum") {
                    entities.push(Entity::new(
                        name,
                        EntityType::Enum,
                        file_path.to_string(),
                        Rc::clone(&deps),
                    ));
                }
            }

            // Check for exported types
            if trimmed.contains("export") && trimmed.contains("type") && !trimmed.contains("typeof")
            {
                if let Some(name) = extract_export_name(trimmed, "type") {
                    entities.push(Entity::new(
                        name,
                        EntityType::Type,
                        file_path.to_string(),
                        Rc::clone(&deps),
                    ));
                }
            }

            // Check for exported interfaces
            if trimmed.contains("export") && trimmed.contains("interface") {
                if let Some(name) = extract_export_name(trimmed, "interface") {
                    entities.push(Entity::new(
                        name,
                        EntityType::Interface,
                        file_path.to_string(),
                        Rc::clone(&deps),
                    ));
                }
            }

            // Check for exported functions
            if trimmed.contains("export") && trimmed.contains("function") {
                if let Some(name) = extract_export_name(trimmed, "function") {
                    entities.push(Entity::new(
                        name,
                        EntityType::Function,
                        file_path.to_string(),
                        Rc::clone(&deps),
                    ));
                }
            }

            // Check for export const/let/var function expressions
            if trimmed.starts_with("export const")
                || trimmed.starts_with("export let")
                || trimmed.starts_with("export var")
            {
                let keyword = if trimmed.starts_with("export const") {
                    "const"
                } else if trimmed.starts_with("export let") {
                    "let"
                } else {
                    "var"
                };

                if let Some(name) = extract_export_name(trimmed, keyword) {
                    if trimmed.contains("=>") || trimmed.contains("= function") {
                        entities.push(Entity::new(
                            name,
                            EntityType::Function,
                            file_path.to_string(),
                            Rc::clone(&deps),
                        ));
                    } else {
                        entities.push(Entity::new(
                            name,
                            EntityType::Const,
                            file_path.to_string(),
                            Rc::clone(&deps),
                        ));
                    }
                }
            }
        }

        // Check if exported entities are used locally in the same file
        for entity in &mut entities {
            if is_entity_used_locally(&content, &entity.name) {
                entity.used = true;
            }
        }

        Ok(FileParseResult { entities, imports })
    }

    pub fn extract_imports(&self, content: &str, file_path: &str) -> Vec<ImportInfo> {
        let mut imports = Vec::new();

        // Strip comments first to avoid parsing commented imports
        let content_without_comments = strip_comments(content);

        // Normalize content: collapse multiline imports into single lines
        let normalized_content =
            NORMALIZE_RE.replace_all(&content_without_comments, |caps: &regex::Captures| {
                let names = caps[1].replace('\n', " ").replace('\r', " ");
                format!("import {{{}}} from", names)
            });

        for cap in NAMED_IMPORT_RE.captures_iter(&normalized_content) {
            let names_str = &cap[1];
            let import_path = cap[2].to_string();

            let resolved_path = match resolve_import_path(file_path, &import_path, self.root_path) {
                Some(path) => path,
                None => continue,
            };

            for name_part in names_str.split(',') {
                let name_part = name_part.trim();
                if name_part.is_empty() {
                    continue;
                }

                let Some(name) = normalize_named_import(name_part) else {
                    continue;
                };

                imports.push(ImportInfo::new(name, resolved_path.clone()));
            }
        }

        for cap in DEFAULT_IMPORT_RE.captures_iter(&normalized_content) {
            let name = cap[1].to_string();
            let import_path = cap[2].to_string();

            if name == "from" {
                continue;
            }

            if let Some(resolved_path) =
                resolve_import_path(file_path, &import_path, self.root_path)
            {
                imports.push(ImportInfo::new(name, resolved_path));
            }
        }

        for cap in NAMESPACE_IMPORT_RE.captures_iter(&normalized_content) {
            let name = cap[1].to_string();
            let import_path = cap[2].to_string();

            if let Some(resolved_path) =
                resolve_import_path(file_path, &import_path, self.root_path)
            {
                imports.push(ImportInfo::new(name, resolved_path));
            }
        }

        // Handle mixed imports: the default clause and the named/namespace clause of the
        // same declaration are both bindings, not alternatives.
        // import Default, { Named } from '...'
        for cap in MIXED_DEFAULT_NAMED_IMPORT_RE.captures_iter(&normalized_content) {
            let default_name = cap[1].to_string();
            let names_str = cap[2].to_string();
            let import_path = cap[3].to_string();

            let resolved_path = match resolve_import_path(file_path, &import_path, self.root_path) {
                Some(path) => path,
                None => continue,
            };

            imports.push(ImportInfo::new(default_name, resolved_path.clone()));

            for name_part in names_str.split(',') {
                let name_part = name_part.trim();
                if name_part.is_empty() {
                    continue;
                }

                let Some(name) = normalize_named_import(name_part) else {
                    continue;
                };

                imports.push(ImportInfo::new(name, resolved_path.clone()));
            }
        }

        // import Default, * as Namespace from '...'
        for cap in MIXED_DEFAULT_NAMESPACE_IMPORT_RE.captures_iter(&normalized_content) {
            let default_name = cap[1].to_string();
            let namespace_name = cap[2].to_string();
            let import_path = cap[3].to_string();

            if let Some(resolved_path) =
                resolve_import_path(file_path, &import_path, self.root_path)
            {
                imports.push(ImportInfo::new(default_name, resolved_path.clone()));
                imports.push(ImportInfo::new(namespace_name, resolved_path));
            }
        }

        // Handle Angular lazy-loaded imports
        for cap in LAZY_IMPORT_RE.captures_iter(&normalized_content) {
            let import_path = cap[1].to_string();
            let name = cap[2].to_string();

            if let Some(resolved_path) =
                resolve_import_path(file_path, &import_path, self.root_path)
            {
                imports.push(ImportInfo::new(name, resolved_path));
            }
        }

        // Handle dynamic destructured imports:
        // const { Foo } = await import('./foo')
        // const { Foo: LocalFoo } = await import('./foo')
        for cap in DYNAMIC_DESTRUCTURED_IMPORT_RE.captures_iter(&content_without_comments) {
            let names_str = &cap[1];
            let import_path = cap[2].to_string();

            let resolved_path = match resolve_import_path(file_path, &import_path, self.root_path) {
                Some(path) => path,
                None => continue,
            };

            for name_part in names_str.split(',') {
                let name_part = name_part.trim();
                if name_part.is_empty() || name_part.starts_with("...") {
                    continue;
                }

                let Some(name) = normalize_destructured_import(name_part) else {
                    continue;
                };

                imports.push(ImportInfo::new(name, resolved_path.clone()));
            }
        }

        // Handle Worker imports: new Worker(new URL('...'))
        for cap in WORKER_IMPORT_RE.captures_iter(&normalized_content) {
            let import_path = cap[1].to_string();

            // Worker paths may or may not have extensions, try .worker.ts first
            let worker_path = if import_path.ends_with(".worker") {
                format!("{}.ts", import_path)
            } else if !import_path.ends_with(".ts") {
                format!("{}.worker.ts", import_path)
            } else {
                import_path.clone()
            };

            if let Some(resolved_path) =
                resolve_import_path(file_path, &worker_path, self.root_path)
            {
                if let Some(worker_name) = worker_filename_to_entity_name(&resolved_path) {
                    imports.push(ImportInfo::new(worker_name, resolved_path));
                }
            }
        }

        imports
    }
}

fn normalize_destructured_import(name_part: &str) -> Option<String> {
    let without_default = name_part.split('=').next().unwrap_or(name_part).trim();
    let imported_name = without_default
        .split(':')
        .next()
        .unwrap_or(without_default)
        .trim();

    if imported_name.is_empty() {
        None
    } else {
        Some(imported_name.to_string())
    }
}

fn normalize_named_import(name_part: &str) -> Option<String> {
    let without_inline_type = name_part.strip_prefix("type ").unwrap_or(name_part).trim();
    if without_inline_type.is_empty() {
        return None;
    }

    let name = if let Some(pos) = without_inline_type.find(" as ") {
        without_inline_type[..pos].trim()
    } else {
        without_inline_type
    };

    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Strips single-line (//) and multi-line (/* */) comments from content.
/// Preserves strings so that comment-like patterns inside strings are not stripped.
pub(crate) fn strip_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_string: Option<char> = None;

    while let Some(c) = chars.next() {
        if in_string.is_none() && (c == '"' || c == '\'' || c == '`') {
            in_string = Some(c);
            result.push(c);
            continue;
        }

        if let Some(quote) = in_string {
            result.push(c);
            if c == '\\' {
                if let Some(&next) = chars.peek() {
                    result.push(next);
                    chars.next();
                }
            } else if c == quote {
                in_string = None;
            }
            continue;
        }

        if c == '/' {
            if let Some(&next) = chars.peek() {
                if next == '/' {
                    chars.next();
                    while let Some(&ch) = chars.peek() {
                        if ch == '\n' {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                } else if next == '*' {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '*' {
                            if let Some(&peek) = chars.peek() {
                                if peek == '/' {
                                    chars.next();
                                    break;
                                }
                            }
                        }
                    }
                    continue;
                }
            }
        }

        result.push(c);
    }

    result
}

fn extract_export_name(line: &str, keyword: &str) -> Option<String> {
    let mut search_start = 0;

    while let Some(relative_pos) = line[search_start..].find(keyword) {
        let pos = search_start + relative_pos;

        // Check that keyword is not part of another word:
        // - preceded by non-alphanumeric (or start of string)
        // - followed by whitespace
        let char_before_ok = pos == 0 || {
            let prev_char = line[..pos].chars().last().unwrap();
            !prev_char.is_alphanumeric() && prev_char != '_'
        };

        let after_keyword = &line[pos + keyword.len()..];
        let char_after_ok = after_keyword.starts_with(|c: char| c.is_whitespace());

        if char_before_ok && char_after_ok {
            let identifier: String = after_keyword
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();

            if !identifier.is_empty() {
                return Some(identifier);
            }
        }

        // Continue searching after this position
        search_start = pos + keyword.len();
    }

    None
}

fn resolve_import_path(
    importing_file: &str,
    import_source: &str,
    root_path: &Path,
) -> Option<String> {
    let base_path = if import_source == "@ui-annotator" {
        root_path.join("libs/ui-annotator/src/lib/create-ui-annotator")
    } else if import_source.starts_with("@awork/") {
        let rest = &import_source[7..];
        root_path.join("libs/shared/src/lib").join(rest)
    } else if import_source.starts_with("./") || import_source.starts_with("../") {
        let importing_dir = Path::new(importing_file).parent()?;
        importing_dir.join(import_source)
    } else {
        return None;
    };

    let extensions = [".ts", ".tsx", "/index.ts", "/index.tsx"];

    for ext in &extensions {
        let full_path = if ext.starts_with('/') {
            base_path.join(&ext[1..])
        } else {
            let path_str = base_path.to_string_lossy();
            Path::new(&format!("{}{}", path_str, ext)).to_path_buf()
        };

        if full_path.exists() {
            return full_path
                .canonicalize()
                .ok()?
                .to_str()
                .map(|s| s.to_string());
        }
    }

    if base_path.exists() && base_path.is_file() {
        return base_path
            .canonicalize()
            .ok()?
            .to_str()
            .map(|s| s.to_string());
    }

    let path_str = base_path.to_string_lossy().to_string();
    if path_str.ends_with(".ts") || path_str.ends_with(".tsx") {
        Some(path_str)
    } else {
        Some(format!("{}.ts", path_str))
    }
}

fn is_entity_used_locally(content: &str, entity_name: &str) -> bool {
    let pattern = format!(r"\b{}\b", regex::escape(entity_name));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let matches: Vec<_> = re.find_iter(content).collect();
    matches.len() > 1
}

/// Checks if a file path is a worker file (ends with .worker.ts)
fn is_worker_file(file_path: &str) -> bool {
    file_path.ends_with(".worker.ts")
}

/// Detects Angular decorators on lines preceding a class declaration.
/// Returns the appropriate EntityType if a decorator is found.
fn detect_angular_decorator(lines: &[&str], class_line_idx: usize) -> Option<EntityType> {
    // Look backwards from the class line for decorator (max 30 lines)
    let start_idx = class_line_idx.saturating_sub(30);

    for i in (start_idx..class_line_idx).rev() {
        let line = lines[i].trim();

        // Skip empty lines
        if line.is_empty() {
            continue;
        }

        // Check for Angular decorators
        if line.starts_with("@Component") {
            return Some(EntityType::Component);
        }
        if line.starts_with("@Injectable") {
            return Some(EntityType::Service);
        }
        if line.starts_with("@Directive") {
            return Some(EntityType::Directive);
        }
        if line.starts_with("@Pipe") {
            return Some(EntityType::Pipe);
        }

        // Stop if we hit an import/export statement (we've gone too far)
        if line.starts_with("import ") || line.starts_with("export ") {
            break;
        }

        // Stop if we hit a class/function/interface declaration
        if line.contains("class ") || line.contains("function ") || line.contains("interface ") {
            break;
        }

        // Otherwise continue looking (could be decorator params, other decorators, etc.)
    }

    None
}

/// Converts a worker filename to PascalCase + "Worker" suffix.
/// Examples:
///   - "planner-overview.worker.ts" -> "PlannerOverviewWorker"
///   - "my_worker.worker.ts" -> "MyWorkerWorker"
///   - "simple.worker.ts" -> "SimpleWorker"
pub(crate) fn worker_filename_to_entity_name(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let file_name = path.file_name()?.to_str()?;

    // Strip the .worker.ts suffix
    let base_name = file_name.strip_suffix(".worker.ts")?;

    // Convert kebab-case and snake_case to PascalCase
    let pascal_case: String = base_name
        .split(|c| c == '-' || c == '_')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect();

    Some(format!("{}Worker", pascal_case))
}
